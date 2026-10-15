use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use accessibility::action::AXUIElementActions;
use accessibility::attribute::AXUIElementAttributes;
use accessibility::{AXAttribute, AXUIElement};
use accessibility_sys::{
    error_string, kAXCloseButtonAttribute, kAXErrorSuccess, AXUIElementPostKeyboardEvent,
};
use core_foundation::base::TCFType;
use core_foundation::string::CFString;
use macos_accessibility_client::accessibility::application_is_trusted_with_prompt;

use super::{registration, BrowserAutomationBackend};

const SYSTEM_CHROME_APP: &str = "/Applications/Google Chrome.app";
const CHROME_EXECUTABLE: &str = "Contents/MacOS/Google Chrome";
const CHROME_BUNDLE_ID: &str = "com.google.Chrome";
const LOAD_UNPACKED_ID: &str = "loadUnpacked";
const OPEN_PANEL_IDS: &[&str] = &["open-panel", "save-panel"];
const GO_TO_FOLDER_FIELD_ID: &str = "PathTextField";
const MAX_AX_DEPTH: usize = 40;
const MAX_AX_ELEMENTS: usize = 8_000;

// macOS virtual key codes. Events are posted to Chrome's AX application
// element, not through mouse movement or the clipboard.
const KEY_G: u16 = 5;
const KEY_RETURN: u16 = 36;
const KEY_ESCAPE: u16 = 53;
const KEY_COMMAND: u16 = 55;
const KEY_SHIFT: u16 = 56;

pub struct MacOsChromeAutomation;

impl MacOsChromeAutomation {
    pub fn new() -> Self {
        Self
    }

    fn chrome_app_path(&self) -> Result<PathBuf, String> {
        let mut candidates = vec![PathBuf::from(SYSTEM_CHROME_APP)];
        if let Some(home) = dirs::home_dir() {
            candidates.push(home.join("Applications/Google Chrome.app"));
        }

        candidates
            .into_iter()
            .find(|candidate| candidate.join(CHROME_EXECUTABLE).is_file())
            .ok_or_else(|| {
                "Google Chrome was not found in /Applications or ~/Applications".to_string()
            })
    }

    fn with_extensions_page<T>(
        &self,
        action: impl FnOnce(&AXUIElement, &AXUIElement) -> Result<T, String>,
    ) -> Result<T, String> {
        let chrome = self.chrome_app_path()?.join(CHROME_EXECUTABLE);
        let existing_windows = AXUIElement::application_with_bundle(CHROME_BUNDLE_ID)
            .ok()
            .and_then(|app| app.windows().ok())
            .map(|windows| windows.iter().map(|window| (*window).clone()).collect())
            .unwrap_or_default();

        Command::new(chrome)
            .args([
                "--profile-directory=Default",
                "--new-window",
                "chrome://extensions/",
            ])
            .spawn()
            .map_err(|error| format!("Could not open Google Chrome: {error}"))?;

        let app =
            AXUIElement::application_with_bundle_timeout(CHROME_BUNDLE_ID, Duration::from_secs(10))
                .map_err(|error| {
                    format!("Chrome did not expose its accessibility tree: {error}")
                })?;
        let _ = app.set_messaging_timeout(3.0);
        app.set_frontmost(true)
            .map_err(|error| format!("Could not bring Chrome to the front: {error}"))?;

        let window = wait_for_extensions_window(&app, &existing_windows)?;
        let _ = window.raise();

        let result = action(&app, &window);
        if open_panel(&app).is_some() {
            let _ = post_key(&app, KEY_ESCAPE, true);
            let _ = post_key(&app, KEY_ESCAPE, false);
            thread::sleep(Duration::from_millis(150));
        }
        close_window(&window);
        result
    }

    fn choose_extension_folder(app: &AXUIElement, extension_path: &Path) -> Result<(), String> {
        let canonical_path = extension_path
            .canonicalize()
            .map_err(|error| format!("Could not resolve the extension folder: {error}"))?;
        let path = canonical_path
            .to_str()
            .ok_or("The extension folder path is not valid Unicode")?;

        let panel = wait_for_open_panel(app)?;
        post_go_to_folder_shortcut(app)?;

        let deadline = Instant::now() + Duration::from_secs(4);
        let path_field = loop {
            if let Some(field) = find_element(&panel, &|element| {
                attribute_string(element.identifier())
                    .as_deref()
                    .is_some_and(|identifier| identifier == GO_TO_FOLDER_FIELD_ID)
            }) {
                break field;
            }
            if Instant::now() >= deadline {
                return Err("The folder path field was not exposed by macOS".into());
            }
            thread::sleep(Duration::from_millis(100));
        };

        path_field
            .set_value(CFString::new(path).as_CFType())
            .map_err(|error| format!("The extension folder path could not be entered: {error}"))?;
        post_return(app)?;

        // The first Return closes the Go to Folder sheet and navigates the
        // open panel. Only confirm while that panel is still focused, so no
        // key event can leak into the Extensions page.
        let navigation_deadline = Instant::now() + Duration::from_secs(3);
        while find_element(&panel, &|element| {
            attribute_string(element.identifier())
                .as_deref()
                .is_some_and(|identifier| identifier == GO_TO_FOLDER_FIELD_ID)
        })
        .is_some()
        {
            if Instant::now() >= navigation_deadline {
                return Err("macOS did not finish navigating to the extension folder".into());
            }
            thread::sleep(Duration::from_millis(75));
        }

        if open_panel(app).is_some() {
            post_return(app)?;
        }

        let close_deadline = Instant::now() + Duration::from_secs(5);
        while open_panel(app).is_some() {
            if Instant::now() >= close_deadline {
                return Err("The folder picker did not close after selecting the extension".into());
            }
            thread::sleep(Duration::from_millis(100));
        }
        Ok(())
    }

    fn verify_version(
        window: &AXUIElement,
        chrome_extension_id: &str,
        expected_version: &str,
    ) -> Result<(), String> {
        let id_node = find_element(window, &|element| {
            element_strings(element).iter().any(|value| {
                extract_chrome_extension_id(value).as_deref() == Some(chrome_extension_id)
            })
        })
        .ok_or_else(|| {
            format!("Chrome extension {chrome_extension_id} was not found on the Extensions page")
        })?;

        let mut current = id_node;
        for _ in 0..10 {
            if tree_contains_text(&current, expected_version) {
                return Ok(());
            }
            match current.parent() {
                Ok(parent) => current = parent,
                Err(_) => break,
            }
        }

        Err(format!(
            "Chrome found {chrome_extension_id}, but did not confirm version {expected_version}"
        ))
    }

    fn unavailable_action(action: &str) -> String {
        format!(
            "macOS accessibility access is ready, but {action} automation is not implemented yet"
        )
    }
}

impl BrowserAutomationBackend for MacOsChromeAutomation {
    fn preflight(&self) -> Result<(), String> {
        self.chrome_app_path()?;
        if application_is_trusted_with_prompt() {
            return Ok(());
        }

        Err(
            "NoX needs Accessibility access to control Chrome. Enable NoX Extension Manager in System Settings > Privacy & Security > Accessibility, then retry"
                .into(),
        )
    }

    fn install(&self, extension_path: &Path, expected_version: &str) -> Result<String, String> {
        self.preflight()?;
        if !extension_path.join("manifest.json").is_file() {
            return Err("The prepared extension does not contain manifest.json".into());
        }

        self.with_extensions_page(|app, window| {
            let load_unpacked = find_element(window, &is_load_unpacked_button)
                .ok_or("Load unpacked was not exposed through Chrome accessibility")?;
            load_unpacked
                .press()
                .map_err(|error| format!("Load unpacked could not be invoked: {error}"))?;

            Self::choose_extension_folder(app, extension_path)?;

            let deadline = Instant::now() + Duration::from_secs(10);
            let chrome_id = loop {
                let profile = dirs::home_dir()
                    .ok_or("The home directory could not be resolved")?
                    .join("Library/Application Support/Google/Chrome/Default");
                if let Some(id) = registration::registered_id(&profile, extension_path)? {
                    break id;
                }
                if Instant::now() >= deadline {
                    return Err(
                        "Chrome did not report a newly installed extension after Load unpacked"
                            .into(),
                    );
                }
                thread::sleep(Duration::from_millis(150));
            };

            Self::verify_version(window, &chrome_id, expected_version)?;
            Ok(chrome_id)
        })
    }

    fn verify_identity(
        &self,
        extension_path: &Path,
        chrome_extension_id: &str,
    ) -> Result<(), String> {
        let profile = dirs::home_dir()
            .ok_or("The home directory could not be resolved")?
            .join("Library/Application Support/Google/Chrome/Default");
        registration::verify(&profile, extension_path, chrome_extension_id)
    }

    fn reload(&self, _: &Path, _: &str, _: &str) -> Result<(), String> {
        self.preflight()?;
        Err(Self::unavailable_action("extension reload"))
    }

    fn remove(&self, _: &Path, _: &str) -> Result<(), String> {
        self.preflight()?;
        Err(Self::unavailable_action("extension removal"))
    }
}

fn wait_for_extensions_window(
    app: &AXUIElement,
    existing_windows: &[AXUIElement],
) -> Result<AXUIElement, String> {
    let deadline = Instant::now() + Duration::from_secs(12);
    loop {
        if let Ok(windows) = app.windows() {
            for window in windows.iter() {
                if existing_windows.iter().any(|existing| existing == &*window) {
                    continue;
                }
                if find_element(&window, &is_load_unpacked_button).is_some() {
                    return Ok((*window).clone());
                }
            }
        }

        if Instant::now() >= deadline {
            return Err(
                "Chrome opened, but chrome://extensions was not exposed through accessibility"
                    .into(),
            );
        }
        thread::sleep(Duration::from_millis(200));
    }
}

fn is_load_unpacked_button(element: &AXUIElement) -> bool {
    if attribute_string(element.identifier()).as_deref() == Some(LOAD_UNPACKED_ID) {
        return true;
    }

    attribute_string(element.role()).as_deref() == Some("AXButton")
        && element_strings(element)
            .iter()
            .any(|value| value.eq_ignore_ascii_case("Load unpacked"))
}

fn open_panel(app: &AXUIElement) -> Option<AXUIElement> {
    let focused = app.focused_window().ok()?;
    let is_panel = |element: &AXUIElement| {
        attribute_string(element.identifier())
            .as_deref()
            .is_some_and(|identifier| OPEN_PANEL_IDS.contains(&identifier))
    };

    if is_panel(&focused) {
        Some(focused)
    } else {
        find_element(&focused, &is_panel)
    }
}

fn wait_for_open_panel(app: &AXUIElement) -> Result<AXUIElement, String> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(panel) = open_panel(app) {
            return Ok(panel);
        }
        if Instant::now() >= deadline {
            return Err("The macOS folder picker was not exposed through accessibility".into());
        }
        thread::sleep(Duration::from_millis(100));
    }
}

fn extension_ids(window: &AXUIElement) -> BTreeSet<String> {
    let mut ids = BTreeSet::new();
    visit_elements(window, &mut |element| {
        for value in element_strings(element) {
            if let Some(id) = extract_chrome_extension_id(&value) {
                ids.insert(id);
            }
        }
    });
    ids
}

fn tree_contains_text(root: &AXUIElement, expected: &str) -> bool {
    let mut found = false;
    visit_elements(root, &mut |element| {
        if !found
            && element_strings(element)
                .iter()
                .any(|value| value.trim() == expected || value.contains(expected))
        {
            found = true;
        }
    });
    found
}

fn element_strings(element: &AXUIElement) -> Vec<String> {
    let mut values = Vec::new();
    for value in [
        attribute_string(element.identifier()),
        attribute_string(element.title()),
        attribute_string(element.description()),
        attribute_string(element.label_value()),
        attribute_string(element.value_description()),
    ]
    .into_iter()
    .flatten()
    {
        if !value.is_empty() {
            values.push(value);
        }
    }
    if let Ok(value) = element.value() {
        if let Some(string) = value.downcast::<CFString>() {
            let string = string.to_string();
            if !string.is_empty() {
                values.push(string);
            }
        }
    }
    values
}

fn attribute_string(value: Result<CFString, accessibility::Error>) -> Option<String> {
    value.ok().map(|value| value.to_string())
}

fn find_element(
    root: &AXUIElement,
    predicate: &impl Fn(&AXUIElement) -> bool,
) -> Option<AXUIElement> {
    let mut visited = 0;
    find_element_inner(root, predicate, 0, &mut visited)
}

fn find_element_inner(
    element: &AXUIElement,
    predicate: &impl Fn(&AXUIElement) -> bool,
    depth: usize,
    visited: &mut usize,
) -> Option<AXUIElement> {
    if depth > MAX_AX_DEPTH || *visited >= MAX_AX_ELEMENTS {
        return None;
    }
    *visited += 1;
    if predicate(element) {
        return Some(element.clone());
    }
    let children = element.children().ok()?;
    for child in children.iter() {
        if let Some(found) = find_element_inner(&child, predicate, depth + 1, visited) {
            return Some(found);
        }
    }
    None
}

fn visit_elements(root: &AXUIElement, visitor: &mut impl FnMut(&AXUIElement)) {
    let mut visited = 0;
    visit_elements_inner(root, visitor, 0, &mut visited);
}

fn visit_elements_inner(
    element: &AXUIElement,
    visitor: &mut impl FnMut(&AXUIElement),
    depth: usize,
    visited: &mut usize,
) {
    if depth > MAX_AX_DEPTH || *visited >= MAX_AX_ELEMENTS {
        return;
    }
    *visited += 1;
    visitor(element);
    if let Ok(children) = element.children() {
        for child in children.iter() {
            visit_elements_inner(&child, visitor, depth + 1, visited);
        }
    }
}

fn post_go_to_folder_shortcut(app: &AXUIElement) -> Result<(), String> {
    post_key(app, KEY_COMMAND, true)?;
    if let Err(error) = post_key(app, KEY_SHIFT, true)
        .and_then(|_| post_key(app, KEY_G, true))
        .and_then(|_| post_key(app, KEY_G, false))
    {
        let _ = post_key(app, KEY_SHIFT, false);
        let _ = post_key(app, KEY_COMMAND, false);
        return Err(error);
    }
    post_key(app, KEY_SHIFT, false)?;
    post_key(app, KEY_COMMAND, false)
}

fn post_return(app: &AXUIElement) -> Result<(), String> {
    post_key(app, KEY_RETURN, true)?;
    post_key(app, KEY_RETURN, false)
}

fn post_key(app: &AXUIElement, key: u16, down: bool) -> Result<(), String> {
    let result = unsafe { AXUIElementPostKeyboardEvent(app.as_concrete_TypeRef(), 0, key, down) };
    if result == kAXErrorSuccess {
        Ok(())
    } else {
        Err(format!(
            "macOS could not send a key event to Chrome: {} ({result})",
            error_string(result)
        ))
    }
}

fn close_window(window: &AXUIElement) {
    let close_attribute = AXAttribute::new(&CFString::from_static_string(kAXCloseButtonAttribute));
    if let Ok(value) = window.attribute(&close_attribute) {
        if let Some(button) = value.downcast::<AXUIElement>() {
            let _ = button.press();
        }
    }
}

fn extract_chrome_extension_id(value: &str) -> Option<String> {
    value
        .split(|character| !('a'..='p').contains(&character))
        .find(|candidate| candidate.len() == 32)
        .map(str::to_string)
}
