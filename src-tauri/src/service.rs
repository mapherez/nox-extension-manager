use std::collections::BTreeSet;
use std::fs;
use std::sync::Arc;

use chrono::Utc;

use crate::automation::BrowserAutomationBackend;
use crate::model::{
    ChromeInstallRecord, ChromeManifest, ChromeState, ExtensionActionResult, ManagedExtension,
    ManagerSnapshot, RepositoryConfig, RepositoryState,
};
use crate::repository::{build_snapshot, compare_versions, hash_directory, RepositoryClient};
use crate::storage::StoragePaths;

pub struct ManagerService {
    paths: StoragePaths,
    repository: RepositoryClient,
    automation: Arc<dyn BrowserAutomationBackend>,
}

impl ManagerService {
    pub fn new(automation: Arc<dyn BrowserAutomationBackend>) -> Result<Self, String> {
        let config: RepositoryConfig = serde_json::from_str(include_str!(
            "../resources/repository.json"
        ))
        .map_err(|error| format!("Embedded repository configuration is invalid: {error}"))?;
        let paths = StoragePaths::discover()?;
        paths.ensure()?;
        let repository = RepositoryClient::new(config)?;
        Ok(Self {
            paths,
            repository,
            automation,
        })
    }

    pub fn snapshot(&self) -> Result<ManagerSnapshot, String> {
        let mut repository = self.paths.load_repository_state()?;
        self.refresh_local_manifests(&mut repository)?;
        self.paths.save_repository_state(&repository)?;
        let chrome = self.chrome_state(&repository)?;
        Ok(build_snapshot(
            &self.paths,
            &repository,
            &chrome,
            self.repository.repository_url(),
        ))
    }

    pub async fn sync(&mut self) -> Result<ManagerSnapshot, String> {
        let mut repository = self.paths.load_repository_state()?;
        self.refresh_local_manifests(&mut repository)?;
        let chrome = self.chrome_state(&repository)?;
        self.paths.log("Extension synchronization started");
        self.repository
            .sync(&self.paths, &mut repository, &chrome)
            .await
            .map_err(|error| {
                self.paths
                    .log(&format!("Extension synchronization failed: {error}"));
                error
            })?;
        self.paths.log("Extension synchronization completed");
        let chrome = self.chrome_state(&repository)?;
        Ok(build_snapshot(
            &self.paths,
            &repository,
            &chrome,
            self.repository.repository_url(),
        ))
    }

    pub async fn install(&mut self, id: &str) -> Result<ExtensionActionResult, String> {
        let mut repository = self.paths.load_repository_state()?;
        self.refresh_local_manifests(&mut repository)?;
        let mut chrome = self.chrome_state(&repository)?;
        if chrome.extensions.contains_key(id) {
            return self.result(
                "This extension is already installed in Chrome",
                &repository,
                &chrome,
            );
        }
        let extension = repository
            .extensions
            .get(id)
            .ok_or_else(|| format!("Unknown extension: {id}"))?;
        ensure_actionable(extension.problem.as_deref())?;
        let version = extension
            .active_version
            .clone()
            .ok_or_else(|| format!("{id} has not been downloaded yet"))?;
        let path = self.paths.extension_dir(id);
        let automation = Arc::clone(&self.automation);
        let action_version = version.clone();
        let chrome_id = tauri::async_runtime::spawn_blocking(move || {
            let chrome_id = automation.install(&path, &action_version)?;
            automation.verify_identity(&path, &chrome_id)?;
            Ok::<_, String>(chrome_id)
        })
        .await
        .map_err(|error| format!("Chrome automation task failed: {error}"))??;
        chrome.extensions.insert(
            id.to_string(),
            ChromeInstallRecord {
                chrome_extension_id: chrome_id,
                version,
                installed_at: Utc::now().to_rfc3339(),
            },
        );
        self.paths.save_chrome_state(&chrome)?;
        self.paths.log(&format!("Installed extension {id}"));
        self.result("Extension installed successfully", &repository, &chrome)
    }

    pub async fn update(&mut self, id: &str) -> Result<ExtensionActionResult, String> {
        let mut repository = self.paths.load_repository_state()?;
        self.refresh_local_manifests(&mut repository)?;
        let mut chrome = self.chrome_state(&repository)?;
        let installed = chrome
            .extensions
            .get(id)
            .cloned()
            .ok_or_else(|| format!("{id} is not registered as installed"))?;
        let extension = repository
            .extensions
            .get_mut(id)
            .ok_or_else(|| format!("Unknown extension: {id}"))?;
        ensure_actionable(extension.problem.as_deref())?;
        if hash_directory(&self.paths.extension_dir(id))? != extension.active_hashes {
            return Err("Local files have changed; update is blocked".into());
        }
        let pending_version = extension
            .pending_version
            .clone()
            .ok_or_else(|| format!("No prepared update exists for {id}"))?;
        let pending = self.paths.pending_dir(id, &pending_version);
        let active = self.paths.extension_dir(id);
        if !pending.is_dir() {
            return Err("The prepared update directory is missing; sync again".into());
        }
        if hash_directory(&pending)? != extension.pending_hashes {
            return Err("The prepared update files have changed; update is blocked".into());
        }
        // Validate ownership before even swapping local files. A stale NoX ID
        // must never reload (or later remove) an unrelated Chrome extension.
        let automation = Arc::clone(&self.automation);
        let target = active.clone();
        let chrome_id = installed.chrome_extension_id.clone();
        tauri::async_runtime::spawn_blocking(move || {
            automation.verify_identity(&target, &chrome_id)
        })
        .await
        .map_err(|error| format!("Chrome automation task failed: {error}"))??;

        let backup_dir = self.paths.backup_dir(id);
        fs::create_dir_all(&backup_dir)
            .map_err(|error| format!("Could not create the extension backup directory: {error}"))?;
        let backup = backup_dir.join(format!(
            "{}-{}",
            installed.version,
            Utc::now().timestamp_millis()
        ));
        fs::rename(&active, &backup)
            .map_err(|error| format!("Could not back up the active extension: {error}"))?;
        if let Err(error) = fs::rename(&pending, &active) {
            let _ = fs::rename(&backup, &active);
            return Err(format!("Could not activate the prepared update: {error}"));
        }

        let automation = Arc::clone(&self.automation);
        let chrome_id = installed.chrome_extension_id.clone();
        let action_version = pending_version.clone();
        let action_path = active.clone();
        let action = tauri::async_runtime::spawn_blocking(move || {
            automation.reload(&action_path, &chrome_id, &action_version)
        })
        .await
        .unwrap_or_else(|error| Err(format!("Chrome automation task failed: {error}")));
        if let Err(error) = action {
            if let Some(parent) = pending.parent() {
                let _ = fs::create_dir_all(parent);
            }
            let _ = fs::rename(&active, &pending);
            let _ = fs::rename(&backup, &active);
            let automation = Arc::clone(&self.automation);
            let chrome_id = installed.chrome_extension_id.clone();
            let previous_version = installed.version.clone();
            let action_path = active.clone();
            let _ = tauri::async_runtime::spawn_blocking(move || {
                automation.reload(&action_path, &chrome_id, &previous_version)
            })
            .await;
            self.paths
                .log(&format!("Update rollback completed for {id}: {error}"));
            return Err(format!(
                "Chrome could not apply the update, so the previous version was restored: {error}"
            ));
        }

        extension.active_version = Some(pending_version.clone());
        extension.active_hashes = std::mem::take(&mut extension.pending_hashes);
        extension.pending_version = None;
        chrome.extensions.insert(
            id.to_string(),
            ChromeInstallRecord {
                chrome_extension_id: installed.chrome_extension_id,
                version: pending_version,
                installed_at: Utc::now().to_rfc3339(),
            },
        );
        self.paths.save_repository_state(&repository)?;
        self.paths.save_chrome_state(&chrome)?;
        if let Err(error) = prune_backups(&backup_dir, 3) {
            self.paths
                .log(&format!("Could not prune old backups for {id}: {error}"));
        }
        self.paths.log(&format!("Updated extension {id}"));
        self.result("Extension updated successfully", &repository, &chrome)
    }

    pub async fn remove(&mut self, id: &str) -> Result<ExtensionActionResult, String> {
        let mut repository = self.paths.load_repository_state()?;
        self.refresh_local_manifests(&mut repository)?;
        let mut chrome = self.chrome_state(&repository)?;
        let installed = chrome
            .extensions
            .get(id)
            .cloned()
            .ok_or_else(|| format!("{id} is not registered as installed"))?;
        let automation = Arc::clone(&self.automation);
        let path = self.paths.extension_dir(id);
        tauri::async_runtime::spawn_blocking(move || {
            automation.verify_identity(&path, &installed.chrome_extension_id)?;
            automation.remove(&path, &installed.chrome_extension_id)
        })
        .await
        .map_err(|error| format!("Chrome automation task failed: {error}"))??;
        chrome.extensions.remove(id);
        self.paths.save_chrome_state(&chrome)?;
        self.paths
            .log(&format!("Removed extension {id} from Chrome"));
        self.result(
            "Extension removed from Chrome; local files were kept",
            &repository,
            &chrome,
        )
    }

    pub async fn repair(&mut self, id: &str) -> Result<ExtensionActionResult, String> {
        let mut repository = self.paths.load_repository_state()?;
        self.refresh_local_manifests(&mut repository)?;
        let mut chrome = self.chrome_state(&repository)?;
        self.repository
            .restore_files(&self.paths, &mut repository, &chrome, id)
            .await?;
        let extension = repository
            .extensions
            .get(id)
            .ok_or_else(|| format!("Unknown extension: {id}"))?;
        ensure_actionable(extension.problem.as_deref())?;
        let version = extension
            .active_version
            .clone()
            .ok_or_else(|| "No local version is ready to repair".to_string())?;
        let path = self.paths.extension_dir(id);
        let automation = Arc::clone(&self.automation);
        let action_version = version.clone();
        let chrome_id = tauri::async_runtime::spawn_blocking(move || {
            // Resolve the actual registration by path; never retry a stored ID
            // which may have belonged to a different extension all along.
            let chrome_id = automation.install(&path, &action_version)?;
            automation.verify_identity(&path, &chrome_id)?;
            Ok::<_, String>(chrome_id)
        })
        .await
        .map_err(|error| format!("Chrome automation task failed: {error}"))??;
        chrome.extensions.insert(
            id.to_string(),
            ChromeInstallRecord {
                chrome_extension_id: chrome_id,
                version,
                installed_at: Utc::now().to_rfc3339(),
            },
        );
        self.paths.save_chrome_state(&chrome)?;
        self.result("Repository files restored and Chrome installation repaired; previous files were backed up", &repository, &chrome)
    }

    fn refresh_local_manifests(&self, repository: &mut RepositoryState) -> Result<(), String> {
        let mut ids: BTreeSet<String> = repository.extensions.keys().cloned().collect();
        for directory in fs::read_dir(&self.paths.root).map_err(|error| error.to_string())? {
            let directory = directory.map_err(|error| error.to_string())?;
            let id = directory.file_name().to_string_lossy().to_string();
            if !id.is_empty()
                && id
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
                && directory.path().join("manifest.json").is_file()
            {
                ids.insert(id);
            }
        }
        for id in ids {
            let path = self.paths.extension_dir(&id);
            let bytes = match fs::read(path.join("manifest.json")) {
                Ok(bytes) => bytes,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(format!("Could not read {id}/manifest.json: {error}")),
            };
            let manifest: ChromeManifest = match serde_json::from_slice(&bytes) {
                Ok(manifest) => manifest,
                Err(error) => {
                    if let Some(entry) = repository.extensions.get_mut(&id) {
                        entry.problem = Some(format!("Invalid local manifest: {error}"));
                    }
                    continue;
                }
            };
            compare_versions(&manifest.version, &manifest.version)?;
            let entry =
                repository
                    .extensions
                    .entry(id.clone())
                    .or_insert_with(|| ManagedExtension {
                        id: id.clone(),
                        name: manifest.name.clone(),
                        description: manifest.description.clone(),
                        ..ManagedExtension::default()
                    });
            // Read installed versions from disk, but retain the last trusted
            // hashes. Recognizing an installation must not authorize overwrites.
            entry.active_version = Some(manifest.version.clone());
            if entry.pending_version.as_ref().is_some_and(|pending| {
                compare_versions(pending, &manifest.version)
                    .is_ok_and(|order| order != std::cmp::Ordering::Greater)
            }) {
                entry.pending_version = None;
                entry.pending_hashes.clear();
            }
            if entry
                .problem
                .as_deref()
                .is_some_and(|problem| problem.starts_with("Invalid local manifest"))
            {
                entry.problem = None;
            }
        }
        Ok(())
    }

    fn chrome_state(&self, repository: &RepositoryState) -> Result<ChromeState, String> {
        let previous = self.paths.load_chrome_state()?;
        let mut chrome = ChromeState::default();
        for (id, extension) in &repository.extensions {
            if let Some(chrome_id) = self
                .automation
                .registered_id(&self.paths.extension_dir(id))?
            {
                let old = previous
                    .extensions
                    .get(id)
                    .filter(|record| record.chrome_extension_id == chrome_id);
                chrome.extensions.insert(
                    id.clone(),
                    ChromeInstallRecord {
                        chrome_extension_id: chrome_id,
                        version: extension
                            .active_version
                            .clone()
                            .or_else(|| old.map(|record| record.version.clone()))
                            .unwrap_or_else(|| "unknown".into()),
                        installed_at: old
                            .map(|record| record.installed_at.clone())
                            .unwrap_or_else(|| Utc::now().to_rfc3339()),
                    },
                );
            }
        }
        self.paths.save_chrome_state(&chrome)?;
        Ok(chrome)
    }

    fn result(
        &self,
        message: &str,
        repository: &crate::model::RepositoryState,
        chrome: &crate::model::ChromeState,
    ) -> Result<ExtensionActionResult, String> {
        Ok(ExtensionActionResult {
            message: message.into(),
            snapshot: build_snapshot(
                &self.paths,
                repository,
                chrome,
                self.repository.repository_url(),
            ),
        })
    }
}

fn ensure_actionable(problem: Option<&str>) -> Result<(), String> {
    problem.map_or(Ok(()), |problem| Err(problem.to_string()))
}

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
