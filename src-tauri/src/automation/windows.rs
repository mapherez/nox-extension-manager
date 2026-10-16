use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use uiautomation::errors::ERR_NOTFOUND;
use uiautomation::inputs::Keyboard;
use uiautomation::patterns::{UIInvokePattern, UIValuePattern, UIWindowPattern};
use uiautomation::types::{ControlType, Handle};
use uiautomation::{UIAutomation, UIElement};

use windows::core::BOOL;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetClassNameW, GetForegroundWindow, GetWindow, GetWindowThreadProcessId, IsWindow,
    IsWindowVisible, PostMessageW, GW_OWNER, WM_CLOSE,
};

use super::{registration, BrowserAutomationBackend};

const LOAD_UNPACKED_ID: &str = "loadUnpacked";
const FOLDER_PICKER_ADDRESS_BAR_ID: &str = "41477";
const FOLDER_PICKER_CONFIRM_ID: &str = "1";
const DETAILS_REMOVE_ID: &str = "remove-extension";
const DETAILS_REMOVE_NAMES: &[&str] =
    &["Remove extension", "Remover extensão", "Remover a extensão"];
const REMOVE_CONFIRM_NAMES: &[&str] = &["Remove", "Remover", "OK"];

pub struct WindowsChromeAutomation {
    profile: Option<crate::model::ChromeProfile>,
}

impl WindowsChromeAutomation {
    pub fn new() -> Self {
        Self { profile: None }
    }

    fn chrome_path() -> Result<PathBuf, String> {
        let mut candidates = vec![
            PathBuf::from(r"C:\Program Files\Google\Chrome\Application\chrome.exe"),
            PathBuf::from(r"C:\Program Files (x86)\Google\Chrome\Application\chrome.exe"),
        ];
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            candidates.push(PathBuf::from(local).join(r"Google\Chrome\Application\chrome.exe"));
        }
        candidates
            .into_iter()
            .find(|path| path.is_file())
            .ok_or_else(|| "Google Chrome Stable was not found in a standard location".into())
    }

    fn open_extensions_page(&self, url: &str) -> Result<(UIAutomation, UIElement), String> {
        let chrome = Self::chrome_path()?;
        let automation = UIAutomation::new()
            .map_err(|error| format!("Could not initialize Windows UI Automation: {error}"))?;
        let existing_handles: BTreeSet<isize> = chrome_windows(&automation)
            .into_iter()
            .filter_map(|window| {
                window
                    .get_native_window_handle()
                    .ok()
                    .map(|handle| handle.into())
            })
            .collect();

        Command::new(chrome)
            .args([
                format!(
                    "--profile-directory={}",
                    self.profile
                        .as_ref()
                        .map(|p| p.id.as_str())
                        .unwrap_or("Default")
                ),
                "--new-window".into(),
                "about:blank".into(),
            ])
            .spawn()
            .map_err(|error| format!("Could not open Google Chrome: {error}"))?;

        let deadline = Instant::now() + Duration::from_secs(8);
        let window = loop {
            if let Some(window) = chrome_windows(&automation).into_iter().find(|window| {
                window
                    .get_native_window_handle()
                    .ok()
                    .map(|handle| !existing_handles.contains(&handle.into()))
                    .unwrap_or(false)
            }) {
                break window;
            }
            if Instant::now() >= deadline {
                return Err("Chrome did not expose the new browser window to Windows".to_string());
            }
            thread::sleep(Duration::from_millis(200));
        };

        window
            .set_focus()
            .map_err(|error| format!("Chrome could not receive focus: {error}"))?;
        thread::sleep(Duration::from_millis(250));
        // Queue each shortcut/text sequence as a single SendInput batch so the
        // focus hand-off is brief and the URL does not appear character by character.
        let keyboard = Keyboard::new().interval(0);
        keyboard
            .send_keys("{ctrl}(l)")
            .and_then(|_| keyboard.send_text(url))
            .and_then(|_| keyboard.send_keys("{enter}"))
            .map_err(|error| {
                format!("Could not navigate Chrome to chrome://extensions: {error}")
            })?;

        let navigation_deadline = Instant::now() + Duration::from_secs(8);
        loop {
            let title = window.get_name().unwrap_or_default().to_lowercase();
            if title.starts_with("extensions") || title.starts_with("extens") {
                break;
            }
            if Instant::now() >= navigation_deadline {
                return Err(
                    "Chrome opened, but did not navigate to chrome://extensions".to_string()
                );
            }
            thread::sleep(Duration::from_millis(200));
        }

        Ok((automation, window))
    }

    fn with_extensions_page<T>(
        &self,
        action: impl FnOnce(&UIAutomation, &UIElement) -> Result<T, String>,
    ) -> Result<T, String> {
        self.with_extensions_url("chrome://extensions/", action)
    }

    fn with_extensions_url<T>(
        &self,
        url: &str,
        action: impl FnOnce(&UIAutomation, &UIElement) -> Result<T, String>,
    ) -> Result<T, String> {
        let (automation, window) = self.open_extensions_page(url)?;
        let result = action(&automation, &window);
        if let Ok(pattern) = window.get_pattern::<UIWindowPattern>() {
            let _ = pattern.close();
        }
        result
    }

    fn choose_extension_folder(
        automation: &UIAutomation,
        picker: &PickerWindow,
        browser_handle: isize,
        extension_path: &Path,
    ) -> Result<(), String> {
        verify_picker_identity(picker, browser_handle)?;
        let dialog = automation
            .element_from_handle(Handle::from(picker.handle))
            .map_err(|error| format!("Could not access the identified folder picker: {error}"))?;
        let path_text = extension_path.to_string_lossy().to_string();
        dialog
            .set_focus()
            .map_err(|error| format!("The folder picker could not receive focus: {error}"))?;
        let keyboard = Keyboard::new().interval(0);
        require_picker_focus(automation, &dialog)?;
        keyboard.send_keys("{ctrl}(l)").map_err(|error| {
            format!("The folder picker's address bar could not be opened: {error}")
        })?;
        thread::sleep(Duration::from_millis(100));

        // A focused edit is accepted only when its UIA ancestry reaches this
        // exact dialog. A process ID cannot establish which dialog owns it.
        let focused = automation.get_focused_element().map_err(|error| {
            format!("Could not read the folder picker's focused field: {error}")
        })?;
        let edit = if focused
            .get_control_type()
            .map_err(|error| error.to_string())?
            == ControlType::Edit
            && element_is_in_dialog(automation, &focused, &dialog)?
        {
            focused
        } else {
            let edits = automation
                .create_matcher()
                .from(dialog.clone())
                .control_type(ControlType::Edit)
                .filter_fn(Box::new(|element: &UIElement| {
                    Ok(element.get_automation_id()? == FOLDER_PICKER_ADDRESS_BAR_ID)
                }))
                .depth(12)
                .timeout(1_000)
                .find_all()
                .map_err(|error| {
                    format!("The folder picker's address field could not be found: {error}")
                })?;
            exactly_one(edits, "folder picker address field")?
        };
        if !element_is_in_dialog(automation, &edit, &dialog)? {
            return Err(
                "The path field does not belong to the identified folder picker; action blocked"
                    .into(),
            );
        }
        verify_picker_identity(picker, browser_handle)?;
        let value = edit
            .get_pattern::<UIValuePattern>()
            .map_err(|error| format!("The folder path field has no Value pattern: {error}"))?;
        value
            .set_value(&path_text)
            .map_err(|error| format!("The extension folder path could not be entered: {error}"))?;
        let entered = value
            .get_value()
            .map_err(|error| format!("Could not verify the entered folder path: {error}"))?;
        if entered != path_text {
            return Err(
                "The folder path field did not retain the requested path; action blocked".into(),
            );
        }
        edit.set_focus()
            .map_err(|error| format!("The path field could not receive focus: {error}"))?;
        let focused = automation
            .get_focused_element()
            .map_err(|error| error.to_string())?;
        if !automation
            .compare_elements(&focused, &edit)
            .map_err(|error| error.to_string())?
        {
            return Err("Focus left the folder path field; action blocked".into());
        }
        verify_picker_identity(picker, browser_handle)?;
        keyboard
            .send_keys("{enter}")
            .map_err(|error| format!("The extension folder could not be opened: {error}"))?;
        thread::sleep(Duration::from_millis(500));

        let buttons = automation
            .create_matcher()
            .from(dialog.clone())
            .control_type(ControlType::Button)
            .filter_fn(Box::new(|element: &UIElement| {
                Ok(element.get_automation_id()? == FOLDER_PICKER_CONFIRM_ID)
            }))
            .depth(12)
            .timeout(4_000)
            .find_all()
            .map_err(|error| {
                format!("The folder picker's confirm button could not be found: {error}")
            })?;
        let confirm = exactly_one(buttons, "folder picker confirm button")?;
        if !element_is_in_dialog(automation, &confirm, &dialog)? {
            return Err("The confirm button does not belong to the identified folder picker; action blocked".into());
        }
        verify_picker_identity(picker, browser_handle)?;
        invoke(&confirm, "Select Folder")
    }

    fn load_folder(
        &self,
        extension_path: &Path,
        expected_version: &str,
        expected_id: Option<&str>,
    ) -> Result<String, String> {
        self.preflight()?;
        verify_local_version(extension_path, expected_version)?;
        self.with_extensions_page(|automation, window| {
            let profile = self.profile_directory()?;
            let before = registration::load_stamp(&profile, extension_path)?;
            let buttons = automation.create_matcher().from(window.clone())
                .control_type(ControlType::Button)
                .filter_fn(Box::new(|element: &UIElement| {
                    let id = element.get_automation_id().unwrap_or_default();
                    let name = element.get_name().unwrap_or_default();
                    Ok(id == LOAD_UNPACKED_ID || name_matches(&name, &[
                        "Load unpacked", "Carregar sem compactação",
                        "Carregar descompactada", "Carregar extensão não empacotada",
                    ]))
                }))
                .depth(30).timeout(8_000).find_all()
                .map_err(|error| format!("Chrome did not expose Load unpacked: {error}"))?;
            let load = exactly_one(buttons, "Load unpacked button")?;
            let browser_handle: isize = window.get_native_window_handle()
                .map_err(|error| format!("Could not identify Chrome's native window: {error}"))?.into();
            if browser_handle == 0 { return Err("Chrome exposed an invalid native window handle".into()); }
            let before_pickers = picker_windows()?;
            let before_handles = before_pickers.iter().map(|picker| picker.handle).collect();
            log_picker(&format!("Opening picker for Chrome hwnd={browser_handle:#x}, UIA pid={:?}; existing: {}",
                window.get_process_id(), describe_pickers(&before_pickers)));
            invoke(&load, "Load unpacked")?;
            let picker = wait_for_picker(browser_handle, &before_handles)?;
            with_picker_cleanup(&picker, browser_handle, || {
                Self::choose_extension_folder(automation, &picker, browser_handle, extension_path)?;
                let deadline = Instant::now() + Duration::from_secs(25);
            loop {
                if let Some(id) = registration::registered_id(&profile, extension_path)? {
                    if expected_id.is_some_and(|expected| expected != id) {
                        return Err("Chrome registered this folder with a different ID; no other extension was changed".into());
                    }
                    let after = registration::load_stamp(&profile, extension_path)?;
                    if after.is_some() && after != before {
                        verify_local_version(extension_path, expected_version)?;
                        registration::verify(&profile, extension_path, &id)?;
                        return Ok(id);
                    }
                }
                if Instant::now() >= deadline {
                    return Err(format!(
                        "Chrome did not confirm loading {}. Its installation timestamp did not change; check Chrome's Load unpacked error",
                        extension_path.display()
                    ));
                }
                thread::sleep(Duration::from_millis(200));
            }
            })
        })
    }

    fn verified_extension_details(
        automation: &UIAutomation,
        window: &UIElement,
        chrome_extension_id: &str,
        extension_name: &str,
    ) -> Result<UIElement, String> {
        // Chromium flattens extensions-item hosts out of its accessibility tree.
        // Use the dedicated details page instead of inferring card boundaries
        // from neighboring ID labels and Remove buttons in the full list.
        let documents = automation
            .create_matcher()
            .from(window.clone())
            .control_type(ControlType::Document)
            .filter_fn(Box::new(|element: &UIElement| {
                Ok(element.get_automation_id()? == "RootWebArea")
            }))
            .depth(30)
            .timeout(5_000)
            .find_all()
            .map_err(|error| {
                format!("Could not read Chrome's extension details document: {error}")
            })?;
        let document = exactly_one(documents, "extension details document")?;
        let addresses = automation
            .create_matcher()
            .from(window.clone())
            .control_type(ControlType::Edit)
            .filter_fn(Box::new(|element: &UIElement| {
                Ok(element.get_automation_id()? == "view_1012")
            }))
            .depth(30)
            .timeout(0)
            .find_all()
            .map_err(|error| format!("Could not verify Chrome's details URL: {error}"))?;
        let address = exactly_one(addresses, "Chrome address bar")?;
        let url = address
            .get_pattern::<UIValuePattern>()
            .and_then(|pattern| pattern.get_value())
            .map_err(|error| format!("Could not read Chrome's details URL: {error}"))?;
        let headings = automation
            .create_matcher()
            .from(document.clone())
            .filter_fn(Box::new(|element: &UIElement| {
                Ok(element.get_automation_id()? == "name")
            }))
            .depth(30)
            .timeout(5_000)
            .find_all()
            .map_err(|error| format!("Could not read the extension's details heading: {error}"))?;
        let names = headings
            .iter()
            .map(|element| element.get_name().map_err(|error| error.to_string()))
            .collect::<Result<Vec<_>, _>>()?;
        let labels = automation
            .create_matcher()
            .from(document.clone())
            .control_type(ControlType::Text)
            .depth(30)
            .timeout(0)
            .find_all()
            .map_err(|error| format!("Could not read the extension's visible ID: {error}"))?;
        let mut ids = BTreeSet::new();
        for label in labels {
            let name = label
                .get_name()
                .map_err(|error| format!("Could not read details text: {error}"))?;
            ids.extend(extract_chrome_extension_ids(&name));
        }
        if !details_identity_matches(&url, chrome_extension_id, extension_name, &ids, &names) {
            return Err(format!("Extension details identity did not match {chrome_extension_id} ({extension_name}); URL={url}, IDs={ids:?}, names={names:?}; removal blocked"));
        }
        Ok(document)
    }

    fn find_details_remove(
        automation: &UIAutomation,
        document: &UIElement,
    ) -> Result<UIElement, String> {
        let buttons = automation
            .create_matcher()
            .from(document.clone())
            .filter_fn(Box::new(|element: &UIElement| {
                Ok(is_details_remove_control(
                    element.get_control_type()?,
                    &element.get_automation_id()?,
                    &element.get_name()?,
                ))
            }))
            .depth(30)
            .timeout(3_000)
            .find_all()
            .map_err(|error| {
                format!(
                    "Chrome did not expose Remove extension in the verified details page: {error}"
                )
            })?;
        exactly_one(
            buttons,
            "Remove extension control in the verified details page",
        )
    }

    fn profile_directory(&self) -> Result<PathBuf, String> {
        if let Some(profile) = &self.profile {
            return Ok(PathBuf::from(&profile.path));
        }
        std::env::var_os("LOCALAPPDATA")
            .map(|local| PathBuf::from(local).join(r"Google\Chrome\User Data\Default"))
            .ok_or_else(|| "Chrome's Default profile directory could not be resolved".into())
    }
}

impl BrowserAutomationBackend for WindowsChromeAutomation {
    fn for_profile(
        &self,
        profile: &crate::model::ChromeProfile,
    ) -> Result<std::sync::Arc<dyn BrowserAutomationBackend>, String> {
        Ok(std::sync::Arc::new(Self {
            profile: Some(profile.clone()),
        }))
    }
    fn registered_id(&self, extension_path: &Path) -> Result<Option<String>, String> {
        let profile = self.profile_directory()?;
        if !profile.is_dir() {
            return Ok(None);
        }
        registration::registered_id(&profile, extension_path)
    }

    fn preflight(&self) -> Result<(), String> {
        Self::chrome_path().map(|_| ())
    }

    fn install(&self, extension_path: &Path, expected_version: &str) -> Result<String, String> {
        let existing = self.registered_id(extension_path)?;
        self.load_folder(extension_path, expected_version, existing.as_deref())
    }

    fn verify_identity(
        &self,
        extension_path: &Path,
        chrome_extension_id: &str,
    ) -> Result<(), String> {
        registration::verify(
            &self.profile_directory()?,
            extension_path,
            chrome_extension_id,
        )
    }

    fn reload(
        &self,
        extension_path: &Path,
        chrome_extension_id: &str,
        expected_version: &str,
    ) -> Result<(), String> {
        self.verify_identity(extension_path, chrome_extension_id)?;
        self.load_folder(extension_path, expected_version, Some(chrome_extension_id))?;
        Ok(())
    }

    fn remove(&self, extension_path: &Path, chrome_extension_id: &str) -> Result<(), String> {
        self.verify_identity(extension_path, chrome_extension_id)?;
        let manifest: crate::model::ChromeManifest = serde_json::from_slice(
            &std::fs::read(extension_path.join("manifest.json")).map_err(|error| {
                format!("Could not verify the extension name: {error}. Sync first")
            })?,
        )
        .map_err(|error| format!("Could not verify the extension name: {error}"))?;
        if manifest.name.trim().is_empty() {
            return Err("The extension name is empty; removal blocked".into());
        }
        let url = extension_details_url(chrome_extension_id)?;
        let result = self.with_extensions_url(&url, |automation, window| {
            self.verify_identity(extension_path, chrome_extension_id)?;
            let document = Self::verified_extension_details(
                automation,
                window,
                chrome_extension_id,
                &manifest.name,
            )?;
            let remove = Self::find_details_remove(automation, &document)?;
            self.verify_identity(extension_path, chrome_extension_id)?;
            // Re-read the page immediately before invoking its destructive
            // action; stale navigation must never select another installation.
            Self::verified_extension_details(
                automation,
                window,
                chrome_extension_id,
                &manifest.name,
            )?;
            if !removal_dialogs(automation, window, &manifest.name)?.is_empty() {
                return Err(
                    "A removal popup was already open before this action; removal blocked".into(),
                );
            }
            invoke(&remove, "Remove")?;
            let dialog = wait_for_removal_dialog(automation, window, &manifest.name)?;
            let buttons = automation
                .create_matcher()
                .from(dialog.clone())
                .control_type(ControlType::Button)
                .filter_fn(Box::new(|element: &UIElement| {
                    Ok(name_matches(&element.get_name()?, REMOVE_CONFIRM_NAMES))
                }))
                .depth(20)
                .timeout(3_000)
                .find_all()
                .map_err(|error| {
                    format!(
                        "Chrome's removal confirmation was not found in the target dialog: {error}"
                    )
                })?;
            let confirm = exactly_one(buttons, "removal confirmation")?;
            prepare_removal_confirmation(automation, window, &dialog, &confirm)?;
            self.verify_identity(extension_path, chrome_extension_id)?;
            // Invoke can report success before Chrome has accepted its native
            // popup. Activate the verified, focused button with a real keypress
            // instead, once only. Never send Enter to an arbitrary active dialog.
            require_removal_confirmation_focus(automation, window, &dialog, &confirm)?;
            Keyboard::new()
                .interval(0)
                .send_keys("{space}")
                .map_err(|error| {
                    format!("Could not activate the removal popup's confirmation: {error}")
                })?;
            wait_for_removal_dialog_to_close(automation, window, &manifest.name, &dialog)?;
            let deadline = Instant::now() + Duration::from_secs(15);
            while registration::registered_id(&self.profile_directory()?, extension_path)?.is_some()
            {
                if Instant::now() >= deadline {
                    return Err("Chrome still reports the extension after Remove".into());
                }
                thread::sleep(Duration::from_millis(100));
            }
            Ok(())
        });
        if let Err(error) = &result {
            if let Ok(paths) = crate::storage::StoragePaths::discover() {
                paths.log(&format!(
                    "Chrome Remove: profile={}, path={}, ID={chrome_extension_id}; {error}",
                    self.profile_directory()
                        .map(|path| path.display().to_string())
                        .unwrap_or_else(|_| "<unavailable>".into()),
                    extension_path.display()
                ));
            }
        }
        result
    }
}

// Removal confirmations can be UIA descendants or separate native popups.
// Neither the browser window itself nor another window from the same process
// is a confirmation. Require ancestry/ownership of this operation's window.
fn native_window_is_owned_by(handle: isize, browser: isize) -> bool {
    if handle == 0 || browser == 0 || handle == browser {
        return false;
    }
    let mut current = native_handle(handle);
    let mut seen = BTreeSet::from([handle]);
    for _ in 0..32 {
        // SAFETY: GetWindow only inspects the native ownership relationship.
        let Ok(owner) = (unsafe { GetWindow(current, GW_OWNER) }) else {
            return false;
        };
        let owner_handle = owner.0 as isize;
        if owner_handle == browser {
            return true;
        }
        if !seen.insert(owner_handle) {
            return false;
        }
        current = owner;
    }
    false
}

fn removal_dialog_belongs_to_window(
    automation: &UIAutomation,
    dialog: &UIElement,
    window: &UIElement,
) -> Result<bool, String> {
    if automation
        .compare_elements(dialog, window)
        .map_err(|error| error.to_string())?
    {
        return Ok(false);
    }
    if element_is_in_dialog(automation, dialog, window)? {
        return Ok(true);
    }
    let handle: isize = dialog
        .get_native_window_handle()
        .map_err(|error| error.to_string())?
        .into();
    let browser: isize = window
        .get_native_window_handle()
        .map_err(|error| error.to_string())?
        .into();
    Ok(native_window_is_owned_by(handle, browser))
}

fn removal_dialogs(
    automation: &UIAutomation,
    window: &UIElement,
    extension_name: &str,
) -> Result<Vec<UIElement>, String> {
    let scoped = automation
        .create_matcher()
        .from(window.clone())
        .filter_fn(Box::new(|element: &UIElement| {
            Ok(element.is_dialog().unwrap_or(false)
                || element.get_control_type()? == ControlType::Window)
        }))
        .depth(30)
        .timeout(0)
        .find_all();
    let mut candidates = match scoped {
        Ok(elements) => elements,
        Err(error) if error.code() == ERR_NOTFOUND => Vec::new(),
        Err(error) => {
            return Err(format!(
                "Could not inspect Chrome's removal popups: {error}"
            ))
        }
    };
    let browser: isize = window
        .get_native_window_handle()
        .map_err(|error| error.to_string())?
        .into();
    let mut handles = Vec::<isize>::new();
    // SAFETY: EnumWindows invokes the callback synchronously with this live Vec.
    unsafe {
        EnumWindows(
            Some(collect_window),
            LPARAM(&mut handles as *mut Vec<isize> as isize),
        )
    }
    .map_err(|error| format!("Could not inspect native removal popups: {error}"))?;
    for handle in handles {
        if native_window_is_owned_by(handle, browser)
            && unsafe { IsWindowVisible(native_handle(handle)).as_bool() }
        {
            let popup = automation
                .element_from_handle(Handle::from(handle))
                .map_err(|error| format!("Could not read the owned Chrome popup: {error}"))?;
            candidates.push(popup);
        }
    }
    let mut dialogs = Vec::new();
    for candidate in candidates {
        if !removal_dialog_belongs_to_window(automation, &candidate, window)? {
            continue;
        }
        if candidate
            .is_offscreen()
            .map_err(|error| error.to_string())?
        {
            continue;
        }
        let title = candidate.get_name().map_err(|error| error.to_string())?;
        let names_extension = if title.contains(extension_name) {
            true
        } else {
            let expected_name = extension_name.to_string();
            match automation
                .create_matcher()
                .from(candidate.clone())
                .control_type(ControlType::Text)
                .filter_fn(Box::new(move |element: &UIElement| {
                    Ok(element.get_name()?.contains(&expected_name))
                }))
                .depth(20)
                .timeout(0)
                .find_all()
            {
                Ok(_) => true,
                Err(error) if error.code() == ERR_NOTFOUND => false,
                Err(error) => {
                    return Err(format!(
                        "Could not verify the removal popup's extension name: {error}"
                    ))
                }
            }
        };
        if !names_extension {
            continue;
        }
        let mut duplicate = false;
        for existing in &dialogs {
            if automation
                .compare_elements(existing, &candidate)
                .map_err(|error| error.to_string())?
            {
                duplicate = true;
                break;
            }
        }
        if !duplicate {
            dialogs.push(candidate);
        }
    }
    // Some UIA providers expose both the native popup Window and its inner
    // Dialog. These are one prompt: retain the innermost named dialog rather
    // than treating its wrapper as a second confirmation.
    let mut isolated = Vec::new();
    for (index, dialog) in dialogs.iter().enumerate() {
        let mut wraps_dialog = false;
        for (other_index, other) in dialogs.iter().enumerate() {
            if index != other_index && element_is_in_dialog(automation, other, dialog)? {
                wraps_dialog = true;
                break;
            }
        }
        if !wraps_dialog {
            isolated.push(dialog.clone());
        }
    }
    Ok(isolated)
}

fn wait_for_removal_dialog(
    automation: &UIAutomation,
    window: &UIElement,
    extension_name: &str,
) -> Result<UIElement, String> {
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        let dialogs = removal_dialogs(automation, window, extension_name)?;
        if !dialogs.is_empty() {
            return exactly_one(dialogs, "new removal popup owned by this Chrome window");
        }
        if Instant::now() >= deadline {
            return Err(format!("Chrome's confirmation popup for {extension_name} did not appear in this operation's window; no confirmation was sent"));
        }
        thread::sleep(Duration::from_millis(100));
    }
}

fn prepare_removal_confirmation(
    automation: &UIAutomation,
    window: &UIElement,
    dialog: &UIElement,
    confirm: &UIElement,
) -> Result<(), String> {
    if !removal_dialog_belongs_to_window(automation, dialog, window)?
        || !element_is_in_dialog(automation, confirm, dialog)?
        || !confirm.is_enabled().map_err(|error| error.to_string())?
        || !name_matches(
            &confirm.get_name().map_err(|error| error.to_string())?,
            REMOVE_CONFIRM_NAMES,
        )
    {
        return Err("The confirmation button is not enabled in this operation's removal popup; action blocked".into());
    }
    confirm
        .set_focus()
        .map_err(|error| format!("Could not focus the removal popup's confirmation: {error}"))?;
    require_removal_confirmation_focus(automation, window, dialog, confirm)
}

fn require_removal_confirmation_focus(
    automation: &UIAutomation,
    window: &UIElement,
    dialog: &UIElement,
    confirm: &UIElement,
) -> Result<(), String> {
    let focused = automation
        .get_focused_element()
        .map_err(|error| error.to_string())?;
    let browser: isize = window
        .get_native_window_handle()
        .map_err(|error| error.to_string())?
        .into();
    // SAFETY: this is a read-only check immediately before sending input.
    let foreground = unsafe { GetForegroundWindow() }.0 as isize;
    if !removal_dialog_belongs_to_window(automation, dialog, window)?
        || !element_is_in_dialog(automation, confirm, dialog)?
        || !automation
            .compare_elements(&focused, confirm)
            .map_err(|error| error.to_string())?
        || !(foreground == browser || native_window_is_owned_by(foreground, browser))
    {
        return Err("Focus is outside the verified removal confirmation; no key was sent".into());
    }
    Ok(())
}

fn wait_for_removal_dialog_to_close(
    automation: &UIAutomation,
    window: &UIElement,
    extension_name: &str,
    dialog: &UIElement,
) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let current = removal_dialogs(automation, window, extension_name)?;
        let mut still_open = false;
        for candidate in current {
            if automation
                .compare_elements(&candidate, dialog)
                .map_err(|error| error.to_string())?
            {
                still_open = true;
            } else {
                return Err("A different removal popup appeared after confirmation; no further input was sent".into());
            }
        }
        if !still_open {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err("Chrome's removal popup remained open after activating its confirmation; no retry or alternate button was attempted".into());
        }
        thread::sleep(Duration::from_millis(100));
    }
}

// Native top-level windows are enumerated independently of UIA provider PIDs.
// Only a newly opened, visible picker owned by the operation's Chrome window
// is eligible; unrelated dialogs are diagnostic candidates, never fallbacks.
#[derive(Clone, Debug, PartialEq, Eq)]
struct PickerWindow {
    handle: isize,
    class: String,
    pid: u32,
    owners: Vec<isize>,
}

fn native_handle(handle: isize) -> HWND {
    HWND(handle as *mut std::ffi::c_void)
}

fn read_picker_window(handle: isize) -> Result<Option<PickerWindow>, String> {
    let hwnd = native_handle(handle);
    // SAFETY: these Win32 calls inspect an opaque window handle. Windows may
    // destroy a window between enumeration and inspection, which is checked.
    if !unsafe { IsWindow(Some(hwnd)).as_bool() && IsWindowVisible(hwnd).as_bool() } {
        return Ok(None);
    }
    let mut name = [0u16; 256];
    let length = unsafe { GetClassNameW(hwnd, &mut name) };
    if length == 0 {
        let error = windows::core::Error::from_thread();
        if !unsafe { IsWindow(Some(hwnd)).as_bool() } {
            return Ok(None);
        }
        return Err(format!("GetClassNameW failed for {handle:#x}: {error}"));
    }
    let class = String::from_utf16_lossy(&name[..length as usize]);
    if class != "#32770" && class != "DirectUIHWND" {
        return Ok(None);
    }
    let mut pid = 0;
    if unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) } == 0 {
        let error = windows::core::Error::from_thread();
        if !unsafe { IsWindow(Some(hwnd)).as_bool() } {
            return Ok(None);
        }
        return Err(format!(
            "GetWindowThreadProcessId failed for {handle:#x}: {error}"
        ));
    }
    let mut owners = Vec::new();
    let mut seen = BTreeSet::from([handle]);
    let mut current = hwnd;
    for _ in 0..32 {
        // No owner is the normal end of the chain. A visited set and a bound
        // prevent an invalid/cyclic relationship from hanging enumeration.
        let Ok(owner) = (unsafe { GetWindow(current, GW_OWNER) }) else {
            break;
        };
        let owner_handle = owner.0 as isize;
        if !seen.insert(owner_handle) {
            return Err(format!("Cyclic owner chain for picker {handle:#x}"));
        }
        owners.push(owner_handle);
        current = owner;
    }
    Ok(Some(PickerWindow {
        handle,
        class,
        pid,
        owners,
    }))
}

unsafe extern "system" fn collect_window(hwnd: HWND, parameter: LPARAM) -> BOOL {
    // SAFETY: EnumWindows calls synchronously; the caller keeps this Vec
    // alive and exclusively borrowed until enumeration completes.
    let handles = unsafe { &mut *(parameter.0 as *mut Vec<isize>) };
    handles.push(hwnd.0 as isize);
    BOOL(1)
}

fn picker_windows() -> Result<Vec<PickerWindow>, String> {
    let mut handles: Vec<isize> = Vec::new();
    // SAFETY: the callback has the required ABI and receives the live Vec above.
    unsafe {
        EnumWindows(
            Some(collect_window),
            LPARAM(&mut handles as *mut Vec<isize> as isize),
        )
    }
    .map_err(|error| format!("Could not enumerate Windows folder pickers: {error}"))?;
    let mut pickers = Vec::new();
    for handle in handles {
        if let Some(picker) = read_picker_window(handle)? {
            pickers.push(picker);
        }
    }
    Ok(pickers)
}

fn belongs_to_browser(picker: &PickerWindow, browser: isize) -> bool {
    browser != 0
        && picker.handle != browser
        && matches!(picker.class.as_str(), "#32770" | "DirectUIHWND")
        && picker.owners.contains(&browser)
}

fn select_picker_index(
    pickers: &[PickerWindow],
    browser: isize,
    before: &BTreeSet<isize>,
) -> Result<Option<usize>, String> {
    let matches: Vec<_> = pickers
        .iter()
        .enumerate()
        .filter(|(_, picker)| {
            belongs_to_browser(picker, browser) && !before.contains(&picker.handle)
        })
        .map(|(index, _)| index)
        .collect();
    match matches.as_slice() {
        [] => Ok(None),
        [index] => Ok(Some(*index)),
        _ => Err(format!(
            "Found {} folder pickers owned by this Chrome window; action blocked",
            matches.len()
        )),
    }
}

fn describe_pickers(pickers: &[PickerWindow]) -> String {
    pickers
        .iter()
        .map(|picker| {
            format!(
                "hwnd={:#x}, class={}, pid={}, owners={:?}",
                picker.handle, picker.class, picker.pid, picker.owners
            )
        })
        .collect::<Vec<_>>()
        .join("; ")
}

fn log_picker(message: &str) {
    if let Ok(paths) = crate::storage::StoragePaths::discover() {
        paths.log(&format!("Folder picker: {message}"));
    }
}

fn wait_for_picker(browser: isize, before: &BTreeSet<isize>) -> Result<PickerWindow, String> {
    let deadline = Instant::now() + Duration::from_secs(8);
    let mut last = None;
    loop {
        let pickers = picker_windows().map_err(|error| {
            let error = format!("{error}; previous candidates: {:?}", last);
            log_picker(&error);
            error
        })?;
        if last.as_ref() != Some(&pickers) {
            log_picker(&format!(
                "Chrome hwnd={browser:#x}; candidates: {}",
                describe_pickers(&pickers)
            ));
            last = Some(pickers.clone());
        }
        match select_picker_index(&pickers, browser, before) {
            Ok(Some(index)) => return Ok(pickers[index].clone()),
            Err(error) => {
                let error = format!("{error}; candidates: {}", describe_pickers(&pickers));
                log_picker(&error);
                return Err(error);
            }
            Ok(None) => {}
        }
        if Instant::now() >= deadline {
            let error = format!("No new folder picker owned by Chrome hwnd={browser:#x} was identified within 8 seconds; candidates: {}",
                describe_pickers(&pickers));
            log_picker(&error);
            return Err(error);
        }
        thread::sleep(Duration::from_millis(100));
    }
}

fn verify_picker_identity(picker: &PickerWindow, browser: isize) -> Result<(), String> {
    let current = read_picker_window(picker.handle)?
        .ok_or("The identified folder picker is no longer open")?;
    // Compare the picker only with its own captured PID to detect HWND reuse;
    // its PID is never required to equal Chrome's PID.
    if current.pid != picker.pid
        || current.class != picker.class
        || !belongs_to_browser(&current, browser)
    {
        return Err(format!(
            "The folder picker's identity or owner changed; action blocked. Current: {}",
            describe_pickers(&[current])
        ));
    }
    Ok(())
}

fn cancel_picker(picker: &PickerWindow, browser: isize) -> Result<(), String> {
    if read_picker_window(picker.handle)?.is_none() {
        return Ok(());
    }
    verify_picker_identity(picker, browser)?;
    // WM_CLOSE targets only the verified picker HWND; no global Escape or click.
    unsafe {
        PostMessageW(
            Some(native_handle(picker.handle)),
            WM_CLOSE,
            WPARAM(0),
            LPARAM(0),
        )
    }
    .map_err(|error| format!("Could not close the identified folder picker: {error}"))?;
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if read_picker_window(picker.handle)?.is_none() {
            return Ok(());
        }
        verify_picker_identity(picker, browser)?;
        if Instant::now() >= deadline {
            return Err("The identified folder picker remained open after cancellation".into());
        }
        thread::sleep(Duration::from_millis(100));
    }
}

fn with_picker_cleanup<T>(
    picker: &PickerWindow,
    browser: isize,
    action: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    let result = action();
    if let Err(error) = result {
        log_picker(&format!(
            "Operation failed: {error}; identified picker: {}",
            describe_pickers(std::slice::from_ref(picker))
        ));
        return match cancel_picker(picker, browser) {
            Ok(()) => Err(error),
            Err(cleanup) => {
                log_picker(&format!("Cancellation failed: {cleanup}"));
                Err(format!(
                    "{error}; folder picker cleanup also failed: {cleanup}"
                ))
            }
        };
    }
    result
}

fn element_is_in_dialog(
    automation: &UIAutomation,
    element: &UIElement,
    dialog: &UIElement,
) -> Result<bool, String> {
    let walker = automation
        .get_raw_view_walker()
        .map_err(|error| format!("Could not inspect dialog ancestry: {error}"))?;
    let mut current = element.clone();
    for _ in 0..32 {
        if automation
            .compare_elements(&current, dialog)
            .map_err(|error| format!("Could not verify dialog ancestry: {error}"))?
        {
            return Ok(true);
        }
        let Ok(parent) = walker.get_parent(&current) else {
            return Ok(false);
        };
        current = parent;
    }
    Ok(false)
}

fn require_picker_focus(automation: &UIAutomation, dialog: &UIElement) -> Result<(), String> {
    let focused = automation
        .get_focused_element()
        .map_err(|error| format!("Could not read picker focus: {error}"))?;
    if !element_is_in_dialog(automation, &focused, dialog)? {
        return Err("Focus is outside the identified folder picker; action blocked".into());
    }
    Ok(())
}

fn verify_local_version(extension_path: &Path, expected_version: &str) -> Result<(), String> {
    let manifest: crate::model::ChromeManifest = serde_json::from_slice(
        &std::fs::read(extension_path.join("manifest.json"))
            .map_err(|error| format!("Could not read the extension manifest: {error}"))?,
    )
    .map_err(|error| format!("The extension manifest is invalid: {error}"))?;
    if manifest.version != expected_version {
        return Err(format!(
            "Local manifest version {} differs from the requested version {expected_version}",
            manifest.version
        ));
    }
    Ok(())
}

fn chrome_windows(automation: &UIAutomation) -> Vec<UIElement> {
    automation
        .create_matcher()
        .control_type(ControlType::Window)
        .filter_fn(Box::new(|element: &UIElement| {
            Ok(element.get_classname()?.starts_with("Chrome_WidgetWin"))
        }))
        .depth(3)
        .timeout(0)
        .find_all()
        .unwrap_or_default()
}

fn name_matches(value: &str, names: &[&str]) -> bool {
    names
        .iter()
        .any(|candidate| value.eq_ignore_ascii_case(candidate))
}

fn exactly_one<T>(mut candidates: Vec<T>, label: &str) -> Result<T, String> {
    if candidates.len() != 1 {
        return Err(format!(
            "Expected exactly one {label}, found {}; action blocked",
            candidates.len()
        ));
    }
    Ok(candidates.remove(0))
}

fn extension_details_url(id: &str) -> Result<String, String> {
    if id.len() != 32 || !id.bytes().all(|byte| (b'a'..=b'p').contains(&byte)) {
        return Err("Invalid Chrome extension ID; removal blocked".into());
    }
    Ok(format!("chrome://extensions/?id={id}"))
}

fn details_identity_matches(
    url: &str,
    expected_id: &str,
    expected_name: &str,
    ids: &BTreeSet<String>,
    names: &[String],
) -> bool {
    let Ok(url) = url::Url::parse(url) else {
        return false;
    };
    url.scheme() == "chrome"
        && url.host_str() == Some("extensions")
        && matches!(url.path(), "" | "/")
        && url.fragment().is_none()
        && url.username().is_empty()
        && url.password().is_none()
        && url.port().is_none()
        && url.query_pairs().count() == 1
        && url
            .query_pairs()
            .next()
            .is_some_and(|(key, value)| key == "id" && value == expected_id)
        && ids.len() == 1
        && ids.contains(expected_id)
        && names.len() == 1
        && names[0].trim() == expected_name.trim()
}

fn is_details_remove_control(control_type: ControlType, id: &str, name: &str) -> bool {
    matches!(control_type, ControlType::Button | ControlType::Hyperlink)
        && (id == DETAILS_REMOVE_ID || name_matches(name, DETAILS_REMOVE_NAMES))
}

fn extract_chrome_extension_ids(value: &str) -> BTreeSet<String> {
    value
        .split(|character| !('a'..='p').contains(&character))
        .filter(|candidate| candidate.len() == 32)
        .map(str::to_owned)
        .collect()
}

fn invoke(element: &UIElement, label: &str) -> Result<(), String> {
    element
        .get_pattern::<UIInvokePattern>()
        .and_then(|pattern| pattern.invoke())
        .map_err(|error| format!("{label} could not be invoked through UI Automation: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn picker(handle: isize, pid: u32, owners: &[isize]) -> PickerWindow {
        PickerWindow {
            handle,
            pid,
            class: "#32770".into(),
            owners: owners.into(),
        }
    }

    #[test]
    fn owned_picker_selection_does_not_depend_on_its_process_id() {
        for pid in [100, 9001] {
            let candidates = [picker(20, pid, &[10])];
            assert_eq!(
                select_picker_index(&candidates, 10, &BTreeSet::new()).unwrap(),
                Some(0)
            );
        }
    }

    #[test]
    fn another_applications_picker_is_rejected_even_with_the_same_pid() {
        let candidates = [picker(20, 100, &[99]), picker(30, 100, &[10])];
        assert_eq!(
            select_picker_index(&candidates, 10, &BTreeSet::new()).unwrap(),
            Some(1)
        );
        assert_eq!(
            select_picker_index(&candidates[..1], 10, &BTreeSet::new()).unwrap(),
            None
        );
    }

    #[test]
    fn picker_selection_follows_intermediate_owners_but_not_other_chrome_windows() {
        let candidates = [picker(20, 100, &[11, 10]), picker(30, 100, &[12])];
        assert_eq!(
            select_picker_index(&candidates, 10, &BTreeSet::new()).unwrap(),
            Some(0)
        );
        assert_eq!(
            select_picker_index(&candidates, 12, &BTreeSet::new()).unwrap(),
            Some(1)
        );
    }

    #[test]
    fn previously_open_pickers_are_not_adopted_by_a_new_operation() {
        let candidates = [picker(20, 100, &[10]), picker(30, 100, &[10])];
        assert_eq!(
            select_picker_index(&candidates, 10, &BTreeSet::from([20])).unwrap(),
            Some(1)
        );
        assert_eq!(
            select_picker_index(&candidates, 10, &BTreeSet::from([20, 30])).unwrap(),
            None
        );
    }

    #[test]
    fn multiple_owned_pickers_are_ambiguous_instead_of_selecting_the_first() {
        let candidates = [picker(20, 100, &[10]), picker(30, 200, &[10])];
        assert!(select_picker_index(&candidates, 10, &BTreeSet::new())
            .unwrap_err()
            .contains("2 folder pickers"));
    }

    #[test]
    fn chrome_itself_and_non_picker_classes_are_never_selected() {
        let mut candidate = picker(20, 100, &[10]);
        candidate.class = "Chrome_WidgetWin_1".into();
        assert!(!belongs_to_browser(&candidate, 10));
        assert!(!belongs_to_browser(&picker(10, 100, &[10]), 10));
        assert!(!belongs_to_browser(&picker(20, 100, &[0]), 0));
    }

    #[test]
    fn picker_diagnostics_include_native_identity_and_owners() {
        let description = describe_pickers(&[picker(20, 9001, &[11, 10])]);
        assert!(description.contains("hwnd=0x14"));
        assert!(description.contains("class=#32770"));
        assert!(description.contains("pid=9001"));
        assert!(description.contains("owners=[11, 10]"));
    }

    #[test]
    fn ambiguous_or_missing_controls_never_choose_the_first() {
        assert!(exactly_one::<u8>(vec![], "Remove").is_err());
        assert!(exactly_one(vec![1, 2], "Remove").is_err());
        assert_eq!(exactly_one(vec![1], "Remove").unwrap(), 1);
    }

    #[test]
    fn removal_popup_accepts_ok_and_remove_but_never_cancel_or_other_actions() {
        for affirmative in ["OK", "Ok", "Remove", "Remover"] {
            assert!(name_matches(affirmative, REMOVE_CONFIRM_NAMES));
        }
        for other in [
            "Cancel",
            "Cancelar",
            "Close",
            "Remove extension",
            "Remove all",
            "Not OK",
        ] {
            assert!(!name_matches(other, REMOVE_CONFIRM_NAMES));
        }
        assert!(exactly_one::<u8>(vec![], "popup confirmation").is_err());
        assert!(exactly_one(vec![1, 2], "popup confirmation").is_err());
    }

    #[test]
    fn remove_requires_the_exact_details_route_and_visible_identity() {
        let expected = "abcdefghijklmnopabcdefghijklmnop";
        let other = "ponmlkjihgfedcbaponmlkjihgfedcba";
        let url = extension_details_url(expected).unwrap();
        let ids = BTreeSet::from([expected.into()]);
        let names = vec!["Example extension".into()];
        assert!(details_identity_matches(
            &url,
            expected,
            "Example extension",
            &ids,
            &names
        ));
        for wrong_url in [
            "chrome://extensions/".into(),
            extension_details_url(other).unwrap(),
            format!("https://extensions/?id={expected}"),
            format!("chrome://extensions/?id={expected}&id={other}"),
            format!("chrome://extensions/?id={expected}#other"),
        ] {
            assert!(!details_identity_matches(
                &wrong_url,
                expected,
                "Example extension",
                &ids,
                &names
            ));
        }
        for wrong_ids in [
            BTreeSet::new(),
            BTreeSet::from([other.into()]),
            BTreeSet::from([expected.into(), other.into()]),
        ] {
            assert!(!details_identity_matches(
                &url,
                expected,
                "Example extension",
                &wrong_ids,
                &names
            ));
        }
        for wrong_names in [
            vec![],
            vec!["Another extension".into()],
            vec!["Example extension".into(), "Example extension".into()],
        ] {
            assert!(!details_identity_matches(
                &url,
                expected,
                "Example extension",
                &ids,
                &wrong_names
            ));
        }
    }

    #[test]
    fn details_remove_accepts_the_link_and_never_a_list_or_settings_button() {
        assert!(is_details_remove_control(
            ControlType::Hyperlink,
            "icon",
            "Remove extension"
        ));
        assert!(is_details_remove_control(
            ControlType::Button,
            DETAILS_REMOVE_ID,
            "Localized label"
        ));
        assert!(!is_details_remove_control(
            ControlType::Button,
            "removeButton",
            "Remove"
        ));
        assert!(!is_details_remove_control(
            ControlType::Hyperlink,
            "icon",
            "Site settings"
        ));
        assert!(!is_details_remove_control(
            ControlType::Text,
            DETAILS_REMOVE_ID,
            "Remove extension"
        ));
    }

    #[test]
    fn details_url_rejects_invalid_ids_and_id_extraction_keeps_all_candidates() {
        for invalid in ["", "abc", "../../other", "abcdefghijklmnopabcdefghijklmnoq"] {
            assert!(extension_details_url(invalid).is_err());
        }
        let expected = "abcdefghijklmnopabcdefghijklmnop";
        let other = "ponmlkjihgfedcbaponmlkjihgfedcba";
        assert_eq!(
            extract_chrome_extension_ids(&format!("ID: {expected}, ID: {other}")),
            BTreeSet::from([expected.into(), other.into()])
        );
    }
}
