//! What Settings shows about browser capture (FP-036).
//!
//! The installer registers the native-messaging host for Chrome, Edge and
//! Firefox and ships an unpacked copy of the extension. This module only
//! reports that state and opens the extension folder; it never writes the
//! registry, so a development build shows "not registered" honestly rather
//! than registering itself.
//!
//! The registry is read through `reg.exe` so this needs no unsafe FFI. `reg`
//! prints type names such as `REG_SZ` untranslated on every Windows locale.

use serde::Serialize;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const HOST: &str = "com.fetchpath.browser";

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserRegistration {
    pub browser: &'static str,
    /// True when the per-user key names a host manifest that exists on disk.
    pub registered: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserSetupStatus {
    /// The unpacked extension shipped with this install, when present.
    pub extension_dir: Option<String>,
    pub browsers: Vec<BrowserRegistration>,
}

pub fn status(resource_dir: Option<&Path>) -> BrowserSetupStatus {
    let extension_dir = resource_dir
        .map(|dir| dir.join("browser-extension"))
        .filter(|dir| dir.join("manifest.json").is_file())
        .map(|dir| dir.display().to_string());
    let browsers = [
        ("Chrome", r"Software\Google\Chrome\NativeMessagingHosts"),
        ("Edge", r"Software\Microsoft\Edge\NativeMessagingHosts"),
        ("Firefox", r"Software\Mozilla\NativeMessagingHosts"),
    ]
    .into_iter()
    .map(|(browser, base)| BrowserRegistration {
        browser,
        registered: registered_manifest(&format!(r"HKCU\{base}\{HOST}")).is_some(),
    })
    .collect();
    BrowserSetupStatus {
        extension_dir,
        browsers,
    }
}

pub fn reveal_extension_folder(resource_dir: Option<&Path>) -> Result<(), String> {
    let dir = status(resource_dir)
        .extension_dir
        .map(PathBuf::from)
        .ok_or("The browser extension is included with installed copies of Fetchpath only.")?;
    Command::new("explorer.exe")
        .arg(&dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("Could not open the folder: {error}"))
}

fn registered_manifest(key: &str) -> Option<PathBuf> {
    use std::os::windows::process::CommandExt;
    let output = Command::new("reg.exe")
        .args(["query", key, "/ve"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        // CREATE_NO_WINDOW: no console flash each time Settings opens.
        .creation_flags(0x0800_0000)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    parse_default_value(&String::from_utf8_lossy(&output.stdout))
        .map(PathBuf::from)
        .filter(|path| path.is_file())
}

/// Pulls the default value's data out of `reg query KEY /ve` output.
fn parse_default_value(output: &str) -> Option<String> {
    output.lines().find_map(|line| {
        let (_, data) = line.split_once("REG_SZ")?;
        let data = data.trim();
        (!data.is_empty()).then(|| data.to_owned())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_value_is_read_from_reg_output() {
        let output = "\r\nHKEY_CURRENT_USER\\Software\\Google\\Chrome\\NativeMessagingHosts\\com.fetchpath.browser\r\n    (Padrão)    REG_SZ    C:\\Users\\a b\\AppData\\Local\\Fetchpath\\com.fetchpath.browser.chromium.json\r\n\r\n";
        assert_eq!(
            parse_default_value(output).as_deref(),
            Some(r"C:\Users\a b\AppData\Local\Fetchpath\com.fetchpath.browser.chromium.json")
        );
    }

    #[test]
    fn an_empty_or_missing_value_is_not_a_registration() {
        assert_eq!(parse_default_value("    (Default)    REG_SZ    \r\n"), None);
        assert_eq!(parse_default_value("ERROR: not found"), None);
    }

    #[test]
    fn a_missing_resource_folder_reports_no_extension() {
        let status = status(Some(Path::new(r"C:\definitely\not\here")));
        assert!(status.extension_dir.is_none());
        assert_eq!(status.browsers.len(), 3);
    }
}
