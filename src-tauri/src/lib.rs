mod automation;
mod model;
mod repository;
mod service;
mod storage;

use std::sync::Arc;

use model::{ActionTarget, ExtensionActionResult, ManagerSnapshot, OverwriteConsent};
use service::ManagerService;
use tauri::State;
use tokio::sync::Mutex;

struct AppState {
    manager: Mutex<ManagerService>,
}

#[tauri::command]
async fn get_snapshot(state: State<'_, AppState>) -> Result<ManagerSnapshot, String> {
    state.manager.lock().await.snapshot()
}

#[tauri::command]
async fn sync_extensions(state: State<'_, AppState>) -> Result<ManagerSnapshot, String> {
    state.manager.lock().await.sync().await
}

#[tauri::command]
async fn select_chrome_profile(
    id: String,
    state: State<'_, AppState>,
) -> Result<ManagerSnapshot, String> {
    state.manager.lock().await.select_profile(&id)
}

#[tauri::command]
async fn download_extension(
    id: String,
    state: State<'_, AppState>,
) -> Result<ExtensionActionResult, String> {
    state.manager.lock().await.download(&id).await
}

#[tauri::command]
async fn prepare_installation(
    id: String,
    target: Option<ActionTarget>,
    state: State<'_, AppState>,
) -> Result<Option<OverwriteConsent>, String> {
    state
        .manager
        .lock()
        .await
        .prepare_installation(&id, target.as_ref())
}

#[tauri::command]
async fn install_extension(
    id: String,
    target: Option<ActionTarget>,
    consent: Option<OverwriteConsent>,
    state: State<'_, AppState>,
) -> Result<ExtensionActionResult, String> {
    state.manager.lock().await.apply(&id, target, consent).await
}

#[tauri::command]
async fn update_extension(
    id: String,
    target: Option<ActionTarget>,
    consent: Option<OverwriteConsent>,
    state: State<'_, AppState>,
) -> Result<ExtensionActionResult, String> {
    state.manager.lock().await.apply(&id, target, consent).await
}

#[tauri::command]
async fn remove_extension(
    id: String,
    target: ActionTarget,
    state: State<'_, AppState>,
) -> Result<ExtensionActionResult, String> {
    state
        .manager
        .lock()
        .await
        .remove_selected(&id, target)
        .await
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    #[cfg(target_os = "windows")]
    let automation: Arc<dyn automation::BrowserAutomationBackend> =
        Arc::new(automation::WindowsChromeAutomation::new());
    #[cfg(target_os = "macos")]
    let automation: Arc<dyn automation::BrowserAutomationBackend> =
        Arc::new(automation::MacOsChromeAutomation::new());
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let automation: Arc<dyn automation::BrowserAutomationBackend> =
        Arc::new(automation::UnsupportedAutomation);

    let manager = ManagerService::new(automation)
        .expect("NoX storage and repository services could not be initialized");
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(AppState {
            manager: Mutex::new(manager),
        })
        .invoke_handler(tauri::generate_handler![
            get_snapshot,
            sync_extensions,
            install_extension,
            update_extension,
            remove_extension,
            download_extension,
            select_chrome_profile,
            prepare_installation,
        ])
        .run(tauri::generate_context!())
        .expect("error while running NoX Extension Manager");
}
