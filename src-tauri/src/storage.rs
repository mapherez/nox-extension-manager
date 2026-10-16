use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use chrono::Utc;
use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::model::{ChromeState, RepositoryState};

#[derive(Debug, Clone)]
pub struct StoragePaths {
    pub root: PathBuf,
    pub internal: PathBuf,
    pub staging: PathBuf,
    pub updates: PathBuf,
    pub backups: PathBuf,
    pub logs: PathBuf,
    pub repository_state: PathBuf,
    pub chrome_state: PathBuf,
}

impl StoragePaths {
    pub fn discover() -> Result<Self, String> {
        let documents = dirs::document_dir()
            .ok_or_else(|| "Windows Documents directory could not be resolved".to_string())?;
        Ok(Self::from_root(documents.join("_NoxChromeExtensions")))
    }

    pub fn from_root(root: PathBuf) -> Self {
        let internal = root.join(".nox");
        Self {
            staging: internal.join("staging"),
            updates: internal.join("updates"),
            backups: internal.join("backups"),
            logs: internal.join("logs"),
            repository_state: internal.join("repository-state.json"),
            chrome_state: internal.join("chrome-state.json"),
            root,
            internal,
        }
    }

    pub fn ensure(&self) -> Result<(), String> {
        for directory in [
            &self.root,
            &self.internal,
            &self.staging,
            &self.updates,
            &self.backups,
            &self.logs,
        ] {
            fs::create_dir_all(directory)
                .map_err(|error| format!("Could not create {}: {error}", directory.display()))?;
        }
        if !self.repository_state.exists() {
            self.save_repository_state(&RepositoryState::default())?;
        }
        if !self.chrome_state.exists() {
            self.save_chrome_state(&ChromeState::default())?;
        }
        Ok(())
    }

    pub fn extension_dir(&self, id: &str) -> PathBuf {
        self.root.join(id)
    }

    pub fn pending_dir(&self, id: &str, version: &str) -> PathBuf {
        self.updates.join(id).join(version)
    }

    pub fn load_repository_state(&self) -> Result<RepositoryState, String> {
        match load_json_or_default(&self.repository_state) {
            Ok(state) => Ok(state),
            Err(error) => {
                self.log(&format!("Ignoring invalid catalog cache: {error}"));
                Ok(RepositoryState::default())
            }
        }
    }

    pub fn preferences(&self) -> Result<crate::model::ManagerPreferences, String> {
        load_json_or_default(&self.internal.join("preferences.json"))
    }

    pub fn save_preferences(&self, value: &crate::model::ManagerPreferences) -> Result<(), String> {
        save_json_atomic(&self.internal.join("preferences.json"), value)
    }

    pub fn save_repository_state(&self, state: &RepositoryState) -> Result<(), String> {
        save_json_atomic(&self.repository_state, state)
    }

    pub fn save_chrome_state(&self, state: &ChromeState) -> Result<(), String> {
        save_json_atomic(&self.chrome_state, state)
    }

    pub fn log(&self, message: &str) {
        let path = self.logs.join("nox.log");
        if let Ok(mut file) = fs::OpenOptions::new().create(true).append(true).open(path) {
            let _ = writeln!(file, "{} {message}", Utc::now().to_rfc3339());
        }
    }
}

fn load_json_or_default<T: DeserializeOwned + Default>(path: &Path) -> Result<T, String> {
    if !path.exists() {
        return Ok(T::default());
    }
    let bytes =
        fs::read(path).map_err(|error| format!("Could not read {}: {error}", path.display()))?;
    serde_json::from_slice(&bytes)
        .map_err(|error| format!("Could not parse {}: {error}", path.display()))
}

fn save_json_atomic<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("{} has no parent directory", path.display()))?;
    fs::create_dir_all(parent)
        .map_err(|error| format!("Could not create {}: {error}", parent.display()))?;

    let temporary = path.with_extension("json.tmp");
    let backup = path.with_extension("json.bak");
    let bytes = serde_json::to_vec_pretty(value)
        .map_err(|error| format!("Could not serialize {}: {error}", path.display()))?;
    fs::write(&temporary, bytes)
        .map_err(|error| format!("Could not write {}: {error}", temporary.display()))?;

    if backup.exists() {
        fs::remove_file(&backup)
            .map_err(|error| format!("Could not clear {}: {error}", backup.display()))?;
    }
    if path.exists() {
        fs::rename(path, &backup)
            .map_err(|error| format!("Could not back up {}: {error}", path.display()))?;
    }
    if let Err(error) = fs::rename(&temporary, path) {
        if backup.exists() {
            let _ = fs::rename(&backup, path);
        }
        return Err(format!("Could not promote {}: {error}", path.display()));
    }
    if backup.exists() {
        fs::remove_file(&backup)
            .map_err(|error| format!("Could not remove {}: {error}", backup.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_round_trip_is_atomic_and_readable() {
        let temp = tempfile::tempdir().unwrap();
        let paths = StoragePaths::from_root(temp.path().join("extensions"));
        paths.ensure().unwrap();

        assert!(paths.chrome_state.exists());
        assert!(paths.repository_state.exists());

        let state = RepositoryState {
            last_synced_at: Some("2026-10-15T15:00:00Z".into()),
            ..RepositoryState::default()
        };
        paths.save_repository_state(&state).unwrap();

        assert_eq!(
            paths
                .load_repository_state()
                .unwrap()
                .last_synced_at
                .as_deref(),
            Some("2026-10-15T15:00:00Z")
        );
        assert!(!paths.repository_state.with_extension("json.tmp").exists());
    }
}
