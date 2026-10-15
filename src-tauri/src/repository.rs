use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Component, Path, PathBuf};

use chrono::Utc;
use reqwest::header::{ACCEPT, ETAG, IF_NONE_MATCH, USER_AGENT};
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
struct GitHubCommit {
    sha: String,
    commit: GitHubCommitBody,
}

#[derive(Debug, Deserialize)]
struct GitHubCommitBody {
    tree: GitHubTreePointer,
}

#[derive(Debug, Deserialize)]
struct GitHubTreePointer {
    sha: String,
}

#[derive(Debug, Deserialize)]
struct GitHubTree {
    tree: Vec<GitHubTreeItem>,
    truncated: bool,
}

#[derive(Debug, Deserialize)]
struct GitHubTreeItem {
    path: String,
    mode: String,
    #[serde(rename = "type")]
    kind: String,
    sha: String,
    #[serde(default)]
    size: u64,
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

        let endpoint = format!(
            "{}/repos/{}/commits/{}",
            self.api_base, self.config.repository, self.config.branch
        );
        let mut request = self
            .client
            .get(endpoint)
            .header(ACCEPT, "application/vnd.github+json")
            .header(USER_AGENT, "NoX-Extension-Manager")
            .header("X-GitHub-Api-Version", "2022-11-28");
        if let Some(etag) = state
            .etag
            .as_ref()
            .filter(|_| state.commit_sha.is_some() && state.tree_sha.is_some())
        {
            request = request.header(IF_NONE_MATCH, etag);
        }

        let response = request
            .send()
            .await
            .map_err(|error| format!("Could not reach GitHub: {error}"))?;
        let not_modified = response.status() == StatusCode::NOT_MODIFIED;
        if !not_modified && !response.status().is_success() {
            return Err(format!(
                "GitHub returned {} while checking the extension repository",
                response.status()
            ));
        }

        let etag = response
            .headers()
            .get(ETAG)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned)
            .or_else(|| not_modified.then(|| state.etag.clone()).flatten());
        let commit: GitHubCommit = if not_modified {
            GitHubCommit {
                sha: state
                    .commit_sha
                    .clone()
                    .ok_or("Cached commit is missing; sync again")?,
                commit: GitHubCommitBody {
                    tree: GitHubTreePointer {
                        sha: state
                            .tree_sha
                            .clone()
                            .ok_or("Cached tree is missing; sync again")?,
                    },
                },
            }
        } else {
            response
                .json()
                .await
                .map_err(|error| format!("GitHub returned invalid commit metadata: {error}"))?
        };
        // A cached commit is not proof that local discovery/downloads are complete.
        // Always reconcile the tree, including after a conditional 304 response.
        let tree = self.fetch_tree(&commit.commit.tree.sha).await?;
        let discovered = discover_extensions(&self.config.extensions_path, tree.tree)?;
        if let Some(id) = repair_id {
            if !discovered.contains_key(id) {
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

        for (id, files) in discovered {
            if repair_id.is_some_and(|target| target != id) {
                continue;
            }
            if let Err(error) = self
                .sync_extension(
                    paths,
                    state,
                    chrome,
                    &commit.sha,
                    &id,
                    files,
                    repair_id == Some(id.as_str()),
                )
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

        state.etag = etag;
        state.tree_sha = Some(commit.commit.tree.sha);
        state.commit_sha = Some(commit.sha);
        state.last_synced_at = Some(Utc::now().to_rfc3339());
        paths.save_repository_state(state)
    }

    async fn fetch_tree(&self, tree_sha: &str) -> Result<GitHubTree, String> {
        let endpoint = format!(
            "{}/repos/{}/git/trees/{}?recursive=1",
            self.api_base, self.config.repository, tree_sha
        );
        let response = self
            .client
            .get(endpoint)
            .header(ACCEPT, "application/vnd.github+json")
            .header(USER_AGENT, "NoX-Extension-Manager")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .send()
            .await
            .map_err(|error| format!("Could not read the GitHub tree: {error}"))?;
        if !response.status().is_success() {
            return Err(format!(
                "GitHub tree request failed with {}",
                response.status()
            ));
        }
        let tree: GitHubTree = response
            .json()
            .await
            .map_err(|error| format!("GitHub returned an invalid tree: {error}"))?;
        if tree.truncated {
            return Err("The GitHub extension tree is too large and was truncated".into());
        }
        Ok(tree)
    }

    async fn sync_extension(
        &self,
        paths: &StoragePaths,
        state: &mut RepositoryState,
        chrome: &ChromeState,
        commit_sha: &str,
        id: &str,
        files: Vec<RemoteFile>,
        restore_local: bool,
    ) -> Result<(), String> {
        validate_id(id)?;
        let manifest_file = files
            .iter()
            .find(|file| file.path == "manifest.json")
            .ok_or_else(|| format!("{id} does not contain manifest.json at its root"))?;
        let manifest_bytes = self.fetch_raw_file(commit_sha, id, manifest_file).await?;
        verify_git_oid(&manifest_bytes, &manifest_file.oid)?;
        let manifest: ChromeManifest = serde_json::from_slice(&manifest_bytes)
            .map_err(|error| format!("{id}/manifest.json is invalid: {error}"))?;
        validate_manifest(id, &manifest)?;

        let existing = state.extensions.get(id).cloned();
        let changed = existing
            .as_ref()
            .map(|entry| entry.remote_files != files)
            .unwrap_or(true);

        if let Some(previous) = &existing {
            if changed
                && !previous.remote_version.is_empty()
                && compare_versions(&manifest.version, &previous.remote_version)?
                    != Ordering::Greater
            {
                return Err(format!(
                    "Remote files changed but manifest version {} is not newer than {}",
                    manifest.version, previous.remote_version
                ));
            }
        }

        let active_missing = !paths.extension_dir(id).exists();
        let pending_missing = existing.as_ref().is_some_and(|entry| {
            entry
                .pending_version
                .as_ref()
                .is_some_and(|version| !paths.pending_dir(id, version).is_dir())
        });
        if !changed && !active_missing && !pending_missing && !restore_local {
            let preferred_icon = manifest.preferred_icon();
            let entry = state.extensions.get_mut(id).expect("existing entry");
            entry.available = true;
            entry.name = manifest.name;
            entry.description = manifest.description;
            entry.icon_relative_path = preferred_icon;
            if entry
                .problem
                .as_deref()
                .is_some_and(|problem| !problem.starts_with("Local files have changed"))
            {
                entry.problem = None;
            }
            return Ok(());
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

        let (downloaded_dir, hashes) = self
            .download_extension(paths, commit_sha, id, &files, &manifest_bytes)
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
        commit_sha: &str,
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
                    self.fetch_raw_file(commit_sha, id, file).await?
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
        commit_sha: &str,
        id: &str,
        file: &RemoteFile,
    ) -> Result<Vec<u8>, String> {
        let mut url = Url::parse(&self.raw_base)
            .map_err(|error| format!("Could not construct raw GitHub URL: {error}"))?;
        {
            let mut segments = url
                .path_segments_mut()
                .map_err(|_| "Could not build raw GitHub URL".to_string())?;
            for segment in self.config.repository.split('/') {
                segments.push(segment);
            }
            segments.push(commit_sha);
            segments.push(&self.config.extensions_path);
            segments.push(id);
            for component in safe_relative_path(&file.path)?.components() {
                if let Component::Normal(segment) = component {
                    segments.push(&segment.to_string_lossy());
                }
            }
        }
        let response = self
            .client
            .get(url)
            .header(USER_AGENT, "NoX-Extension-Manager")
            .send()
            .await
            .map_err(|error| format!("Could not download {id}/{}: {error}", file.path))?;
        if !response.status().is_success() {
            return Err(format!(
                "GitHub returned {} for {id}/{}",
                response.status(),
                file.path
            ));
        }
        Ok(response
            .bytes()
            .await
            .map_err(|error| format!("Could not read {id}/{}: {error}", file.path))?
            .to_vec())
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
    items: Vec<GitHubTreeItem>,
) -> Result<BTreeMap<String, Vec<RemoteFile>>, String> {
    let prefix = format!("{}/", extensions_path.trim_matches('/'));
    let mut grouped: BTreeMap<String, Vec<RemoteFile>> = BTreeMap::new();
    let mut manifests = BTreeSet::new();

    for item in items {
        if item.kind != "blob" || item.mode == "120000" {
            continue;
        }
        let Some(remainder) = item.path.strip_prefix(&prefix) else {
            continue;
        };
        let Some((id, relative)) = remainder.split_once('/') else {
            continue;
        };
        validate_id(id)?;
        safe_relative_path(relative)?;
        if relative == "manifest.json" {
            manifests.insert(id.to_string());
        }
        grouped.entry(id.to_string()).or_default().push(RemoteFile {
            path: relative.to_string(),
            oid: item.sha,
            size: item.size,
        });
    }
    grouped.retain(|id, _| manifests.contains(id));
    for files in grouped.values_mut() {
        files.sort_by(|left, right| left.path.cmp(&right.path));
    }
    Ok(grouped)
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
        return Err("Downloaded file hash does not match the GitHub tree".into());
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

    fn tree_file(path: &str, oid: &str) -> GitHubTreeItem {
        GitHubTreeItem {
            path: path.into(),
            mode: "100644".into(),
            kind: "blob".into(),
            sha: oid.into(),
            size: 10,
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
    fn discovery_uses_only_immediate_extension_manifests() {
        let tree = vec![
            tree_file("extensions/First/manifest.json", "one"),
            tree_file("extensions/First/content.js", "two"),
            tree_file("extensions/group/Nested/manifest.json", "three"),
            tree_file("extensions/README.md", "four"),
        ];
        let discovered = discover_extensions("extensions", tree).unwrap();
        assert_eq!(discovered.keys().cloned().collect::<Vec<_>>(), ["First"]);
        assert_eq!(discovered["First"].len(), 2);
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
