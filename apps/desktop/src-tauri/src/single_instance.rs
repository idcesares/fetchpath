//! One Fetchpath window per Windows user account.
//!
//! The queue belongs to the engine, which holds its own single-owner lock; this
//! guard only keeps a second window (and a second tray icon) from opening.
//! It is a deny-sharing handle on `desktop-window.lock` in the same folder:
//! whoever opens it first keeps it for the life of the process.
//!
//! It fails open. Only a sharing or locking violation means "another window is
//! open"; any other error (an unwritable directory, a missing `%APPDATA%`)
//! lets the application start, because two windows are only untidy: both are
//! clients of the one engine.

use std::fs::{File, OpenOptions};
use std::os::windows::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// `FILE_SHARE_NONE`: no other process may open this file at all.
const NO_SHARING: u32 = 0;
const ERROR_SHARING_VIOLATION: i32 = 32;
const ERROR_LOCK_VIOLATION: i32 = 33;
const SW_SHOW: i32 = 5;
const SW_RESTORE: i32 = 9;
/// The window class tao registers for a Tauri window. Matching on it as well
/// as the title avoids activating some unrelated window that happens to be
/// called "Fetchpath" — an Explorer window on a folder of that name, say.
const WINDOW_CLASS: &str = "Tauri Window";

/// Held for the lifetime of the process; dropping it would release the claim.
static HELD: OnceLock<File> = OnceLock::new();

#[link(name = "user32")]
unsafe extern "system" {
    fn FindWindowW(class_name: *const u16, window_name: *const u16) -> *mut core::ffi::c_void;
    fn IsIconic(window: *mut core::ffi::c_void) -> i32;
    fn ShowWindow(window: *mut core::ffi::c_void, command: i32) -> i32;
    fn SetForegroundWindow(window: *mut core::ffi::c_void) -> i32;
}

/// Not the engine's lock: the engine is the queue's owner.
pub fn window_lock_path() -> Option<PathBuf> {
    let home = fetchpath_protocol::launch::EngineHome::from_env().ok()?;
    Some(home.dir().join("desktop-window.lock"))
}

/// `true` when this process may proceed as the single instance.
pub fn claim(path: &Path) -> bool {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .share_mode(NO_SHARING)
        .open(path)
    {
        Ok(file) => {
            let _ = HELD.set(file);
            true
        }
        Err(error) => !matches!(
            error.raw_os_error(),
            Some(ERROR_SHARING_VIOLATION) | Some(ERROR_LOCK_VIOLATION)
        ),
    }
}

fn wide(value: &str) -> Vec<u16> {
    let mut buffer: Vec<u16> = value.encode_utf16().collect();
    buffer.push(0);
    buffer
}

/// Brings the already-running window forward, including from the tray, so a
/// second launch looks like reopening Fetchpath rather than doing nothing.
pub fn activate_running_window(title: &str) {
    let class = wide(WINDOW_CLASS);
    let name = wide(title);
    // SAFETY: both buffers are NUL-terminated UTF-16 and outlive the call.
    unsafe {
        let window = FindWindowW(class.as_ptr(), name.as_ptr());
        if window.is_null() {
            return;
        }
        // Hidden-to-tray needs SW_SHOW; minimized needs SW_RESTORE. Avoid
        // calling SW_RESTORE unconditionally so a maximized window stays
        // maximized when a browser capture activates Fetchpath.
        ShowWindow(window, SW_SHOW);
        if IsIconic(window) != 0 {
            ShowWindow(window, SW_RESTORE);
        }
        SetForegroundWindow(window);
    }
}
