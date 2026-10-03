//! What Fetchpath setup installed (FP-099), for clients to show read-only.
//!
//! Setup records the selection with its uninstall entry: `FetchpathInstallType`
//! (`full` or `custom`), `FetchpathComponents` (canonical sorted names such as
//! `browser,cli,core,desktop,mcp,torrent`) and `FetchpathComponentsSchema`. An
//! install from before components existed has none of them and has everything
//! this build has, which is `full`. Nothing here changes the selection: a
//! person changes it by running Fetchpath setup again.
//!
//! The registry is read through `reg.exe`, as the desktop's browser setup does,
//! so this needs no unsafe FFI.

/// The components a selection can name, in the order setup lists them to people.
const LABELS: [(&str, &str); 6] = [
    ("core", "Core"),
    ("desktop", "Desktop app"),
    ("cli", "Terminal"),
    ("mcp", "AI agents (MCP)"),
    ("browser", "Browser integration"),
    ("torrent", "Torrent helper"),
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstalledSelection {
    pub version: String,
    /// `full` or `custom`.
    pub install_type: String,
    /// Canonical names, sorted.
    pub components: Vec<String>,
    /// False when the stored type or list could not be read: the components
    /// are then unknown and the summary says to run setup.
    pub readable: bool,
}

impl InstalledSelection {
    /// Names for a person, in setup's order. Unknown names are left out.
    pub fn labels(&self) -> Vec<&'static str> {
        LABELS
            .iter()
            .filter(|(name, _)| self.components.iter().any(|c| c == name))
            .map(|(_, label)| *label)
            .collect()
    }

    /// One sentence for `fetchpath engine status` and Settings.
    pub fn summary(&self) -> String {
        if !self.readable {
            return format!(
                "Installed: Fetchpath {}, components unknown. Run Fetchpath setup again to see and change them.",
                self.version
            );
        }
        format!(
            "Installed: Fetchpath {} ({}): {}. To change the components, download Fetchpath setup again and choose Change components.",
            self.version,
            if self.install_type == "full" {
                "Full"
            } else {
                "Custom"
            },
            self.labels().join(", ")
        )
    }
}

/// Reads `reg query` output for the uninstall key. `None` when it names no
/// Fetchpath version, so a copy that setup did not install shows nothing.
pub fn parse_selection(output: &str) -> Option<InstalledSelection> {
    let value = |name: &str| -> Option<String> {
        output.lines().find_map(|line| {
            let line = line.trim();
            let rest = line.strip_prefix(name)?;
            let data = rest.trim_start().strip_prefix("REG_SZ")?.trim();
            (!data.is_empty()).then(|| data.to_owned())
        })
    };
    let version = value("DisplayVersion")?;
    let stored_type = value("FetchpathInstallType").map(|t| t.to_ascii_lowercase());
    let stored_list = value("FetchpathComponents").map(|l| l.to_ascii_lowercase());
    let known = |list: &str| -> Vec<String> {
        let mut names: Vec<String> = list
            .split(',')
            .map(|part| part.trim().to_ascii_lowercase())
            .filter(|name| LABELS.iter().any(|(known, _)| known == name))
            .collect();
        names.sort();
        names.dedup();
        names
    };
    let all = || {
        LABELS
            .iter()
            .map(|(name, _)| (*name).to_owned())
            .collect::<Vec<_>>()
    };
    let mut sorted_all = all();
    sorted_all.sort();
    let (install_type, components, readable) = match (stored_type.as_deref(), stored_list) {
        (None, None) => ("full".to_owned(), sorted_all, true),
        (Some("full"), _) => ("full".to_owned(), sorted_all, true),
        (Some("custom"), Some(list)) => {
            let names = known(&list);
            let ok = names.iter().any(|n| n == "desktop" || n == "cli");
            ("custom".to_owned(), names, ok)
        }
        _ => ("custom".to_owned(), Vec::new(), false),
    };
    Some(InstalledSelection {
        version,
        install_type,
        components,
        readable,
    })
}

/// The installed selection, from the per-user uninstall entry setup wrote.
#[cfg(windows)]
pub fn installed_selection() -> Option<InstalledSelection> {
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};
    let reg = std::path::PathBuf::from(std::env::var_os("SystemRoot")?)
        .join("System32")
        .join("reg.exe");
    let output = Command::new(reg)
        .args([
            "query",
            r"HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\Fetchpath",
        ])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        // CREATE_NO_WINDOW: no console flash from a window or a terminal command.
        .creation_flags(0x0800_0000)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    parse_selection(&String::from_utf8_lossy(&output.stdout))
}

#[cfg(not(windows))]
pub fn installed_selection() -> Option<InstalledSelection> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str =
        "HKEY_CURRENT_USER\\Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\Fetchpath\r\n";

    #[test]
    fn a_custom_selection_is_read_from_the_stored_list() {
        let output = format!(
            "{KEY}    DisplayVersion    REG_SZ    0.1.0\r\n    FetchpathInstallType    REG_SZ    custom\r\n    FetchpathComponents    REG_SZ    cli,core\r\n    FetchpathComponentsSchema    REG_DWORD    0x1\r\n"
        );
        let selection = parse_selection(&output).unwrap();
        assert_eq!(selection.install_type, "custom");
        assert_eq!(selection.components, ["cli", "core"]);
        assert_eq!(selection.labels(), ["Core", "Terminal"]);
        assert!(
            selection
                .summary()
                .contains("download Fetchpath setup again")
        );
    }

    #[test]
    fn an_install_from_before_components_is_full() {
        let selection =
            parse_selection(&format!("{KEY}    DisplayVersion    REG_SZ    0.1.0\r\n")).unwrap();
        assert_eq!(selection.install_type, "full");
        assert_eq!(selection.labels().len(), 6);
    }

    #[test]
    fn full_means_everything_this_build_has_whatever_the_list_says() {
        let output = format!(
            "{KEY}    DisplayVersion    REG_SZ    0.1.0\r\n    FetchpathInstallType    REG_SZ    full\r\n    FetchpathComponents    REG_SZ    core\r\n"
        );
        assert_eq!(parse_selection(&output).unwrap().labels().len(), 6);
    }

    #[test]
    fn values_are_lowercased_and_an_unusable_selection_is_unknown_not_guessed() {
        let upper = format!(
            "{KEY}    DisplayVersion    REG_SZ    0.1.0
    FetchpathInstallType    REG_SZ    CUSTOM
    FetchpathComponents    REG_SZ    CLI,Core
"
        );
        assert_eq!(parse_selection(&upper).unwrap().components, ["cli", "core"]);
        for bad in [
            "    FetchpathInstallType    REG_SZ    banana
",
            "    FetchpathInstallType    REG_SZ    custom
",
            "    FetchpathInstallType    REG_SZ    custom
    FetchpathComponents    REG_SZ    mcp,core
",
        ] {
            let selection = parse_selection(&format!(
                "{KEY}    DisplayVersion    REG_SZ    0.1.0
{bad}"
            ))
            .unwrap();
            assert!(!selection.readable, "{bad}");
            assert!(selection.summary().contains("components unknown"));
        }
    }

    #[test]
    fn unknown_names_are_ignored_and_no_version_means_not_installed() {
        let output = format!(
            "{KEY}    DisplayVersion    REG_SZ    0.1.0\r\n    FetchpathInstallType    REG_SZ    custom\r\n    FetchpathComponents    REG_SZ    web,core,desktop\r\n"
        );
        assert_eq!(
            parse_selection(&output).unwrap().components,
            ["core", "desktop"]
        );
        assert_eq!(parse_selection("ERROR: not found"), None);
    }
}
