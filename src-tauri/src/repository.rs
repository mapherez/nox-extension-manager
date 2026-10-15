use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Component, Path, PathBuf};

use chrono::Utc;
use reqwest::header::{ACCEPT, CACHE_CONTROL, USER_AGENT};
use reqwest::{Client, StatusCode};
use serde::Deserialize;
use sha1::{Digest as Sha1Digest, Sha1};
use sha2::Sha256;
use url::Url;

use crate::model::{
    ChromeManifest, ChromeState, ExtensionStatus, ExtensionView, ManagedExtension, ManagerSnapshot,
    RemoteFile, RepositoryConfig, RepositoryState,
};
use crate::storage::StoragePaths;

const MAX_EXTENSION_BYTES: u64 = 100 * 1024 * 1024;
const MAX_FILE_BYTES: u64 = 25 * 1024 * 1024;

#[derive(Debug, Deserialize)]
struct GitHubContentItem {
    name: String,
    path: String,
    #[serde(rename = "type")]
    kind: String,
    sha: String,
    #[serde(default)]
    size: u64,
    submodule_git_url: Option<String>,
}

pub struct RepositoryClient {
    config: RepositoryConfig,
    client: Client,
    api_base: String,
    raw_base: String,
}

impl RepositoryClient {
    pub fn new(config: RepositoryConfig) -> Result<Self, String> {
        let client = Client::builder()
            .https_only(true)
            .timeout(std::time::Duration::from_secs(20))
            .build()
            .map_err(|error| format!("Could not initialize the GitHub client: {error}"))?;
        Ok(Self {
            config,
            client,
            api_base: "https://api.github.com".into(),
            raw_base: "https://raw.githubusercontent.com".into(),
        })
    }

    pub fn repository_url(&self) -> &str {
        &self.config.repository_url
    }

    // Discovery only: manifests are metadata, never an extension download.
    pub async fn sync(
        &self,
        paths: &StoragePaths,
        state: &mut RepositoryState,
        _chrome: &ChromeState,
    ) -> Result<(), String> {
        let ids = discover_extensions(
            &self.config.extensions_path,
            self.fetch_directory(&self.config.extensions_path).await?,
        )?;
        for entry in state.extensions.values_mut() {
            entry.available = false;
        }
        for id in ids {
            match self.read_manifest(&id).await {
                Ok(Some((manifest, _))) => {
                    let entry = state.extensions.entry(id.clone()).or_default();
                    entry.id = id;
                    entry.icon_relative_path = manifest.preferred_icon();
                    entry.name = manifest.name;
                    entry.description = manifest.description;
                    entry.remote_version = manifest.version;
                    entry.available = true;
                    entry.problem = None;
                }
                Ok(None) => {}
                Err(error) => {
                    let entry = state.extensions.entry(id.clone()).or_default();
                    entry.id = id.clone();
                    if entry.name.is_empty() {
                        entry.name = id;
                    }
                    entry.problem = Some(error);
                }
            }
        }
        state.last_synced_at = Some(Utc::now().to_rfc3339());
        paths.save_repository_state(state)
    }

    async fn read_manifest(&self, id: &str) -> Result<Option<(ChromeManifest, Vec<u8>)>, String> {
        validate_id(id)?;
        let key = Utc::now()
            .timestamp_nanos_opt()
            .unwrap_or_default()
            .to_string();
        let Some(bytes) = self.fetch_raw_file(id, "manifest.json", &key).await? else {
            return Ok(None);
        };
        let manifest: ChromeManifest = serde_json::from_slice(&bytes)
            .map_err(|e| format!("{id}/manifest.json is invalid: {e}"))?;
        validate_manifest(id, &manifest)?;
        Ok(Some((manifest, bytes)))
    }

    pub async fn download(
        &self,
        paths: &StoragePaths,
        state: &mut RepositoryState,
        id: &str,
    ) -> Result<(), String> {
        let catalog = state
            .extensions
            .get(id)
            .ok_or_else(|| format!("Unknown extension: {id}"))?;
        if !catalog.available || catalog.problem.is_some() {
            return Err("Sync the catalog before downloading".into());
        }
        let (manifest, bytes) = self
            .read_manifest(id)
            .await?
            .ok_or("The remote manifest is missing")?;
        if manifest.name != catalog.name || manifest.version != catalog.remote_version {
            return Err("The repository manifest changed; Sync before downloading".into());
        }
        let files = self.extension_files(id).await?;
        let file = files
            .iter()
            .find(|file| file.path == "manifest.json")
            .ok_or("Remote manifest disappeared")?;
        verify_git_oid(&bytes, &file.oid)?;
        let (staging, hashes) = self.download_extension(paths, id, &files, &bytes).await?;
        let destination = paths.pending_dir(id, &manifest.version);
        replace_directory(&staging, &destination, &paths.backups)?;
        let entry = state
            .extensions
            .get_mut(id)
            .ok_or("Catalog entry disappeared")?;
        entry.pending_version = Some(manifest.version);
        entry.pending_hashes = hashes;
        entry.remote_files = files;
        paths.save_repository_state(state)
    }

    #[cfg(test)]
    pub async fn restore_files(
        &self,
        paths: &StoragePaths,
        state: &mut RepositoryState,
        _chrome: &ChromeState,
        id: &str,
    ) -> Result<(), String> {
        self.download(paths, state, id).await
    }

    async fn fetch_directory(&self, directory: &str) -> Result<Vec<GitHubContentItem>, String> {
        let mut url = Url::parse(&self.api_base)
            .map_err(|error| format!("Could not construct the GitHub contents URL: {error}"))?;
        {
            let mut segments = url
                .path_segments_mut()
                .map_err(|_| "Could not build the GitHub contents URL".to_string())?;
            segments.push("repos");
            for segment in self.config.repository.split('/') {
                segments.push(segment);
            }
            segments.push("contents");
            for component in safe_relative_path(directory)?.components() {
                if let Component::Normal(segment) = component {
                    segments.push(&segment.to_string_lossy());
                }
            }
        }
        url.query_pairs_mut()
            .append_pair("ref", &self.config.branch);
        let response = self
            .client
            .get(url)
            .header(ACCEPT, "application/vnd.github+json")
            .header(USER_AGENT, "NoX-Extension-Manager")
            .header(CACHE_CONTROL, "no-cache")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .send()
            .await
            .map_err(|error| format!("Could not read {directory} from GitHub: {error}"))?;
        if !response.status().is_success() {
            return Err(format!(
                "GitHub returned {} while listing {directory}",
                response.status()
            ));
        }
        let items: Vec<GitHubContentItem> = response.json().await.map_err(|error| {
            format!("GitHub returned an invalid directory listing for {directory}: {error}")
        })?;
        if items.len() >= 1_000 {
            return Err(format!(
                "{directory} reached GitHub's directory listing limit"
            ));
        }
        Ok(items)
    }

    async fn extension_files(&self, id: &str) -> Result<Vec<RemoteFile>, String> {
        let root = format!("{}/{}", self.config.extensions_path.trim_matches('/'), id);
        let prefix = format!("{root}/");
        let mut directories = vec![root];
        let mut files = Vec::new();
        while let Some(directory) = directories.pop() {
            for item in self.fetch_directory(&directory).await? {
                if item.submodule_git_url.is_some() {
                    continue;
                }
                let expected_path = format!("{directory}/{}", item.name);
                if item.path != expected_path || item.name.contains(['/', '\\']) {
                    return Err(format!("Unexpected extension file path: {}", item.path));
                }
                let relative = item.path.strip_prefix(&prefix).ok_or_else(|| {
                    format!("File is outside the extension directory: {}", item.path)
                })?;
                safe_relative_path(relative)?;
                match item.kind.as_str() {
                    "dir" => directories.push(item.path),
                    "file" => files.push(RemoteFile {
                        path: relative.to_string(),
                        oid: item.sha,
                        size: item.size,
                    }),
                    _ => {} // Symlinks and submodules are never downloaded.
                }
            }
        }
        files.sort_by(|left, right| left.path.cmp(&right.path));
        Ok(files)
    }

    async fn download_extension(
        &self,
        paths: &StoragePaths,
        id: &str,
        files: &[RemoteFile],
        manifest_bytes: &[u8],
    ) -> Result<(PathBuf, BTreeMap<String, String>), String> {
        let total_size: u64 = files.iter().map(|file| file.size).sum();
        if total_size > MAX_EXTENSION_BYTES {
            return Err(format!("{id} exceeds the 100 MB extension limit"));
        }
        let staging = paths.staging.join(format!(
            "{}-{}",
            id,
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        fs::create_dir_all(&staging)
            .map_err(|error| format!("Could not create staging directory: {error}"))?;

        let result = async {
            let mut hashes = BTreeMap::new();
            for file in files {
                if file.size > MAX_FILE_BYTES {
                    return Err(format!("{} exceeds the 25 MB file limit", file.path));
                }
                let relative = safe_relative_path(&file.path)?;
                let bytes = if file.path == "manifest.json" {
                    manifest_bytes.to_vec()
                } else {
                    self.fetch_raw_file(id, &file.path, &file.oid)
                        .await?
                        .ok_or_else(|| {
                            format!(
                                "{id}/{} disappeared while downloading; sync again",
                                file.path
                            )
                        })?
                };
                verify_git_oid(&bytes, &file.oid)?;
                let destination = staging.join(&relative);
                if let Some(parent) = destination.parent() {
                    fs::create_dir_all(parent).map_err(|error| {
                        format!("Could not create {}: {error}", parent.display())
                    })?;
                }
                fs::write(&destination, &bytes).map_err(|error| {
                    format!("Could not write {}: {error}", destination.display())
                })?;
                hashes.insert(file.path.clone(), sha256_hex(&bytes));
            }
            Ok::<_, String>(hashes)
        }
        .await;

        match result {
            Ok(hashes) => Ok((staging, hashes)),
            Err(error) => {
                let _ = fs::remove_dir_all(&staging);
                Err(error)
            }
        }
    }

    async fn fetch_raw_file(
        &self,
        id: &str,
        relative: &str,
        cache_key: &str,
    ) -> Result<Option<Vec<u8>>, String> {
        let mut url = Url::parse(&self.raw_base)
            .map_err(|error| format!("Could not construct raw GitHub URL: {error}"))?;
        {
            let mut segments = url
                .path_segments_mut()
                .map_err(|_| "Could not build raw GitHub URL".to_string())?;
            for segment in self.config.repository.split('/') {
                segments.push(segment);
            }
            segments.push(&self.config.branch);
            for component in safe_relative_path(&self.config.extensions_path)?.components() {
                if let Component::Normal(segment) = component {
                    segments.push(&segment.to_string_lossy());
                }
            }
            segments.push(id);
            for component in safe_relative_path(relative)?.components() {
                if let Component::Normal(segment) = component {
                    segments.push(&segment.to_string_lossy());
                }
            }
        }
        // A fresh key for manifests avoids stale branch responses; file hashes
        // provide stable keys for downloads without consulting commit metadata.
        url.query_pairs_mut().append_pair("nox", cache_key);
        let response = self
            .client
            .get(url)
            .header(USER_AGENT, "NoX-Extension-Manager")
            .header(CACHE_CONTROL, "no-cache")
            .send()
            .await
            .map_err(|error| format!("Could not download {id}/{relative}: {error}"))?;
        if response.status() == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !response.status().is_success() {
            return Err(format!(
                "GitHub returned {} for {id}/{relative}",
                response.status()
            ));
        }
        let bytes = response
            .bytes()
            .await
            .map_err(|error| format!("Could not read {id}/{relative}: {error}"))?;
        if bytes.len() as u64 > MAX_FILE_BYTES {
            return Err(format!("{id}/{relative} exceeds the 25 MB file limit"));
        }
        Ok(Some(bytes.to_vec()))
    }
}

#[cfg(test)]
#[path = "repository_sync_tests.rs"]
mod sync_tests;

pub fn build_snapshot(
    _paths: &StoragePaths,
    state: &RepositoryState,
    _chrome: &ChromeState,
    repository_url: &str,
) -> ManagerSnapshot {
    let mut extensions: Vec<_> = state
        .extensions
        .values()
        .map(|entry| ExtensionView {
            id: entry.id.clone(),
            name: entry.name.clone(),
            description: entry.description.clone(),
            local_version: None,
            installed_version: None,
            available_version: (!entry.remote_version.is_empty())
                .then(|| entry.remote_version.clone()),
            icon_path: None,
            status: ExtensionStatus::NotInstalled,
            problem: entry.problem.clone(),
            download_state: "notDownloaded".into(),
            downloaded_version: None,
            installations: Vec::new(),
            requires_overwrite_confirmation: false,
            overwrite_path: None,
        })
        .collect();
    extensions.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then_with(|| a.id.cmp(&b.id))
    });
    ManagerSnapshot {
        extensions,
        last_synced_at: state.last_synced_at.clone(),
        repository_url: repository_url.into(),
        profiles: Vec::new(),
        selected_profile: "Default".into(),
        detection_error: None,
    }
}

fn discover_extensions(
    extensions_path: &str,
    items: Vec<GitHubContentItem>,
) -> Result<BTreeSet<String>, String> {
    let prefix = format!("{}/", extensions_path.trim_matches('/'));
    let mut directories = BTreeSet::new();
    for item in items {
        if item.kind != "dir" || item.submodule_git_url.is_some() {
            continue;
        }
        let Some(id) = item.path.strip_prefix(&prefix) else {
            continue;
        };
        if id != item.name || id.contains(['/', '\\']) {
            continue;
        }
        validate_id(id)?;
        directories.insert(id.to_string());
    }
    Ok(directories)
}

pub fn compare_versions(left: &str, right: &str) -> Result<Ordering, String> {
    Ok(parse_version(left)?.cmp(&parse_version(right)?))
}

fn parse_version(version: &str) -> Result<[u16; 4], String> {
    let parts: Vec<_> = version.split('.').collect();
    if parts.is_empty() || parts.len() > 4 {
        return Err(format!("Invalid Chrome extension version: {version}"));
    }
    let mut parsed = [0u16; 4];
    for (index, part) in parts.iter().enumerate() {
        if part.is_empty() || (part.len() > 1 && part.starts_with('0')) {
            return Err(format!("Invalid Chrome extension version: {version}"));
        }
        parsed[index] = part
            .parse::<u16>()
            .map_err(|_| format!("Invalid Chrome extension version: {version}"))?;
    }
    Ok(parsed)
}

fn validate_manifest(id: &str, manifest: &ChromeManifest) -> Result<(), String> {
    if manifest.manifest_version != 3 {
        return Err(format!("{id} must use Manifest V3"));
    }
    if manifest.name.trim().is_empty() {
        return Err(format!("{id} must have a non-empty manifest name"));
    }
    parse_version(&manifest.version)?;
    if let Some(icon) = manifest.preferred_icon() {
        safe_relative_path(&icon)?;
    }
    Ok(())
}

fn validate_id(id: &str) -> Result<(), String> {
    if id.is_empty()
        || !id.chars().all(|character| {
            character.is_ascii_alphanumeric() || character == '-' || character == '_'
        })
    {
        return Err(format!("Invalid extension directory id: {id}"));
    }
    Ok(())
}

fn safe_relative_path(value: &str) -> Result<PathBuf, String> {
    let path = Path::new(value);
    if path.as_os_str().is_empty()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(format!("Unsafe repository path: {value}"));
    }
    Ok(path.to_path_buf())
}

fn verify_git_oid(bytes: &[u8], expected: &str) -> Result<(), String> {
    let header = format!("blob {}\0", bytes.len());
    let actual = match expected.len() {
        40 => {
            let mut digest = Sha1::new();
            digest.update(header.as_bytes());
            digest.update(bytes);
            hex::encode(digest.finalize())
        }
        64 => {
            let mut digest = Sha256::new();
            digest.update(header.as_bytes());
            digest.update(bytes);
            hex::encode(digest.finalize())
        }
        _ => return Err("GitHub returned an unsupported object hash".into()),
    };
    if actual != expected {
        return Err("Downloaded file hash does not match the GitHub directory listing".into());
    }
    Ok(())
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut digest = Sha256::new();
    digest.update(bytes);
    hex::encode(digest.finalize())
}

pub fn copy_directory(source: &Path, destination: &Path) -> Result<(), String> {
    for relative in hash_directory(source)?.keys() {
        let target = destination.join(safe_relative_path(relative)?);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        fs::copy(source.join(relative), target).map_err(|error| error.to_string())?;
    }
    Ok(())
}

pub fn hash_directory(root: &Path) -> Result<BTreeMap<String, String>, String> {
    let mut hashes = BTreeMap::new();
    hash_directory_inner(root, root, &mut hashes)?;
    Ok(hashes)
}

fn hash_directory_inner(
    root: &Path,
    current: &Path,
    hashes: &mut BTreeMap<String, String>,
) -> Result<(), String> {
    let entries = fs::read_dir(current)
        .map_err(|error| format!("Could not read {}: {error}", current.display()))?;
    for entry in entries {
        let entry = entry.map_err(|error| format!("Could not read directory entry: {error}"))?;
        let path = entry.path();
        let kind = entry
            .file_type()
            .map_err(|error| format!("Could not inspect {}: {error}", path.display()))?;
        if kind.is_symlink() {
            return Err(format!(
                "Symbolic links are not allowed: {}",
                path.display()
            ));
        }
        if kind.is_dir() {
            hash_directory_inner(root, &path, hashes)?;
        } else if kind.is_file() {
            let relative = path
                .strip_prefix(root)
                .map_err(|error| format!("Could not normalize {}: {error}", path.display()))?
                .to_string_lossy()
                .replace('\\', "/");
            let bytes = fs::read(&path)
                .map_err(|error| format!("Could not read {}: {error}", path.display()))?;
            hashes.insert(relative, sha256_hex(&bytes));
        }
    }
    Ok(())
}

pub fn replace_directory(source: &Path, destination: &Path, backups: &Path) -> Result<(), String> {
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("Could not create {}: {error}", parent.display()))?;
    }
    fs::create_dir_all(backups)
        .map_err(|error| format!("Could not create {}: {error}", backups.display()))?;
    let backup = backups.join(format!(
        "swap-{}",
        Utc::now().timestamp_nanos_opt().unwrap_or_default()
    ));
    if destination.exists() {
        fs::rename(destination, &backup)
            .map_err(|error| format!("Could not back up {}: {error}", destination.display()))?;
    }
    if let Err(error) = fs::rename(source, destination) {
        if backup.exists() {
            let _ = fs::rename(&backup, destination);
        }
        return Err(format!(
            "Could not promote {}: {error}",
            destination.display()
        ));
    }
    if backup.exists() {
        fs::remove_dir_all(&backup)
            .map_err(|error| format!("Could not remove {}: {error}", backup.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn content_item(name: &str, path: &str, kind: &str) -> GitHubContentItem {
        GitHubContentItem {
            name: name.into(),
            path: path.into(),
            kind: kind.into(),
            sha: "file-hash".into(),
            size: 10,
            submodule_git_url: None,
        }
    }

    #[test]
    fn chrome_versions_compare_numerically() {
        assert_eq!(
            compare_versions("1.10", "1.9.9").unwrap(),
            Ordering::Greater
        );
        assert_eq!(compare_versions("2", "2.0.0.0").unwrap(), Ordering::Equal);
        assert!(compare_versions("1.01", "1.1").is_err());
    }

    #[test]
    fn directory_hashes_detect_changes() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("manifest.json"), b"one").unwrap();
        let before = hash_directory(temp.path()).unwrap();
        fs::write(temp.path().join("manifest.json"), b"two").unwrap();
        let after = hash_directory(temp.path()).unwrap();
        assert_ne!(before, after);
    }

    #[test]
    fn unsafe_paths_and_ids_are_rejected() {
        assert!(safe_relative_path("../manifest.json").is_err());
        assert!(validate_id("Bad Id").is_err());
        assert!(validate_id("safe-extension-1").is_ok());
        assert!(validate_id("InstaReelControls").is_ok());
    }

    #[test]
    fn discovery_lists_only_immediate_extension_directories() {
        let items = vec![
            content_item("First", "extensions/First", "dir"),
            content_item("Nested", "extensions/group/Nested", "dir"),
            content_item("README.md", "extensions/README.md", "file"),
            content_item("Link", "extensions/Link", "symlink"),
        ];
        let discovered = discover_extensions("extensions", items).unwrap();
        assert_eq!(discovered.into_iter().collect::<Vec<_>>(), ["First"]);
    }

    #[test]
    fn manifest_validation_requires_v3_and_a_numeric_version() {
        let valid = ChromeManifest {
            manifest_version: 3,
            name: "Test".into(),
            description: String::new(),
            version: "1.2.3".into(),
            icons: BTreeMap::new(),
        };
        assert!(validate_manifest("Test", &valid).is_ok());
        assert!(validate_manifest(
            "Test",
            &ChromeManifest {
                manifest_version: 2,
                ..valid.clone()
            }
        )
        .is_err());
        assert!(validate_manifest(
            "Test",
            &ChromeManifest {
                version: "one".into(),
                ..valid
            }
        )
        .is_err());
    }

    #[test]
    fn downloaded_bytes_are_checked_against_the_git_blob_id() {
        assert!(verify_git_oid(b"hello\n", "ce013625030ba8dba906f756967f9e9ca394464a").is_ok());
        assert!(verify_git_oid(b"changed", "ce013625030ba8dba906f756967f9e9ca394464a").is_err());
    }

    #[test]
    fn directory_promotion_replaces_the_active_copy() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        let destination = temp.path().join("destination");
        let backups = temp.path().join("backups");
        fs::create_dir(&source).unwrap();
        fs::create_dir(&destination).unwrap();
        fs::write(source.join("version.txt"), "new").unwrap();
        fs::write(destination.join("version.txt"), "old").unwrap();
        replace_directory(&source, &destination, &backups).unwrap();
        assert_eq!(
            fs::read_to_string(destination.join("version.txt")).unwrap(),
            "new"
        );
        assert!(!source.exists());
    }
}
