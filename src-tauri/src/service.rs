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
        let profiles = registration::profiles()?;
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
        if !registration::profiles()?.iter().any(|p| p.id == id) {
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
                snapshot.selected_profile = profile.id;
                registration::inventory(Path::new(&profile.path))
            }
            Err(error) => {
                snapshot.profiles = registration::profiles().unwrap_or_default();
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
        let items = registration::inventory(Path::new(&profile.path))?;
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
        let before = registration::inventory(Path::new(&profile.path))?;
        let backend = self.automation.for_profile(&profile)?;
        let path = PathBuf::from(&installed.path);
        let chrome_id = installed.chrome_extension_id.clone();
        tauri::async_runtime::spawn_blocking(move || {
            backend.verify_identity(&path, &chrome_id)?;
            backend.remove(&path, &chrome_id)
        })
        .await
        .map_err(|e| e.to_string())??;
        let after = registration::inventory(Path::new(&profile.path))?;
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

    #[cfg(test)]
    pub async fn install(&mut self, id: &str) -> Result<ExtensionActionResult, String> {
        self.apply(id, None, None).await
    }
    #[cfg(test)]
    pub async fn update(&mut self, id: &str) -> Result<ExtensionActionResult, String> {
        self.apply(id, None, None).await
    }
    #[cfg(test)]
    pub async fn remove(&mut self, _: &str) -> Result<ExtensionActionResult, String> {
        Err("Exact Chrome target is required".into())
    }
}

#[cfg(test)]
fn prune_backups(directory: &std::path::Path, keep: usize) -> Result<(), String> {
    let mut backups: Vec<_> = fs::read_dir(directory)
        .map_err(|error| format!("Could not inspect extension backups: {error}"))?
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .collect();
    backups.sort_by_key(|entry| {
        entry
            .file_name()
            .to_string_lossy()
            .rsplit_once('-')
            .and_then(|(_, timestamp)| timestamp.parse::<i64>().ok())
            .unwrap_or_default()
    });
    let remove_count = backups.len().saturating_sub(keep);
    for entry in backups.into_iter().take(remove_count) {
        fs::remove_dir_all(entry.path())
            .map_err(|error| format!("Could not prune an old extension backup: {error}"))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ChromeState, ManagedExtension, RepositoryState};
    use std::fs;
    use std::path::Path;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn backup_retention_keeps_the_newest_three() {
        let temp = tempfile::tempdir().unwrap();
        for name in ["1.0.0-1", "1.0.0-2", "1.0.0-3", "1.0.0-4", "1.0.0-5"] {
            fs::create_dir(temp.path().join(name)).unwrap();
        }
        prune_backups(temp.path(), 3).unwrap();
        let mut remaining: Vec<_> = fs::read_dir(temp.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().to_string())
            .collect();
        remaining.sort();
        assert_eq!(remaining, ["1.0.0-3", "1.0.0-4", "1.0.0-5"]);
    }

    struct WrongRegistration {
        mutations: Arc<AtomicUsize>,
    }

    impl BrowserAutomationBackend for WrongRegistration {
        fn registered_id(&self, _: &Path) -> Result<Option<String>, String> {
            Err("Chrome ID belongs to BetterTTV, not the requested directory".into())
        }
        fn preflight(&self) -> Result<(), String> {
            Ok(())
        }
        fn install(&self, _: &Path, _: &str) -> Result<String, String> {
            Ok("ajopnjidmegmdimjlfnijceegpefgped".into())
        }
        fn verify_identity(&self, _: &Path, _: &str) -> Result<(), String> {
            Err("Chrome ID belongs to BetterTTV, not the requested directory".into())
        }
        fn reload(&self, _: &Path, _: &str, _: &str) -> Result<(), String> {
            self.mutations.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        fn remove(&self, _: &Path, _: &str) -> Result<(), String> {
            self.mutations.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    fn manager_fixture() -> (tempfile::TempDir, ManagerService, Arc<AtomicUsize>) {
        let temp = tempfile::tempdir().unwrap();
        let paths = StoragePaths::from_root(temp.path().join("managed"));
        paths.ensure().unwrap();
        let active = paths.extension_dir("PurgeUndesirables");
        fs::create_dir(&active).unwrap();
        fs::write(active.join("content.js"), "active").unwrap();
        let entry = ManagedExtension {
            id: "PurgeUndesirables".into(),
            name: "PurgeUndesirables".into(),
            active_version: Some("1.0.0".into()),
            active_hashes: hash_directory(&active).unwrap(),
            available: true,
            ..ManagedExtension::default()
        };
        paths
            .save_repository_state(&RepositoryState {
                extensions: std::collections::BTreeMap::from([(entry.id.clone(), entry)]),
                ..RepositoryState::default()
            })
            .unwrap();
        let mutations = Arc::new(AtomicUsize::new(0));
        let repository = RepositoryClient::new(RepositoryConfig {
            repository: "owner/repo".into(),
            branch: "master".into(),
            extensions_path: "extensions".into(),
            repository_url: "https://github.com/owner/repo".into(),
        })
        .unwrap();
        let manager = ManagerService {
            paths,
            repository,
            automation: Arc::new(WrongRegistration {
                mutations: mutations.clone(),
            }),
        };
        (temp, manager, mutations)
    }

    fn register_wrong_id(manager: &ManagerService) {
        manager
            .paths
            .save_chrome_state(&ChromeState {
                extensions: std::collections::BTreeMap::from([(
                    "PurgeUndesirables".into(),
                    ChromeInstallRecord {
                        chrome_extension_id: "ajopnjidmegmdimjlfnijceegpefgped".into(),
                        version: "1.0.0".into(),
                        installed_at: String::new(),
                    },
                )]),
            })
            .unwrap();
    }

    #[test]
    fn removing_a_stale_id_never_reaches_chrome_or_changes_records() {
        let (_temp, mut manager, mutations) = manager_fixture();
        register_wrong_id(&manager);
        let before = fs::read(&manager.paths.chrome_state).unwrap();
        let error =
            tauri::async_runtime::block_on(manager.remove("PurgeUndesirables")).unwrap_err();
        assert!(error.contains("BetterTTV"));
        assert_eq!(mutations.load(Ordering::SeqCst), 0);
        assert_eq!(fs::read(&manager.paths.chrome_state).unwrap(), before);
    }

    #[test]
    fn installing_does_not_save_an_id_for_an_unrelated_extension() {
        let (_temp, mut manager, _) = manager_fixture();
        assert!(tauri::async_runtime::block_on(manager.install("PurgeUndesirables")).is_err());
        assert!(manager
            .paths
            .load_chrome_state()
            .unwrap()
            .extensions
            .is_empty());
    }

    #[test]
    fn updating_a_stale_id_preserves_active_and_pending_files() {
        let (_temp, mut manager, mutations) = manager_fixture();
        register_wrong_id(&manager);
        let pending = manager.paths.pending_dir("PurgeUndesirables", "1.1.0");
        fs::create_dir_all(&pending).unwrap();
        fs::write(pending.join("content.js"), "pending").unwrap();
        let mut state = manager.paths.load_repository_state().unwrap();
        let extension = state.extensions.get_mut("PurgeUndesirables").unwrap();
        extension.pending_version = Some("1.1.0".into());
        extension.pending_hashes = hash_directory(&pending).unwrap();
        manager.paths.save_repository_state(&state).unwrap();
        assert!(tauri::async_runtime::block_on(manager.update("PurgeUndesirables")).is_err());
        assert_eq!(mutations.load(Ordering::SeqCst), 0);
        assert_eq!(
            fs::read_to_string(
                manager
                    .paths
                    .extension_dir("PurgeUndesirables")
                    .join("content.js")
            )
            .unwrap(),
            "active"
        );
        assert_eq!(
            fs::read_to_string(pending.join("content.js")).unwrap(),
            "pending"
        );
    }
}
