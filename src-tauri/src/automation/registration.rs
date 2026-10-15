use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

// Read-only: Chrome owns these files. Never infer ownership from a UI label,
// a version number, or an ID previously saved by NoX.
pub fn registered_id(profile: &Path, extension_path: &Path) -> Result<Option<String>, String> {
    id_for_path(&read_settings(profile)?, extension_path)
}

// Chromium updates last_update_time when UnpackedInstaller finishes installing
// the directory, including when the same unpacked extension is loaded again.
pub fn load_stamp(profile: &Path, extension_path: &Path) -> Result<Option<String>, String> {
    let settings = read_settings(profile)?;
    let id = id_for_path(&settings, extension_path)?;
    Ok(id.and_then(|id| {
        settings
            .get(&id)
            .and_then(|entry| entry.get("last_update_time"))
            .map(Value::to_string)
    }))
}

fn read_settings(profile: &Path) -> Result<BTreeMap<String, Value>, String> {
    let mut settings = BTreeMap::new();
    let mut found_preferences = false;
    for name in ["Preferences", "Secure Preferences"] {
        let path = profile.join(name);
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(format!(
                    "Could not verify Chrome extension ownership: {error}"
                ))
            }
        };
        found_preferences = true;
        let preferences: Value = serde_json::from_slice(&bytes)
            .map_err(|error| format!("Could not verify Chrome extension ownership: {error}"))?;
        if let Some(entries) = preferences
            .pointer("/extensions/settings")
            .and_then(Value::as_object)
        {
            settings.extend(
                entries
                    .iter()
                    .map(|(id, entry)| (id.clone(), entry.clone())),
            );
        }
    }
    if !found_preferences {
        return Err(
            "Chrome's Default profile could not be read; no extension action was performed".into(),
        );
    }
    Ok(settings)
}

pub fn verify(profile: &Path, extension_path: &Path, expected_id: &str) -> Result<(), String> {
    let actual = registered_id(profile, extension_path)?;
    if actual.as_deref() != Some(expected_id) {
        return Err(format!(
            "Chrome ID {expected_id} does not belong to {} in the Default profile; action blocked. Sync to refresh the Chrome registration",
            extension_path.display()
        ));
    }
    Ok(())
}

fn id_for_path(
    settings: &BTreeMap<String, Value>,
    extension_path: &Path,
) -> Result<Option<String>, String> {
    if !extension_path.is_absolute() {
        return Err("An absolute extension directory is required".into());
    }
    let expected = normalized_path(extension_path);
    let ids: Vec<_> = settings
        .iter()
        .filter_map(|(id, entry)| {
            let path = Path::new(entry.get("path")?.as_str()?);
            // ManifestLocation::kUnpacked. Store extensions use paths relative to
            // the profile and can never be owned by a managed unpacked directory.
            (entry.get("location").and_then(Value::as_u64) == Some(4)
                && valid_id(id)
                && path.is_absolute()
                && normalized_path(path) == expected)
                .then(|| id.clone())
        })
        .collect();
    match ids.as_slice() {
        [] => Ok(None),
        [id] => Ok(Some(id.clone())),
        _ => Err("Chrome reports multiple IDs for this extension directory; action blocked".into()),
    }
}

fn valid_id(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|byte| (b'a'..=b'p').contains(&byte))
}

fn normalized_path(path: &Path) -> String {
    let canonical: PathBuf = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let value = canonical.to_string_lossy();
    #[cfg(windows)]
    let value = value
        .strip_prefix(r"\\?\")
        .unwrap_or(&value)
        .replace('\\', "/")
        .to_lowercase();
    value.trim_end_matches('/').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const OWNED: &str = "abcdefghijklmnopabcdefghijklmnop";
    const OTHER: &str = "ponmlkjihgfedcbaponmlkjihgfedcba";

    #[test]
    fn rejects_another_extensions_recorded_id() {
        let temp = tempfile::tempdir().unwrap();
        let extension = temp.path().join("PurgeUndesirables");
        let prefs = json!({"extensions":{"settings":{
            OWNED: {"path": extension, "location": 4},
            OTHER: {"path": "BetterTTV/7.7.28_0", "location": 1}
        }}});
        fs::write(temp.path().join("Secure Preferences"), prefs.to_string()).unwrap();
        assert_eq!(
            registered_id(temp.path(), &extension).unwrap().as_deref(),
            Some(OWNED)
        );
        assert!(verify(temp.path(), &extension, OTHER).is_err());
        assert!(verify(temp.path(), &extension, OWNED).is_ok());
    }

    #[test]
    fn missing_directory_still_has_verifiable_ownership() {
        let temp = tempfile::tempdir().unwrap();
        let missing = temp.path().join("deleted");
        let settings = BTreeMap::from([(OWNED.into(), json!({"path": missing, "location": 4}))]);
        assert_eq!(
            id_for_path(&settings, &missing).unwrap().as_deref(),
            Some(OWNED)
        );
    }

    #[test]
    fn ambiguous_paths_and_unreadable_preferences_block_actions() {
        let temp = tempfile::tempdir().unwrap();
        let settings = BTreeMap::from([
            (OWNED.into(), json!({"path": temp.path(), "location": 4})),
            (OTHER.into(), json!({"path": temp.path(), "location": 4})),
        ]);
        assert!(id_for_path(&settings, temp.path()).is_err());
        assert!(registered_id(temp.path(), temp.path()).is_err());
        fs::write(temp.path().join("Preferences"), "invalid JSON").unwrap();
        assert!(registered_id(temp.path(), temp.path()).is_err());
    }
}
