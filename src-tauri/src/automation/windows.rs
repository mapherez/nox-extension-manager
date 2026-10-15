use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use uiautomation::inputs::Keyboard;
use uiautomation::patterns::{UIInvokePattern, UIValuePattern, UIWindowPattern};
use uiautomation::types::ControlType;
use uiautomation::{UIAutomation, UIElement};

use super::{registration, BrowserAutomationBackend};

const LOAD_UNPACKED_ID: &str = "loadUnpacked";
const FOLDER_PICKER_ADDRESS_BAR_ID: &str = "41477";
const FOLDER_PICKER_CONFIRM_ID: &str = "1";
const REMOVE_BUTTON_ID: &str = "removeButton";
const REMOVE_NAMES: &[&str] = &["Remove", "Remover"];

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

    fn open_extensions_page(&self) -> Result<(UIAutomation, UIElement), String> {
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
            .and_then(|_| keyboard.send_text("chrome://extensions/"))
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
        let (automation, window) = self.open_extensions_page()?;
        let result = action(&automation, &window);
        if let Ok(pattern) = window.get_pattern::<UIWindowPattern>() {
            let _ = pattern.close();
        }
        result
    }

    fn choose_extension_folder(
        automation: &UIAutomation,
        window: &UIElement,
        extension_path: &Path,
    ) -> Result<(), String> {
        let process_id = window.get_process_id().map_err(|error| error.to_string())?;
        let dialog = automation
            .create_matcher()
            .control_type(ControlType::Window)
            .filter_fn(Box::new(move |element: &UIElement| {
                let class = element.get_classname()?;
                Ok(element.get_process_id()? == process_id
                    && (class == "#32770" || class == "DirectUIHWND"))
            }))
            .depth(4)
            .timeout(8_000)
            .find_first()
            .map_err(|_| "The Windows folder picker did not appear".to_string())?;

        let path_text = extension_path.to_string_lossy().to_string();
        dialog
            .set_focus()
            .map_err(|error| format!("The folder picker could not receive focus: {error}"))?;
        let keyboard = Keyboard::new().interval(0);
        keyboard.send_keys("{ctrl}(l)").map_err(|error| {
            format!("The folder picker's address bar could not be opened: {error}")
        })?;
        thread::sleep(Duration::from_millis(100));

        let edit = automation
            .get_focused_element()
            .ok()
            .filter(|element| {
                element.get_control_type().ok() == Some(ControlType::Edit)
                    && element.get_process_id().ok() == Some(process_id)
            })
            .or_else(|| {
                automation
                    .create_matcher()
                    .from(dialog.clone())
                    .control_type(ControlType::Edit)
                    .filter_fn(Box::new(|element: &UIElement| {
                        Ok(element.get_automation_id()? == FOLDER_PICKER_ADDRESS_BAR_ID)
                    }))
                    .depth(12)
                    .timeout(1_000)
                    .find_first()
                    .ok()
            })
            .ok_or_else(|| {
                "The folder picker's address field was not exposed by Windows".to_string()
            })?;
        edit.get_pattern::<UIValuePattern>()
            .and_then(|pattern| pattern.set_value(&path_text))
            .map_err(|error| format!("The extension folder path could not be entered: {error}"))?;
        keyboard
            .send_keys("{enter}")
            .map_err(|error| format!("The extension folder could not be opened: {error}"))?;
        thread::sleep(Duration::from_millis(500));

        let confirm = automation
            .create_matcher()
            .from(dialog)
            .control_type(ControlType::Button)
            .filter_fn(Box::new(|element: &UIElement| {
                Ok(element.get_automation_id()? == FOLDER_PICKER_CONFIRM_ID)
            }))
            .depth(12)
            .timeout(4_000)
            .find_first()
            .map_err(|_| {
                "The folder picker's confirm button was not exposed by Windows".to_string()
            })?;
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
            invoke(&load, "Load unpacked")?;
            Self::choose_extension_folder(automation, window, extension_path)?;

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
    }

    fn find_extension_card(
        automation: &UIAutomation,
        window: &UIElement,
        chrome_extension_id: &str,
    ) -> Result<UIElement, String> {
        // Prefer Chromium's exact extensions-item host. A flattened accessibility
        // tree is usable only if a bounded ancestor contains exactly this ID
        // and exactly one Remove control. Never choose controls by distance.
        let expected_id = chrome_extension_id.to_string();
        let cards = automation
            .create_matcher()
            .from(window.clone())
            .filter_fn(Box::new(move |element: &UIElement| {
                Ok(element.get_automation_id()? == expected_id)
            }))
            .depth(30)
            .timeout(0)
            .find_all()
            .unwrap_or_default();
        if !cards.is_empty() {
            return exactly_one(cards, "extension card");
        }

        let expected = chrome_extension_id.to_string();
        let labels = automation
            .create_matcher()
            .from(window.clone())
            .filter_fn(Box::new(move |element: &UIElement| {
                Ok(element
                    .get_name()
                    .ok()
                    .and_then(|name| extract_chrome_extension_id(&name))
                    .as_deref()
                    == Some(&expected))
            }))
            .depth(30)
            .timeout(5_000)
            .find_all()
            .map_err(|_| {
                format!("Chrome did not expose ID {chrome_extension_id}; action blocked")
            })?;
        let mut current = exactly_one(labels, "extension ID label")?;
        let walker = automation
            .get_control_view_walker()
            .map_err(|error| error.to_string())?;
        for _ in 0..10 {
            let labels = automation
                .create_matcher()
                .from(current.clone())
                .filter_fn(Box::new(|element: &UIElement| {
                    Ok(element
                        .get_name()
                        .ok()
                        .and_then(|name| extract_chrome_extension_id(&name))
                        .is_some())
                }))
                .depth(30)
                .timeout(0)
                .find_all()
                .map_err(|error| error.to_string())?;
            let ids: BTreeSet<_> = labels
                .into_iter()
                .filter_map(|element| {
                    element
                        .get_name()
                        .ok()
                        .and_then(|name| extract_chrome_extension_id(&name))
                })
                .collect();
            let remove_count = automation
                .create_matcher()
                .from(current.clone())
                .control_type(ControlType::Button)
                .filter_fn(Box::new(|element: &UIElement| {
                    Ok(element.get_automation_id()? == REMOVE_BUTTON_ID)
                }))
                .depth(30)
                .timeout(0)
                .find_all()
                .ok()
                .map(|buttons| buttons.len())
                .unwrap_or(0);
            if card_scope_is_unique(chrome_extension_id, &ids, remove_count) {
                return Ok(current);
            }
            if ids.len() > 1 || remove_count > 1 {
                break;
            }
            current = walker
                .get_parent(&current)
                .map_err(|error| error.to_string())?;
        }
        Err(format!(
            "Chrome did not expose an isolated card for {chrome_extension_id}; action blocked"
        ))
    }

    fn find_extension_button(
        automation: &UIAutomation,
        window: &UIElement,
        chrome_extension_id: &str,
        automation_ids: &'static [&'static str],
        label: &str,
    ) -> Result<UIElement, String> {
        let card = Self::find_extension_card(automation, window, chrome_extension_id)?;
        let buttons = automation
            .create_matcher()
            .from(card)
            .control_type(ControlType::Button)
            .filter_fn(Box::new(move |element: &UIElement| {
                Ok(automation_ids.contains(&element.get_automation_id()?.as_str()))
            }))
            .depth(12)
            .timeout(3_000)
            .find_all()
            .map_err(|_| format!("{label} was not found for {chrome_extension_id}"))?;
        exactly_one(buttons, label)
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
        self.with_extensions_page(|automation, window| {
            self.verify_identity(extension_path, chrome_extension_id)?;
            let remove = Self::find_extension_button(
                automation,
                window,
                chrome_extension_id,
                &[REMOVE_BUTTON_ID],
                "Remove",
            )
            .map_err(|_| format!("Remove was not found for {chrome_extension_id}"))?;
            invoke(&remove, "Remove")?;
            let extension_name = manifest.name.clone();
            let dialogs = automation
                .create_matcher()
                .from(window.clone())
                .control_type(ControlType::Window)
                .filter_fn(Box::new(move |element: &UIElement| {
                    Ok(element.get_name()?.contains(&extension_name))
                }))
                .depth(12)
                .timeout(5_000)
                .find_all()
                .map_err(|_| {
                    "Chrome did not expose a removal dialog naming this extension; removal blocked"
                        .to_string()
                })?;
            let dialog = exactly_one(dialogs, "removal dialog naming this extension")?;
            let buttons = automation
                .create_matcher()
                .from(dialog)
                .control_type(ControlType::Button)
                .filter_fn(Box::new(|element: &UIElement| {
                    Ok(name_matches(&element.get_name()?, REMOVE_NAMES))
                }))
                .depth(8)
                .timeout(3_000)
                .find_all()
                .map_err(|_| {
                    "Chrome's removal confirmation was not found in the target dialog".to_string()
                })?;
            let confirm = exactly_one(buttons, "removal confirmation")?;
            self.verify_identity(extension_path, chrome_extension_id)?;
            invoke(&confirm, "Confirm Remove")?;
            let deadline = Instant::now() + Duration::from_secs(15);
            while registration::registered_id(&self.profile_directory()?, extension_path)?.is_some()
            {
                if Instant::now() >= deadline {
                    return Err("Chrome still reports the extension after Remove".into());
                }
                thread::sleep(Duration::from_millis(100));
            }
            Ok(())
        })
    }
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

fn card_scope_is_unique(expected: &str, ids: &BTreeSet<String>, remove_count: usize) -> bool {
    ids.len() == 1 && ids.contains(expected) && remove_count == 1
}

fn extract_chrome_extension_id(value: &str) -> Option<String> {
    value
        .split(|character| !('a'..='p').contains(&character))
        .find(|candidate| candidate.len() == 32)
        .map(str::to_owned)
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

    #[test]
    fn ambiguous_or_missing_controls_never_choose_the_first() {
        assert!(exactly_one::<u8>(vec![], "Remove").is_err());
        assert!(exactly_one(vec![1, 2], "Remove").is_err());
        assert_eq!(exactly_one(vec![1], "Remove").unwrap(), 1);
    }

    #[test]
    fn shared_ancestors_and_neighboring_controls_are_never_extension_cards() {
        let expected = "abcdefghijklmnopabcdefghijklmnop";
        let other = "ponmlkjihgfedcbaponmlkjihgfedcba";
        assert!(card_scope_is_unique(
            expected,
            &BTreeSet::from([expected.into()]),
            1
        ));
        assert!(!card_scope_is_unique(
            expected,
            &BTreeSet::from([expected.into(), other.into()]),
            1
        ));
        assert!(!card_scope_is_unique(
            expected,
            &BTreeSet::from([expected.into()]),
            2
        ));
        assert!(!card_scope_is_unique(
            expected,
            &BTreeSet::from([other.into()]),
            1
        ));
        assert!(!card_scope_is_unique(expected, &BTreeSet::new(), 1));
    }
}
