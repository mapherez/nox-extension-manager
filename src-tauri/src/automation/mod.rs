use std::path::Path;

pub trait BrowserAutomationBackend: Send + Sync {
    fn profiles(&self) -> Result<Vec<crate::model::ChromeProfile>, String> {
        registration::profiles()
    }

    fn inventory(
        &self,
        profile: &crate::model::ChromeProfile,
    ) -> Result<Vec<crate::model::ChromeInstallation>, String> {
        registration::inventory(Path::new(&profile.path))
    }

    fn for_profile(
        &self,
        _: &crate::model::ChromeProfile,
    ) -> Result<std::sync::Arc<dyn BrowserAutomationBackend>, String> {
        Err("Chrome automation is unavailable for this profile".into())
    }
    fn preflight(&self) -> Result<(), String>;
    fn registered_id(&self, extension_path: &Path) -> Result<Option<String>, String>;
    fn install(&self, extension_path: &Path, expected_version: &str) -> Result<String, String>;
    fn verify_identity(
        &self,
        extension_path: &Path,
        chrome_extension_id: &str,
    ) -> Result<(), String>;
    fn reload(
        &self,
        extension_path: &Path,
        chrome_extension_id: &str,
        expected_version: &str,
    ) -> Result<(), String>;
    fn remove(&self, extension_path: &Path, chrome_extension_id: &str) -> Result<(), String>;
}

pub mod registration;

#[cfg(target_os = "windows")]
mod windows;

#[cfg(target_os = "windows")]
pub use windows::WindowsChromeAutomation;

#[cfg(target_os = "macos")]
mod macos;

#[cfg(target_os = "macos")]
pub use macos::MacOsChromeAutomation;

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub struct UnsupportedAutomation;

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
impl BrowserAutomationBackend for UnsupportedAutomation {
    fn preflight(&self) -> Result<(), String> {
        Err("Chrome automation is available only on Windows and macOS".into())
    }

    fn install(&self, _: &Path, _: &str) -> Result<String, String> {
        self.preflight()?;
        unreachable!()
    }

    fn registered_id(&self, _: &Path) -> Result<Option<String>, String> {
        Ok(None)
    }

    fn verify_identity(&self, _: &Path, _: &str) -> Result<(), String> {
        self.preflight()
    }

    fn reload(&self, _: &Path, _: &str, _: &str) -> Result<(), String> {
        self.preflight()
    }

    fn remove(&self, _: &Path, _: &str) -> Result<(), String> {
        self.preflight()
    }
}
