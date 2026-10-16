use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RepositoryConfig {
    pub repository: String,
    pub branch: String,
    pub extensions_path: String,
    pub repository_url: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RemoteFile {
    pub path: String,
    pub oid: String,
    pub size: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagedExtension {
    pub id: String,
    pub name: String,
    pub description: String,
    pub remote_version: String,
    pub active_version: Option<String>,
    pub pending_version: Option<String>,
    pub icon_relative_path: Option<String>,
    pub available: bool,
    pub remote_files: Vec<RemoteFile>,
    pub active_hashes: BTreeMap<String, String>,
    pub pending_hashes: BTreeMap<String, String>,
    pub problem: Option<String>,
    #[serde(default)]
    pub trusted_path: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RepositoryState {
    pub last_synced_at: Option<String>,
    pub extensions: BTreeMap<String, ManagedExtension>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChromeInstallRecord {
    pub chrome_extension_id: String,
    pub version: String,
    pub installed_at: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChromeState {
    pub extensions: BTreeMap<String, ChromeInstallRecord>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ExtensionStatus {
    NotInstalled,
    Ambiguous,
    DetectionUnavailable,
    Installed,
    UpdateReady,
    Unavailable,
    Error,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionView {
    pub id: String,
    pub name: String,
    pub description: String,
    pub local_version: Option<String>,
    pub installed_version: Option<String>,
    pub available_version: Option<String>,
    pub icon_path: Option<String>,
    pub status: ExtensionStatus,
    pub problem: Option<String>,
    pub download_state: String,
    pub downloaded_version: Option<String>,
    pub installations: Vec<ChromeInstallation>,
    pub requires_overwrite_confirmation: bool,
    pub overwrite_path: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagerSnapshot {
    pub extensions: Vec<ExtensionView>,
    pub last_synced_at: Option<String>,
    pub repository_url: String,
    pub profiles: Vec<ChromeProfile>,
    pub selected_profile: String,
    pub detection_error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionActionResult {
    pub message: String,
    pub snapshot: ManagerSnapshot,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ChromeManifest {
    pub manifest_version: u8,
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub version: String,
    #[serde(default)]
    pub icons: BTreeMap<String, String>,
}

impl ChromeManifest {
    pub fn preferred_icon(&self) -> Option<String> {
        self.icons
            .iter()
            .max_by_key(|(size, _)| size.parse::<u32>().unwrap_or_default())
            .map(|(_, path)| path.clone())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChromeProfile {
    pub id: String,
    pub name: String,
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChromeInstallation {
    pub chrome_extension_id: String,
    pub path: String,
    pub name: String,
    pub version: String,
    pub unpacked: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagerPreferences {
    pub profile: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionTarget {
    pub profile_id: String,
    pub chrome_extension_id: String,
    pub path: String,
    pub version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OverwriteConsent {
    pub profile_id: String,
    pub path: String,
    pub hashes: BTreeMap<String, String>,
}
