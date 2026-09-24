//! The opt-in sign-in start: a value under the current user's Run key.
//!
//! Changed only when a person changes the setting, and added (never removed)
//! at engine start, so an engine started with another data folder, as tests
//! do, cannot undo a person's choice.

use windows_sys::Win32::System::Registry::{
    HKEY_CURRENT_USER, REG_SZ, RegDeleteKeyValueW, RegSetKeyValueW,
};

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const VALUE: &str = "Fetchpath engine";

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// The command Windows runs at sign-in.
pub fn command_line(exe: &std::path::Path) -> String {
    format!("\"{}\" engine", exe.display())
}

/// Adds or removes the Run value. Failures are reported, not fatal: the
/// engine works without it.
pub fn apply(enabled: bool) {
    let key = wide(RUN_KEY);
    let value = wide(VALUE);
    let status = if enabled {
        let Ok(exe) = std::env::current_exe() else {
            return;
        };
        let data = wide(&command_line(&exe));
        // SAFETY: every pointer is to a NUL-terminated UTF-16 buffer that
        // outlives the call; the length counts the data's bytes.
        unsafe {
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                key.as_ptr(),
                value.as_ptr(),
                REG_SZ,
                data.as_ptr().cast(),
                (data.len() * 2) as u32,
            )
        }
    } else {
        // SAFETY: as above.
        unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, key.as_ptr(), value.as_ptr()) }
    };
    // 2 is "not found" when removing a value that is already absent.
    if status != 0 && !(status == 2 && !enabled) {
        eprintln!("fetchpath engine: the sign-in start could not be changed (error {status}).");
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_sign_in_command_quotes_the_path_and_starts_the_engine() {
        assert_eq!(
            super::command_line(std::path::Path::new(
                r"C:\Program Files\Fetchpath\fetchpath.exe"
            )),
            r#""C:\Program Files\Fetchpath\fetchpath.exe" engine"#
        );
    }
}
