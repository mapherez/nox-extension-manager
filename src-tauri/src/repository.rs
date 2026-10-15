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

    pub async fn sync(
        &self,
        paths: &StoragePaths,
        state: &mut RepositoryState,
        chrome: &ChromeState,
    ) -> Result<(), String> {
        self.sync_inner(paths, state, chrome, None).await
    }

    pub async fn restore_files(
        &self,
        paths: &StoragePaths,
        state: &mut RepositoryState,
        chrome: &ChromeState,
        id: &str,
    ) -> Result<(), String> {
        self.sync_inner(paths, state, chrome, Some(id)).await?;
        let entry = state
            .extensions
            .get(id)
            .ok_or_else(|| format!("Unknown extension: {id}"))?;
        if let Some(problem) = &entry.problem {
            return Err(problem.clone());
        }
        Ok(())
    }

    async fn sync_inner(
        &self,
        paths: &StoragePaths,
        state: &mut RepositoryState,
        chrome: &ChromeState,
        repair_id: Option<&str>,
    ) -> Result<(), String> {
        refresh_local_integrity(paths, state, repair_id)?;

        let directory = self.fetch_directory(&self.config.extensions_path).await?;
        let discovered = discover_extensions(&self.config.extensions_path, directory)?;
        if let Some(id) = repair_id {
            if !discovered.contains(id) {
                return Err(format!(
                    "{id} is no longer available in the repository; repair blocked"
                ));
            }
        }

        for extension in state.extensions.values_mut() {
            if repair_id.is_none() || repair_id == Some(extension.id.as_str()) {
                extension.available = false;
            }
        }

        for id in discovered {
            if repair_id.is_some_and(|target| target != id) {
                continue;
            }
            if let Err(error) = self
                .sync_extension(paths, state, chrome, &id, repair_id == Some(id.as_str()))
                .await
            {
                paths.log(&format!("Could not synchronize {id}: {error}"));
                let entry =
                    state
                        .extensions
                        .entry(id.clone())
                        .or_insert_with(|| ManagedExtension {
                            id: id.clone(),
                            name: id.clone(),
                            available: true,
                            ..ManagedExtension::default()
                        });
                entry.available = true;
                entry.problem = Some(error);
            }
        }

        state.last_synced_at = Some(Utc::now().to_rfc3339());
        paths.save_repository_state(state)
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

    async fn sync_extension(
        &self,
        paths: &StoragePaths,
        state: &mut RepositoryState,
        chrome: &ChromeState,
        id: &str,
        restore_local: bool,
    ) -> Result<(), String> {
        validate_id(id)?;
        // Read each manifest on every sync. Extension versions, not changes
        // elsewhere in the repository, decide whether an update is available.
        let cache_key = Utc::now()
            .timestamp_nanos_opt()
            .unwrap_or_default()
            .to_string();
        let Some(manifest_bytes) = self.fetch_raw_file(id, "manifest.json", &cache_key).await?
        else {
            if restore_local {
                return Err(format!(
                    "{id}/manifest.json is no longer available; repair blocked"
                ));
            }
            return Ok(()); // A directory without a root manifest is not an extension.
        };
        let manifest: ChromeManifest = serde_json::from_slice(&manifest_bytes)
            .map_err(|error| format!("{id}/manifest.json is invalid: {error}"))?;
        validate_manifest(id, &manifest)?;
        let existing = state.extensions.get(id).cloned();
        let version_order = existing
            .as_ref()
            .and_then(|entry| {
                entry.active_version.as_deref().or_else(|| {
                    (!entry.remote_version.is_empty()).then_some(entry.remote_version.as_str())
                })
            })
            .map(|version| compare_versions(&manifest.version, version))
            .transpose()?;

        let active_missing = !paths.extension_dir(id).exists();
        let pending_missing = existing.as_ref().is_some_and(|entry| {
            entry
                .pending_version
                .as_ref()
                .is_some_and(|version| !paths.pending_dir(id, version).is_dir())
        });
        if version_order.is_some_and(|order| order != Ordering::Greater)
            && !active_missing
            && !pending_missing
            && !restore_local
            && existing
                .as_ref()
                .is_some_and(|entry| !entry.active_hashes.is_empty())
        {
            let preferred_icon = manifest.preferred_icon();
            let entry = state.extensions.get_mut(id).expect("existing entry");
            entry.available = true;
            entry.name = manifest.name;
            entry.description = manifest.description;
            entry.icon_relative_path = preferred_icon;
            entry.remote_version = manifest.version;
            entry.pending_version = None;
            entry.pending_hashes.clear();
            if entry
                .problem
                .as_deref()
                .is_some_and(|problem| !problem.starts_with("Local files have changed"))
            {
                entry.problem = None;
            }
            return Ok(());
        }

        if !restore_local && !active_missing {
            if let Some(previous) = &existing {
                if previous.pending_version.as_deref() == Some(&manifest.version)
                    && paths.pending_dir(id, &manifest.version).is_dir()
                    && hash_directory(&paths.pending_dir(id, &manifest.version))?
                        == previous.pending_hashes
                {
                    let entry = state.extensions.get_mut(id).expect("existing entry");
                    entry.available = true;
                    entry.remote_version = manifest.version;
                    return Ok(());
                }
            }
        }

        if let Some(previous) = &existing {
            if !restore_local
                && !previous.active_hashes.is_empty()
                && local_files_changed(paths, previous)?
            {
                return Err(
                    "Local files have changed since the last NoX download; update blocked".into(),
                );
            }
        }

        let files = self.extension_files(id).await?;
        let manifest_file = files
            .iter()
            .find(|file| file.path == "manifest.json")
            .ok_or_else(|| {
                format!("{id}/manifest.json disappeared while downloading; sync again")
            })?;
        verify_git_oid(&manifest_bytes, &manifest_file.oid)?;

        let (downloaded_dir, hashes) = self
            .download_extension(paths, id, &files, &manifest_bytes)
            .await?;
        let destination = paths.extension_dir(id);
        let unmanaged = existing
            .as_ref()
            .is_none_or(|entry| entry.active_hashes.is_empty());
        if !restore_local
            && unmanaged
            && destination.exists()
            && hash_directory(&destination)? != hashes
        {
            let _ = fs::remove_dir_all(&downloaded_dir);
            return Err("Local files already exist and differ from the repository; use Repair to restore them with a backup".into());
        }
        let installed = chrome.extensions.contains_key(id);
        let mut entry = existing.unwrap_or_else(|| ManagedExtension {
            id: id.to_string(),
            ..ManagedExtension::default()
        });
        let preferred_icon = manifest.preferred_icon();
        entry.name = manifest.name;
        entry.description = manifest.description;
        entry.remote_version = manifest.version.clone();
        entry.icon_relative_path = preferred_icon;
        entry.available = true;
        entry.remote_files = files;
        entry.problem = None;

        // Rebuild lost NoX metadata only after every local file matches the
        // downloaded repository snapshot. No active files are overwritten.
        if installed
            && !active_missing
            && unmanaged
            && version_order == Some(Ordering::Equal)
            && !restore_local
        {
            fs::remove_dir_all(&downloaded_dir).map_err(|error| error.to_string())?;
            entry.active_version = Some(manifest.version);
            entry.active_hashes = hashes;
            entry.pending_version = None;
            entry.pending_hashes.clear();
            state.extensions.insert(id.to_string(), entry);
            return Ok(());
        }

        if installed && !active_missing && !restore_local {
            let pending = paths.pending_dir(id, &manifest.version);
            replace_directory(&downloaded_dir, &pending, &paths.backups)?;
            entry.pending_version = Some(manifest.version);
            entry.pending_hashes = hashes;
        } else {
            if restore_local && destination.exists() {
                let backup = paths.backup_dir(id).join(format!(
                    "before-repair-{}",
                    Utc::now().timestamp_nanos_opt().unwrap_or_default()
                ));
                fs::create_dir_all(paths.backup_dir(id)).map_err(|error| error.to_string())?;
                fs::rename(&destination, &backup).map_err(|error| {
                    format!("Could not preserve local files before repair: {error}")
                })?;
                if let Err(error) = fs::rename(&downloaded_dir, &destination) {
                    let rollback = fs::rename(&backup, &destination);
                    return Err(format!("Could not restore repository files: {error}; backup rollback: {rollback:?}"));
                }
                paths.log(&format!("Preserved files for {id} at {}", backup.display()));
            } else {
                replace_directory(&downloaded_dir, &destination, &paths.backups)?;
            }
            entry.active_version = Some(manifest.version);
            entry.active_hashes = hashes;
            entry.pending_version = None;
            entry.pending_hashes.clear();
            if installed && !restore_local {
                // Restore deleted files immediately, but still require an explicit
                // Update to reload the registered Chrome extension.
                let pending = paths.pending_dir(id, &entry.remote_version);
                let recovered = paths.staging.join(format!(
                    "recovered-{id}-{}",
                    Utc::now().timestamp_nanos_opt().unwrap_or_default()
                ));
                fs::create_dir_all(&recovered)
                    .map_err(|error| format!("Could not prepare recovered update: {error}"))?;
                copy_directory(&destination, &recovered)?;
                replace_directory(&recovered, &pending, &paths.backups)?;
                entry.pending_version = Some(entry.remote_version.clone());
                entry.pending_hashes = entry.active_hashes.clone();
            }
        }

        state.extensions.insert(id.to_string(), entry);
        Ok(())
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
    paths: &StoragePaths,
    state: &RepositoryState,
    chrome: &ChromeState,
    repository_url: &str,
) -> ManagerSnapshot {
    let mut extensions: Vec<_> = state
        .extensions
        .values()
        .map(|entry| {
            let installed = chrome.extensions.get(&entry.id);
            let status = if let Some(problem) = &entry.problem {
                if problem.starts_with("Local files have changed")
                    || problem.starts_with("Local files already exist")
                {
                    ExtensionStatus::LocallyModified
                } else {
                    ExtensionStatus::Error
                }
            } else if !entry.available {
                ExtensionStatus::Unavailable
            } else if installed.is_some() && entry.pending_version.is_some() {
                ExtensionStatus::UpdateReady
            } else if installed.is_some() {
                ExtensionStatus::Installed
            } else {
                ExtensionStatus::ReadyToInstall
            };
            let icon_path = entry.icon_relative_path.as_ref().and_then(|relative| {
                let path = paths.extension_dir(&entry.id).join(relative);
                path.exists().then(|| path.to_string_lossy().to_string())
            });
            ExtensionView {
                id: entry.id.clone(),
                name: entry.name.clone(),
                description: entry.description.clone(),
                local_version: entry.active_version.clone(),
                installed_version: installed.map(|record| record.version.clone()),
                available_version: entry.pending_version.clone().or_else(|| {
                    Some(entry.remote_version.clone()).filter(|value| !value.is_empty())
                }),
                icon_path,
                status,
                problem: entry.problem.clone(),
            }
        })
        .collect();
    extensions.sort_by(|left, right| {
        left.name
            .to_lowercase()
            .cmp(&right.name.to_lowercase())
            .then_with(|| left.id.cmp(&right.id))
    });
    ManagerSnapshot {
        extensions,
        last_synced_at: state.last_synced_at.clone(),
        repository_url: repository_url.to_string(),
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
    if manifest.name.trim().is_empty() || manifest.name.starts_with("__MSG_") {
        return Err(format!(
            "{id} must use a non-localized manifest name in the MVP"
        ));
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

fn local_files_changed(paths: &StoragePaths, entry: &ManagedExtension) -> Result<bool, String> {
    let directory = paths.extension_dir(&entry.id);
    if !directory.exists() {
        // A deleted directory can be downloaded again without overwriting edits.
        return Ok(false);
    }
    if entry.active_hashes.is_empty() {
        return Ok(true);
    }
    Ok(hash_directory(&directory)? != entry.active_hashes)
}

fn copy_directory(source: &Path, destination: &Path) -> Result<(), String> {
    for relative in hash_directory(source)?.keys() {
        let target = destination.join(safe_relative_path(relative)?);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        fs::copy(source.join(relative), target).map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn refresh_local_integrity(
    paths: &StoragePaths,
    state: &mut RepositoryState,
    target: Option<&str>,
) -> Result<(), String> {
    for entry in state.extensions.values_mut() {
        if target.is_some_and(|id| id != entry.id) {
            continue;
        }
        let changed = local_files_changed(paths, entry)?;
        if changed {
            entry.problem =
                Some("Local files have changed since the last NoX download; update blocked".into());
        } else if entry
            .problem
            .as_deref()
            .is_some_and(|problem| problem.starts_with("Local files have changed"))
        {
            entry.problem = None;
        }
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
