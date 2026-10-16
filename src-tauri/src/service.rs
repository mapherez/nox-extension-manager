use crate::automation::{registration, BrowserAutomationBackend};
use crate::model::*;
use crate::repository::{
    build_snapshot, compare_versions, copy_directory, hash_directory, RepositoryClient,
};
use crate::storage::StoragePaths;
use chrono::Utc;
use std::cmp::Ordering;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub struct ManagerService {
    paths: StoragePaths,
    repository: RepositoryClient,
    automation: Arc<dyn BrowserAutomationBackend>,
}

impl ManagerService {
    pub fn new(automation: Arc<dyn BrowserAutomationBackend>) -> Result<Self, String> {
        let config = serde_json::from_str(include_str!("../resources/repository.json"))
            .map_err(|e| format!("Invalid repository configuration: {e}"))?;
        let paths = StoragePaths::discover()?;
        paths.ensure()?;
        Ok(Self {
            paths,
            repository: RepositoryClient::new(config)?,
            automation,
        })
    }

    fn profile(&self) -> Result<(Vec<ChromeProfile>, ChromeProfile), String> {
        let profiles = self.automation.profiles()?;
        let selected = self
            .paths
            .preferences()
            .unwrap_or_default()
            .profile
            .unwrap_or_else(|| "Default".into());
        let profile = profiles
            .iter()
            .find(|p| p.id == selected)
            .ok_or_else(|| {
                format!("Chrome profile {selected} is unavailable; select another profile")
            })?
            .clone();
        Ok((profiles, profile))
    }

    pub fn select_profile(&self, id: &str) -> Result<ManagerSnapshot, String> {
        if !self.automation.profiles()?.iter().any(|p| p.id == id) {
            return Err("Unknown Chrome profile".into());
        }
        self.paths.save_preferences(&ManagerPreferences {
            profile: Some(id.into()),
        })?;
        self.snapshot()
    }

    pub fn snapshot(&self) -> Result<ManagerSnapshot, String> {
        let state = self.paths.load_repository_state()?;
        let mut snapshot = build_snapshot(
            &self.paths,
            &state,
            &ChromeState::default(),
            self.repository.repository_url(),
        );
        let inventory = match self.profile() {
            Ok((profiles, profile)) => {
                snapshot.profiles = profiles;
                snapshot.selected_profile = profile.id.clone();
                self.automation.inventory(&profile)
            }
            Err(error) => {
                snapshot.profiles = self.automation.profiles().unwrap_or_default();
                snapshot.selected_profile = self
                    .paths
                    .preferences()
                    .unwrap_or_default()
                    .profile
                    .unwrap_or_else(|| "Default".into());
                Err(error)
            }
        };
        let installations = match inventory {
            Ok(items) => items,
            Err(error) => {
                snapshot.detection_error = Some(error);
                Vec::new()
            }
        };
        for view in &mut snapshot.extensions {
            let entry = &state.extensions[&view.id];
            view.installations = installations
                .iter()
                .filter(|item| item.name == entry.name)
                .cloned()
                .collect();
            let installed = (view.installations.len() == 1).then(|| &view.installations[0]);
            view.installed_version = installed.map(|i| i.version.clone());
            view.local_version = None;
            view.downloaded_version = None;
            view.download_state = "notDownloaded".into();
            // Validate saved payloads before exposing an Install action.
            if let Some(version) = &entry.pending_version {
                let pending = self.paths.pending_dir(&entry.id, version);
                if compare_versions(version, &entry.remote_version)
                    .is_ok_and(|o| o == Ordering::Equal)
                    && !entry.pending_hashes.is_empty()
                    && hash_directory(&pending).is_ok_and(|h| h == entry.pending_hashes)
                {
                    view.downloaded_version = Some(version.clone());
                    view.download_state = "downloaded".into();
                }
            }
            view.overwrite_path = installed.map(|i| i.path.clone()).or_else(|| {
                let path = self.paths.extension_dir(&entry.id);
                path.exists().then(|| path.to_string_lossy().to_string())
            });
            view.requires_overwrite_confirmation =
                view.overwrite_path.as_ref().is_some_and(|path| {
                    entry.trusted_path.as_deref().is_none_or(|trusted| {
                        !registration::same_path(Path::new(trusted), Path::new(path))
                    }) || entry.active_hashes.is_empty()
                });
            view.status = if snapshot.detection_error.is_some() {
                ExtensionStatus::DetectionUnavailable
            } else if entry.problem.is_some() {
                ExtensionStatus::Error
            } else if !entry.available {
                ExtensionStatus::Unavailable
            } else if view.installations.len() > 1 {
                ExtensionStatus::Ambiguous
            } else if let Some(installed) = installed {
                if compare_versions(&entry.remote_version, &installed.version)? == Ordering::Greater
                {
                    ExtensionStatus::UpdateReady
                } else {
                    ExtensionStatus::Installed
                }
            } else {
                ExtensionStatus::NotInstalled
            };
            view.icon_path = installed.and_then(|i| {
                let manifest: ChromeManifest = serde_json::from_slice(
                    &fs::read(Path::new(&i.path).join("manifest.json")).ok()?,
                )
                .ok()?;
                let relative = manifest.preferred_icon()?;
                if Path::new(&relative)
                    .components()
                    .any(|c| !matches!(c, std::path::Component::Normal(_)))
                {
                    return None;
                }
                let path = Path::new(&i.path).join(relative);
                path.is_file().then(|| path.to_string_lossy().to_string())
            });
        }
        Ok(snapshot)
    }

    pub async fn sync(&mut self) -> Result<ManagerSnapshot, String> {
        let mut state = self.paths.load_repository_state()?;
        self.repository
            .sync(&self.paths, &mut state, &ChromeState::default())
            .await?;
        self.snapshot()
    }

    pub async fn download(&mut self, id: &str) -> Result<ExtensionActionResult, String> {
        let mut state = self.paths.load_repository_state()?;
        self.repository
            .download(&self.paths, &mut state, id)
            .await?;
        self.result("Download completed. Choose Install to apply it.")
    }

    fn resolve(
        &self,
        id: &str,
        target: Option<&ActionTarget>,
    ) -> Result<(ChromeProfile, ManagedExtension, Option<ChromeInstallation>), String> {
        let (_, profile) = self.profile()?;
        let state = self.paths.load_repository_state()?;
        let entry = state
            .extensions
            .get(id)
            .cloned()
            .ok_or("Unknown catalog extension; Sync first")?;
        let items = self.automation.inventory(&profile)?;
        let matches: Vec<_> = items.into_iter().filter(|i| i.name == entry.name).collect();
        if let Some(target) = target {
            if target.profile_id != profile.id {
                return Err("The selected profile changed; choose the installation again".into());
            }
            let item = matches
                .into_iter()
                .find(|i| {
                    i.chrome_extension_id == target.chrome_extension_id
                        && registration::same_path(Path::new(&i.path), Path::new(&target.path))
                        && i.version == target.version
                })
                .ok_or("The Chrome installation changed; Sync and choose it again")?;
            if !item.unpacked {
                return Err("This installation is managed by Chrome, not an unpacked folder; use Chrome to change it".into());
            }
            return Ok((profile, entry, Some(item)));
        }
        if !matches.is_empty() {
            return Err("Select the exact Chrome installation before continuing".into());
        }
        Ok((profile, entry, None))
    }

    pub fn prepare_installation(
        &self,
        id: &str,
        target: Option<&ActionTarget>,
    ) -> Result<Option<OverwriteConsent>, String> {
        let (profile, entry, installed) = self.resolve(id, target)?;
        let active = installed
            .as_ref()
            .map(|i| PathBuf::from(&i.path))
            .unwrap_or_else(|| self.paths.extension_dir(id));
        if !active.exists() {
            return Ok(None);
        }
        let hashes = hash_directory(&active)?;
        let trusted = entry
            .trusted_path
            .as_ref()
            .is_some_and(|p| registration::same_path(Path::new(p), &active));
        if trusted && !entry.active_hashes.is_empty() && entry.active_hashes == hashes {
            return Ok(None);
        }
        Ok(Some(OverwriteConsent {
            profile_id: profile.id,
            path: active.to_string_lossy().to_string(),
            hashes,
        }))
    }

    pub async fn apply(
        &mut self,
        id: &str,
        target: Option<ActionTarget>,
        consent: Option<OverwriteConsent>,
    ) -> Result<ExtensionActionResult, String> {
        let (profile, entry, installed) = self.resolve(id, target.as_ref())?;
        if !entry.available || entry.problem.is_some() {
            return Err("The catalog entry is unavailable; Sync first".into());
        }
        let version = entry
            .pending_version
            .as_deref()
            .ok_or("Download the extension first")?;
        if compare_versions(version, &entry.remote_version)? != Ordering::Equal {
            return Err("The downloaded version is stale; download the available version".into());
        }
        if let Some(item) = &installed {
            if compare_versions(version, &item.version)? != Ordering::Greater {
                return Err("This installation is already current; downgrade blocked".into());
            }
        }
        let pending = self.paths.pending_dir(id, version);
        if entry.pending_hashes.is_empty() || hash_directory(&pending)? != entry.pending_hashes {
            return Err("Downloaded files are missing or have changed; download again".into());
        }
        let downloaded: ChromeManifest = serde_json::from_slice(
            &fs::read(pending.join("manifest.json")).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        if downloaded.name != entry.name || downloaded.version != version {
            return Err("Downloaded manifest does not match the catalog".into());
        }
        let active = installed
            .as_ref()
            .map(|i| PathBuf::from(&i.path))
            .unwrap_or_else(|| self.paths.extension_dir(id));
        if installed.is_some() {
            let current: serde_json::Value = serde_json::from_slice(
                &fs::read(active.join("manifest.json")).map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())?;
            let next: serde_json::Value = serde_json::from_slice(
                &fs::read(pending.join("manifest.json")).map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())?;
            if current.get("key") != next.get("key") {
                return Err("The manifest key changed; updating would change the Chrome ID. Installation blocked to preserve extension data".into());
            }
        }
        let before = if active.exists() {
            Some(hash_directory(&active)?)
        } else {
            None
        };
        if let Some(required) = self.prepare_installation(id, target.as_ref())? {
            let approved = consent.as_ref().is_some_and(|c| {
                c.profile_id == required.profile_id
                    && registration::same_path(Path::new(&c.path), Path::new(&required.path))
                    && c.hashes == required.hashes
            });
            if !approved {
                return Err(format!(
                    "Confirm replacement of {} with a backup before installing",
                    active.display()
                ));
            }
        }
        let automation = self.automation.for_profile(&profile)?;
        let checker = Arc::clone(&automation);
        let path = active.clone();
        let expected = installed.as_ref().map(|i| i.chrome_extension_id.clone());
        tauri::async_runtime::spawn_blocking(move || {
            checker.preflight()?;
            if let Some(id) = expected {
                checker.verify_identity(&path, &id)?;
            }
            Ok::<_, String>(())
        })
        .await
        .map_err(|e| e.to_string())??;
        // Keep copies and backups beside the active directory so promotion and
        // rollback work even when the installation is on another volume.
        let parent = active
            .parent()
            .ok_or("The installation cannot be a filesystem root")?;
        let leaf = active
            .file_name()
            .ok_or("Invalid installation directory")?
            .to_string_lossy();
        let stamp = Utc::now().timestamp_nanos_opt().unwrap_or_default();
        let prepared = parent.join(format!(".nox-prepared-{leaf}-{stamp}"));
        let backup = parent.join(format!(".nox-backup-{leaf}-{stamp}"));
        fs::create_dir_all(&prepared).map_err(|e| e.to_string())?;
        copy_directory(&pending, &prepared)?;
        if hash_directory(&prepared)? != entry.pending_hashes {
            return Err("Prepared files changed; installation blocked".into());
        }
        // Re-read registration and files immediately before changing anything.
        self.resolve(id, target.as_ref())?;
        if before.as_ref().is_some_and(|h| {
            hash_directory(&active)
                .as_ref()
                .map_or(true, |current| current != h)
        }) || (before.is_none() && active.exists())
        {
            return Err("Installed files changed during confirmation; retry".into());
        }
        if active.exists() {
            fs::rename(&active, &backup).map_err(|e| format!("Could not create backup: {e}"))?;
        }
        if let Err(error) = fs::rename(&prepared, &active) {
            if backup.exists() {
                fs::rename(&backup, &active).map_err(|e| {
                    format!(
                        "Activation failed: {error}; rollback failed: {e}; backup: {}",
                        backup.display()
                    )
                })?;
            }
            return Err(format!("Activation failed: {error}"));
        }
        let backend = Arc::clone(&automation);
        let action_path = active.clone();
        let action_version = version.to_string();
        let expected = installed.as_ref().map(|i| i.chrome_extension_id.clone());
        let result = tauri::async_runtime::spawn_blocking(move || {
            let id = if let Some(id) = expected {
                backend.reload(&action_path, &id, &action_version)?;
                id
            } else {
                backend.install(&action_path, &action_version)?
            };
            backend.verify_identity(&action_path, &id)?;
            Ok::<_, String>(id)
        })
        .await
        .unwrap_or_else(|e| Err(e.to_string()));
        if let Err(error) = result {
            if !backup.exists() {
                return Err(format!("Chrome did not confirm installation: {error}. Files were kept at {}; Sync to refresh the actual registration", active.display()));
            }
            let failed = parent.join(format!(".nox-failed-{leaf}-{stamp}"));
            fs::rename(&active, &failed).map_err(|e| {
                format!(
                    "Chrome failed: {error}; could not move failed files: {e}; backup: {}",
                    backup.display()
                )
            })?;
            if backup.exists() {
                fs::rename(&backup, &active).map_err(|e| {
                    format!(
                        "Chrome failed: {error}; rollback failed: {e}; backup: {}",
                        backup.display()
                    )
                })?;
                if let Some(previous) = installed {
                    let backend = Arc::clone(&automation);
                    let path = active.clone();
                    let reload = tauri::async_runtime::spawn_blocking(move || {
                        backend.reload(&path, &previous.chrome_extension_id, &previous.version)
                    })
                    .await;
                    if !matches!(reload, Ok(Ok(()))) {
                        return Err(format!("Files restored, but Chrome did not confirm the previous version: {error}"));
                    }
                }
            }
            return Err(format!(
                "Chrome did not confirm installation; previous files were restored: {error}"
            ));
        }
        let mut state = self.paths.load_repository_state()?;
        let saved = state
            .extensions
            .get_mut(id)
            .ok_or("Catalog entry disappeared")?;
        saved.active_hashes = entry.pending_hashes.clone();
        saved.trusted_path = Some(active.to_string_lossy().to_string());
        saved.active_version = Some(version.into());
        self.paths.save_repository_state(&state)?;
        self.paths.log(&format!(
            "Installed {id} in {}. Backup: {}",
            profile.id,
            backup.display()
        ));
        self.result("Extension installed successfully; previous files were kept as a backup")
    }

    pub async fn remove_selected(
        &mut self,
        id: &str,
        target: ActionTarget,
    ) -> Result<ExtensionActionResult, String> {
        let (profile, _, installed) = self.resolve(id, Some(&target))?;
        let installed = installed.ok_or("Select an installed extension")?;
        let before = self.automation.inventory(&profile)?;
        let backend = self.automation.for_profile(&profile)?;
        let path = PathBuf::from(&installed.path);
        let chrome_id = installed.chrome_extension_id.clone();
        tauri::async_runtime::spawn_blocking(move || {
            backend.verify_identity(&path, &chrome_id)?;
            backend.remove(&path, &chrome_id)
        })
        .await
        .map_err(|e| e.to_string())??;
        let after = self.automation.inventory(&profile)?;
        if after
            .iter()
            .any(|i| i.chrome_extension_id == installed.chrome_extension_id)
        {
            return Err("Chrome still reports the selected extension".into());
        }
        if before.iter().any(|i| {
            i.chrome_extension_id != installed.chrome_extension_id
                && !after
                    .iter()
                    .any(|j| j.chrome_extension_id == i.chrome_extension_id)
        }) {
            return Err("Another Chrome registration changed during removal; no further action was attempted".into());
        }
        self.result("Selected extension removed from Chrome; local files were kept")
    }

    fn result(&self, message: &str) -> Result<ExtensionActionResult, String> {
        Ok(ExtensionActionResult {
            message: message.into(),
            snapshot: self.snapshot()?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

    const CATALOG_ID: &str = "SampleExtension";
    const OWNED: &str = "abcdefghijklmnopabcdefghijklmnop";
    const OTHER: &str = "ponmlkjihgfedcbaponmlkjihgfedcba";

    #[derive(Clone)]
    struct FixtureBrowser {
        profiles: Vec<ChromeProfile>,
        mutations: Arc<AtomicUsize>,
        fail_version: Option<String>,
    }

    impl BrowserAutomationBackend for FixtureBrowser {
        fn profiles(&self) -> Result<Vec<ChromeProfile>, String> {
            Ok(self.profiles.clone())
        }
        fn for_profile(
            &self,
            profile: &ChromeProfile,
        ) -> Result<Arc<dyn BrowserAutomationBackend>, String> {
            if !self
                .profiles
                .iter()
                .any(|p| p.id == profile.id && p.path == profile.path)
            {
                return Err("Unknown fixture profile".into());
            }
            Ok(Arc::new(Self {
                profiles: vec![profile.clone()],
                ..self.clone()
            }))
        }
        fn preflight(&self) -> Result<(), String> {
            Ok(())
        }
        fn registered_id(&self, path: &Path) -> Result<Option<String>, String> {
            registration::registered_id(Path::new(&self.profiles[0].path), path)
        }
        fn install(&self, _: &Path, _: &str) -> Result<String, String> {
            Err("Unexpected new-install automation in this fixture".into())
        }
        fn verify_identity(&self, path: &Path, id: &str) -> Result<(), String> {
            registration::verify(Path::new(&self.profiles[0].path), path, id)
        }
        fn reload(&self, path: &Path, id: &str, version: &str) -> Result<(), String> {
            self.verify_identity(path, id)?;
            self.mutations.fetch_add(1, AtomicOrdering::SeqCst);
            if self.fail_version.as_deref() == Some(version) {
                return Err("Chrome rejected the update".into());
            }
            let manifest: ChromeManifest = serde_json::from_slice(
                &fs::read(path.join("manifest.json")).map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())?;
            if manifest.version != version {
                return Err("Unexpected reloaded version".into());
            }
            Ok(())
        }
        fn remove(&self, path: &Path, id: &str) -> Result<(), String> {
            self.verify_identity(path, id)?;
            let preferences = Path::new(&self.profiles[0].path).join("Secure Preferences");
            let mut value: serde_json::Value =
                serde_json::from_slice(&fs::read(&preferences).map_err(|e| e.to_string())?)
                    .map_err(|e| e.to_string())?;
            value
                .pointer_mut("/extensions/settings")
                .and_then(serde_json::Value::as_object_mut)
                .ok_or("Missing fixture registrations")?
                .remove(id);
            fs::write(
                preferences,
                serde_json::to_vec(&value).map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())?;
            self.mutations.fetch_add(1, AtomicOrdering::SeqCst);
            Ok(())
        }
    }

    struct Fixture {
        _temp: tempfile::TempDir,
        manager: ManagerService,
        profiles: Vec<ChromeProfile>,
        active: PathBuf,
        unrelated: PathBuf,
        mutations: Arc<AtomicUsize>,
    }

    fn write_extension(path: &Path, name: &str, version: &str) {
        fs::create_dir_all(path).unwrap();
        fs::write(
            path.join("manifest.json"),
            json!({
                "manifest_version": 3, "name": name, "version": version,
            })
            .to_string(),
        )
        .unwrap();
        fs::write(path.join("content.js"), format!("// {name} {version}")).unwrap();
    }

    fn fixture(fail_version: Option<&str>) -> Fixture {
        let temp = tempfile::tempdir().unwrap();
        let paths = StoragePaths::from_root(temp.path().join("managed"));
        paths.ensure().unwrap();
        let active = temp.path().join("external-installations").join("active");
        let unrelated = temp.path().join("external-installations").join("unrelated");
        write_extension(&active, "Sample Extension", "1.0.0");
        write_extension(&unrelated, "Unrelated Extension", "7.0.0");
        let profiles: Vec<_> = ["Default", "Profile 1"]
            .into_iter()
            .map(|id| {
                let path = temp.path().join("chrome").join(id);
                fs::create_dir_all(&path).unwrap();
                fs::write(
                    path.join("Secure Preferences"),
                    json!({"extensions": {"settings": {}}}).to_string(),
                )
                .unwrap();
                ChromeProfile {
                    id: id.into(),
                    name: id.into(),
                    path: path.to_string_lossy().into(),
                }
            })
            .collect();
        fs::write(
            Path::new(&profiles[0].path).join("Secure Preferences"),
            json!({"extensions": {"settings": {
                OWNED: {"path": active, "location": 4},
                OTHER: {"path": unrelated, "location": 4},
            }}})
            .to_string(),
        )
        .unwrap();
        let entry = ManagedExtension {
            id: CATALOG_ID.into(),
            name: "Sample Extension".into(),
            remote_version: "1.1.0".into(),
            available: true,
            ..ManagedExtension::default()
        };
        paths
            .save_repository_state(&RepositoryState {
                extensions: std::collections::BTreeMap::from([(entry.id.clone(), entry)]),
                ..RepositoryState::default()
            })
            .unwrap();
        let repository = RepositoryClient::new(RepositoryConfig {
            repository: "owner/repo".into(),
            branch: "master".into(),
            extensions_path: "extensions".into(),
            repository_url: "https://github.com/owner/repo".into(),
        })
        .unwrap();
        let mutations = Arc::new(AtomicUsize::new(0));
        let manager = ManagerService {
            paths,
            repository,
            automation: Arc::new(FixtureBrowser {
                profiles: profiles.clone(),
                mutations: mutations.clone(),
                fail_version: fail_version.map(str::to_owned),
            }),
        };
        Fixture {
            _temp: temp,
            manager,
            profiles,
            active,
            unrelated,
            mutations,
        }
    }

    fn target(fixture: &Fixture) -> ActionTarget {
        ActionTarget {
            profile_id: "Default".into(),
            chrome_extension_id: OWNED.into(),
            path: fixture.active.to_string_lossy().into(),
            version: "1.0.0".into(),
        }
    }

    fn stage_update(fixture: &Fixture) {
        let pending = fixture.manager.paths.pending_dir(CATALOG_ID, "1.1.0");
        write_extension(&pending, "Sample Extension", "1.1.0");
        let mut state = fixture.manager.paths.load_repository_state().unwrap();
        let entry = state.extensions.get_mut(CATALOG_ID).unwrap();
        entry.pending_version = Some("1.1.0".into());
        entry.pending_hashes = hash_directory(&pending).unwrap();
        fixture.manager.paths.save_repository_state(&state).unwrap();
    }

    #[test]
    fn snapshot_recovers_installation_from_chrome_without_manager_install_records() {
        let fixture = fixture(None);
        fs::remove_file(&fixture.manager.paths.chrome_state).unwrap();
        let snapshot = fixture.manager.snapshot().unwrap();
        let extension = &snapshot.extensions[0];
        assert!(matches!(extension.status, ExtensionStatus::UpdateReady));
        assert_eq!(extension.installed_version.as_deref(), Some("1.0.0"));
        assert_eq!(extension.installations[0].chrome_extension_id, OWNED);
        assert_eq!(
            extension.installations[0].path,
            fixture.active.to_string_lossy()
        );
        assert_eq!(extension.download_state, "notDownloaded");
        assert!(!fixture.manager.paths.extension_dir(CATALOG_ID).exists());
        assert_eq!(fixture.mutations.load(AtomicOrdering::SeqCst), 0);
    }

    #[test]
    fn stale_manager_ids_are_ignored_and_wrong_removal_targets_are_blocked() {
        let mut fixture = fixture(None);
        fixture
            .manager
            .paths
            .save_chrome_state(&ChromeState {
                extensions: std::collections::BTreeMap::from([(
                    CATALOG_ID.into(),
                    ChromeInstallRecord {
                        chrome_extension_id: OTHER.into(),
                        version: "1.0.0".into(),
                        installed_at: String::new(),
                    },
                )]),
            })
            .unwrap();
        let cached = fs::read(&fixture.manager.paths.chrome_state).unwrap();
        assert_eq!(
            fixture.manager.snapshot().unwrap().extensions[0].installations[0].chrome_extension_id,
            OWNED
        );
        let mut wrong = target(&fixture);
        wrong.chrome_extension_id = OTHER.into();
        let error =
            tauri::async_runtime::block_on(fixture.manager.remove_selected(CATALOG_ID, wrong))
                .unwrap_err();
        assert!(error.contains("installation changed"));
        assert_eq!(fixture.mutations.load(AtomicOrdering::SeqCst), 0);
        assert_eq!(
            fs::read(&fixture.manager.paths.chrome_state).unwrap(),
            cached
        );
        assert!(fixture.active.join("manifest.json").exists());
        assert!(fixture.unrelated.join("manifest.json").exists());
    }

    #[test]
    fn removal_uses_only_the_selected_registration_and_keeps_source_files() {
        let mut fixture = fixture(None);
        let selected = target(&fixture);
        let unrelated = hash_directory(&fixture.unrelated).unwrap();
        let result =
            tauri::async_runtime::block_on(fixture.manager.remove_selected(CATALOG_ID, selected))
                .unwrap();
        let registrations = registration::inventory(Path::new(&fixture.profiles[0].path)).unwrap();
        assert_eq!(registrations.len(), 1);
        assert_eq!(registrations[0].chrome_extension_id, OTHER);
        assert_eq!(hash_directory(&fixture.unrelated).unwrap(), unrelated);
        assert!(fixture.active.join("manifest.json").exists());
        assert!(matches!(
            result.snapshot.extensions[0].status,
            ExtensionStatus::NotInstalled
        ));
        assert_eq!(fixture.mutations.load(AtomicOrdering::SeqCst), 1);
    }

    #[test]
    fn switching_profile_changes_detection_and_rejects_a_previous_profiles_target() {
        let mut fixture = fixture(None);
        let selected = target(&fixture);
        let snapshot = fixture.manager.select_profile("Profile 1").unwrap();
        assert_eq!(snapshot.selected_profile, "Profile 1");
        assert!(matches!(
            snapshot.extensions[0].status,
            ExtensionStatus::NotInstalled
        ));
        let error =
            tauri::async_runtime::block_on(fixture.manager.remove_selected(CATALOG_ID, selected))
                .unwrap_err();
        assert!(error.contains("profile changed"));
        assert_eq!(fixture.mutations.load(AtomicOrdering::SeqCst), 0);
        assert!(matches!(
            fixture
                .manager
                .select_profile("Default")
                .unwrap()
                .extensions[0]
                .status,
            ExtensionStatus::UpdateReady
        ));
    }

    #[test]
    fn unreadable_registrations_are_a_detection_failure_not_an_empty_installation_list() {
        let fixture = fixture(None);
        fs::write(
            Path::new(&fixture.profiles[0].path).join("Secure Preferences"),
            "invalid JSON",
        )
        .unwrap();
        let snapshot = fixture.manager.snapshot().unwrap();
        assert!(snapshot.detection_error.is_some());
        assert!(matches!(
            snapshot.extensions[0].status,
            ExtensionStatus::DetectionUnavailable
        ));
        assert!(fixture
            .manager
            .prepare_installation(CATALOG_ID, None)
            .is_err());
    }

    #[test]
    fn duplicate_manifest_names_require_an_explicit_installation_selection() {
        let fixture = fixture(None);
        write_extension(&fixture.unrelated, "Sample Extension", "1.0.0");
        let snapshot = fixture.manager.snapshot().unwrap();
        assert!(matches!(
            snapshot.extensions[0].status,
            ExtensionStatus::Ambiguous
        ));
        assert_eq!(snapshot.extensions[0].installations.len(), 2);
        assert!(fixture
            .manager
            .prepare_installation(CATALOG_ID, None)
            .is_err());
        let consent = fixture
            .manager
            .prepare_installation(CATALOG_ID, Some(&target(&fixture)))
            .unwrap()
            .unwrap();
        assert!(registration::same_path(
            Path::new(&consent.path),
            &fixture.active
        ));
    }

    #[test]
    fn manifest_names_match_exactly_without_legacy_aliases() {
        let fixture = fixture(None);
        write_extension(&fixture.active, "Different Name", "1.0.0");
        let snapshot = fixture.manager.snapshot().unwrap();
        assert!(matches!(
            snapshot.extensions[0].status,
            ExtensionStatus::NotInstalled
        ));
        assert!(snapshot.extensions[0].installations.is_empty());
    }

    #[test]
    fn equal_or_older_remote_versions_are_installed_without_a_downgrade_action() {
        let fixture = fixture(None);
        for version in ["1.0.0.0", "0.9"] {
            let mut state = fixture.manager.paths.load_repository_state().unwrap();
            state.extensions.get_mut(CATALOG_ID).unwrap().remote_version = version.into();
            fixture.manager.paths.save_repository_state(&state).unwrap();
            assert!(matches!(
                fixture.manager.snapshot().unwrap().extensions[0].status,
                ExtensionStatus::Installed
            ));
        }
        assert_eq!(fixture.mutations.load(AtomicOrdering::SeqCst), 0);
    }

    #[test]
    fn updates_without_a_trusted_baseline_require_consent_before_touching_files() {
        let mut fixture = fixture(None);
        stage_update(&fixture);
        let selected = target(&fixture);
        let before = hash_directory(&fixture.active).unwrap();
        let error =
            tauri::async_runtime::block_on(fixture.manager.apply(CATALOG_ID, Some(selected), None))
                .unwrap_err();
        assert!(error.contains("Confirm replacement"));
        assert_eq!(hash_directory(&fixture.active).unwrap(), before);
        assert_eq!(fixture.mutations.load(AtomicOrdering::SeqCst), 0);
    }

    #[test]
    fn approved_update_preserves_id_and_unrelated_files_and_keeps_a_backup() {
        let mut fixture = fixture(None);
        stage_update(&fixture);
        let selected = target(&fixture);
        let consent = fixture
            .manager
            .prepare_installation(CATALOG_ID, Some(&selected))
            .unwrap();
        let before = hash_directory(&fixture.active).unwrap();
        let unrelated = hash_directory(&fixture.unrelated).unwrap();
        let result = tauri::async_runtime::block_on(fixture.manager.apply(
            CATALOG_ID,
            Some(selected),
            consent,
        ))
        .unwrap();
        assert!(matches!(
            result.snapshot.extensions[0].status,
            ExtensionStatus::Installed
        ));
        assert_eq!(
            result.snapshot.extensions[0].installed_version.as_deref(),
            Some("1.1.0")
        );
        assert_eq!(
            result.snapshot.extensions[0].installations[0].chrome_extension_id,
            OWNED
        );
        assert_eq!(hash_directory(&fixture.unrelated).unwrap(), unrelated);
        let backups: Vec<_> = fs::read_dir(fixture.active.parent().unwrap())
            .unwrap()
            .map(Result::unwrap)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".nox-backup-")
            })
            .collect();
        assert_eq!(backups.len(), 1);
        assert_eq!(hash_directory(&backups[0].path()).unwrap(), before);
        assert_eq!(fixture.mutations.load(AtomicOrdering::SeqCst), 1);
    }

    #[test]
    fn changed_files_invalidate_consent_and_tampered_downloads_never_reach_chrome() {
        let mut fixture = fixture(None);
        stage_update(&fixture);
        let selected = target(&fixture);
        let consent = fixture
            .manager
            .prepare_installation(CATALOG_ID, Some(&selected))
            .unwrap();
        fs::write(fixture.active.join("content.js"), "new manual edits").unwrap();
        let error = tauri::async_runtime::block_on(fixture.manager.apply(
            CATALOG_ID,
            Some(selected.clone()),
            consent,
        ))
        .unwrap_err();
        assert!(error.contains("Confirm replacement"));
        let consent = fixture
            .manager
            .prepare_installation(CATALOG_ID, Some(&selected))
            .unwrap();
        fs::write(
            fixture
                .manager
                .paths
                .pending_dir(CATALOG_ID, "1.1.0")
                .join("content.js"),
            "tampered download",
        )
        .unwrap();
        let error = tauri::async_runtime::block_on(fixture.manager.apply(
            CATALOG_ID,
            Some(selected),
            consent,
        ))
        .unwrap_err();
        assert!(error.contains("Downloaded files"));
        assert_eq!(
            fs::read_to_string(fixture.active.join("content.js")).unwrap(),
            "new manual edits"
        );
        assert_eq!(fixture.mutations.load(AtomicOrdering::SeqCst), 0);
    }

    #[test]
    fn rejected_update_restores_the_previous_files_and_reloads_the_previous_version() {
        let mut fixture = fixture(Some("1.1.0"));
        stage_update(&fixture);
        let selected = target(&fixture);
        let consent = fixture
            .manager
            .prepare_installation(CATALOG_ID, Some(&selected))
            .unwrap();
        let before = hash_directory(&fixture.active).unwrap();
        let error = tauri::async_runtime::block_on(fixture.manager.apply(
            CATALOG_ID,
            Some(selected),
            consent,
        ))
        .unwrap_err();
        assert!(error.contains("previous files were restored"));
        assert_eq!(hash_directory(&fixture.active).unwrap(), before);
        assert_eq!(fixture.mutations.load(AtomicOrdering::SeqCst), 2);
    }
}
