pub mod browser_bridge;
pub mod browser_setup;
pub mod media_setup;
pub use fetchpath_session::settings;

use fetchpath_media::MediaInspection;
use fetchpath_session::{
    CancelResponse, DEFAULT_MAX_ACTIVE, JobDetails, JobDraft, JobSnapshot, MAX_DESTINATION_LENGTH,
    MediaDraft, QueueStats, Session,
};
use serde::Serialize;
use settings::Settings;
use std::path::PathBuf;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, State, WindowEvent};

#[tauri::command]
fn start_download(
    url: String,
    destination: String,
    jobs: State<'_, Session>,
) -> Result<JobSnapshot, String> {
    jobs.enqueue(vec![JobDraft {
        url,
        destination,
        not_before_ms: None,
        checksum: None,
    }])?
    .into_iter()
    .next()
    .ok_or_else(|| "The download was not queued.".to_string())
}

#[tauri::command]
fn start_batch(
    drafts: Vec<JobDraft>,
    jobs: State<'_, Session>,
) -> Result<Vec<JobSnapshot>, String> {
    jobs.enqueue(drafts)
}

#[tauri::command]
fn inspect_media(url: String, jobs: State<'_, Session>) -> Result<MediaInspection, String> {
    jobs.inspect_media(&url)
}

#[tauri::command]
fn start_media_download(
    draft: MediaDraft,
    jobs: State<'_, Session>,
) -> Result<JobSnapshot, String> {
    jobs.enqueue_media(draft)
}

#[tauri::command]
fn list_downloads(jobs: State<'_, Session>) -> Result<Vec<JobSnapshot>, String> {
    jobs.list()
}

/// Media pages sent from the browser, taken once each by the interface.
#[tauri::command]
fn take_link_reviews(jobs: State<'_, Session>) -> Vec<String> {
    jobs.take_link_reviews()
}

#[tauri::command]
fn get_download(job_id: String, jobs: State<'_, Session>) -> Result<JobSnapshot, String> {
    jobs.snapshot(&job_id)
}

#[tauri::command]
fn download_details(job_id: String, jobs: State<'_, Session>) -> Result<JobDetails, String> {
    jobs.details(&job_id)
}

#[tauri::command]
fn cancel_download(job_id: String, jobs: State<'_, Session>) -> Result<CancelResponse, String> {
    jobs.cancel(&job_id)
}

#[tauri::command]
fn start_now(job_id: String, jobs: State<'_, Session>) -> Result<JobSnapshot, String> {
    jobs.start_now(&job_id)
}

#[tauri::command]
fn retry_download(
    job_id: String,
    url: Option<String>,
    destination: Option<String>,
    checksum: Option<String>,
    jobs: State<'_, Session>,
) -> Result<JobSnapshot, String> {
    jobs.retry(&job_id, url, destination, checksum)
}

#[tauri::command]
fn pause_download(job_id: String, jobs: State<'_, Session>) -> Result<JobSnapshot, String> {
    jobs.pause(&job_id)
}

#[tauri::command]
fn resume_download(job_id: String, jobs: State<'_, Session>) -> Result<JobSnapshot, String> {
    jobs.resume(&job_id)
}

#[tauri::command]
fn queue_stats(jobs: State<'_, Session>) -> Result<QueueStats, String> {
    Ok(jobs.stats())
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SettingsView {
    settings: Settings,
    /// True when the stored settings file was unusable and defaults were
    /// substituted, so the interface can say so instead of presenting the
    /// defaults as the user's own choices.
    repaired: bool,
    max_active_limit: usize,
    max_retry_attempts: u32,
    /// The folder used when no default destination is set.
    system_download_dir: Option<String>,
}

#[tauri::command]
fn get_settings(app: AppHandle, jobs: State<'_, Session>) -> Result<SettingsView, String> {
    Ok(SettingsView {
        settings: jobs.settings(),
        repaired: jobs.settings_repaired(),
        max_active_limit: settings::MAX_ACTIVE_DOWNLOADS,
        max_retry_attempts: settings::MAX_RETRY_ATTEMPTS,
        system_download_dir: app
            .path()
            .download_dir()
            .ok()
            .map(|dir| dir.display().to_string()),
    })
}

#[tauri::command]
fn update_settings(
    next: Settings,
    app: AppHandle,
    jobs: State<'_, Session>,
) -> Result<SettingsView, String> {
    jobs.update_settings(next)?;
    get_settings(app, jobs)
}

/// The folder new downloads are saved in: the user's choice, else Windows'
/// own Downloads folder.
#[tauri::command]
fn default_destination_dir(
    app: AppHandle,
    jobs: State<'_, Session>,
) -> Result<Option<String>, String> {
    Ok(jobs.settings().default_destination_dir.or_else(|| {
        app.path()
            .download_dir()
            .ok()
            .map(|dir| dir.display().to_string())
    }))
}

/// Opens Explorer with the finished file selected.
///
/// Only ever points at a destination this queue recorded, so a renderer message
/// cannot turn this into a way to launch an arbitrary path.
#[tauri::command]
fn reveal_download(job_id: String, jobs: State<'_, Session>) -> Result<(), String> {
    let snapshot = jobs.snapshot(&job_id)?;
    let destination = snapshot
        .destination
        .ok_or_else(|| "This download has no saved file yet.".to_string())?;
    let path = PathBuf::from(&destination);
    if !path.exists() {
        return Err(format!("{destination} is no longer on disk."));
    }
    // Explorer parses its own command line rather than using the standard
    // argv rules, and `/select,` with the path must arrive as one unquoted
    // token followed by a quoted path. Passing it through `arg` lets Rust
    // quote the whole `/select,C:\Some Folder\file` string, which Explorer
    // then fails to split and answers by opening Documents instead. `raw_arg`
    // writes the command line exactly.
    //
    // A quote inside the path would escape the quoting below, so it is refused.
    // `validated_destination` already rejects one, which makes this a second
    // fence rather than the only one.
    if destination.contains('"') {
        return Err("That destination cannot be shown in File Explorer.".into());
    }
    std::os::windows::process::CommandExt::raw_arg(
        &mut std::process::Command::new("explorer.exe"),
        format!("/select,\"{}\"", path.display()),
    )
    .stdin(std::process::Stdio::null())
    .stdout(std::process::Stdio::null())
    .stderr(std::process::Stdio::null())
    .spawn()
    .map(|_| ())
    .map_err(|error| format!("Could not open the folder: {error}"))
}

#[tauri::command]
fn media_tools_status(
    app: AppHandle,
    jobs: State<'_, Session>,
) -> Result<media_setup::ToolsStatus, String> {
    let install_dir = media_tools_install_dir(&app)?;
    Ok(media_setup::status(
        jobs.settings().media_tools_dir.as_deref(),
        &install_dir,
    ))
}

#[tauri::command]
fn install_media_tools(
    app: AppHandle,
    jobs: State<'_, Session>,
) -> Result<media_setup::ToolsStatus, String> {
    let install_dir = media_tools_install_dir(&app)?;
    media_setup::install(&install_dir)?;
    let mut settings = jobs.settings();
    settings.media_tools_dir = Some(install_dir.display().to_string());
    jobs.update_settings(settings)?;
    media_tools_status(app, jobs)
}

#[tauri::command]
fn use_media_tools_dir(
    directory: String,
    app: AppHandle,
    jobs: State<'_, Session>,
) -> Result<media_setup::ToolsStatus, String> {
    if directory.len() > MAX_DESTINATION_LENGTH {
        return Err("That folder path is too long.".into());
    }
    let accepted = media_setup::use_directory(&PathBuf::from(&directory))?;
    let mut settings = jobs.settings();
    settings.media_tools_dir = Some(accepted);
    jobs.update_settings(settings)?;
    media_tools_status(app, jobs)
}

/// Where the `fetchpath` command is, when the installer put it beside the app.
#[tauri::command]
fn cli_path() -> Option<String> {
    let cli = std::env::current_exe()
        .ok()?
        .parent()?
        .join("fetchpath.exe");
    cli.is_file().then(|| cli.display().to_string())
}

#[tauri::command]
fn browser_setup_status(app: AppHandle) -> browser_setup::BrowserSetupStatus {
    browser_setup::status(app.path().resource_dir().ok().as_deref())
}

#[tauri::command]
fn reveal_extension_folder(app: AppHandle) -> Result<(), String> {
    browser_setup::reveal_extension_folder(app.path().resource_dir().ok().as_deref())
}

fn media_tools_install_dir(app: &AppHandle) -> Result<PathBuf, String> {
    app.path()
        .app_data_dir()
        .map(|dir| dir.join("media-tools"))
        .map_err(|error| format!("Could not resolve the Fetchpath data folder: {error}"))
}

#[tauri::command]
fn remove_download(job_id: String, jobs: State<'_, Session>) -> Result<(), String> {
    jobs.remove(&job_id)
}

/// One Fetchpath process per Windows user account.
///
/// Two processes would both own `queue-v1.json` and both claim the tray icon, so
/// the second one would race the first over the persisted queue. The guard is a
/// deny-sharing handle on a lock file in the same application-data directory as
/// the queue: whoever opens it first keeps it for the life of the process.
///
/// It fails open. Only a sharing or locking violation means "another instance is
/// running"; any other error (an unwritable directory, a missing `%APPDATA%`)
/// lets the application start, because refusing to launch is worse than the
/// unlikely double-launch it would prevent.
mod single_instance {
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
        fn ShowWindow(window: *mut core::ffi::c_void, command: i32) -> i32;
        fn SetForegroundWindow(window: *mut core::ffi::c_void) -> i32;
    }

    pub fn lock_path() -> Option<PathBuf> {
        let roaming = std::env::var_os("APPDATA")?;
        Some(
            Path::new(&roaming)
                .join("app.fetchpath.desktop")
                .join("instance.lock"),
        )
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
            // Hidden-to-tray needs SW_SHOW; minimized needs SW_RESTORE.
            ShowWindow(window, SW_SHOW);
            ShowWindow(window, SW_RESTORE);
            SetForegroundWindow(window);
        }
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    if let Some(path) = single_instance::lock_path()
        && !single_instance::claim(&path)
    {
        single_instance::activate_running_window("Fetchpath");
        return;
    }

    let app = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            start_download,
            start_batch,
            inspect_media,
            start_media_download,
            list_downloads,
            take_link_reviews,
            get_download,
            download_details,
            cancel_download,
            start_now,
            retry_download,
            remove_download,
            pause_download,
            resume_download,
            queue_stats,
            get_settings,
            update_settings,
            default_destination_dir,
            reveal_download,
            media_tools_status,
            install_media_tools,
            use_media_tools_dir,
            browser_setup_status,
            cli_path,
            reveal_extension_folder
        ])
        .setup(|app| {
            let state_path = app.path().app_data_dir()?.join("queue-v1.json");
            let download_dir = app.path().download_dir()?;
            app.manage(Session::load_with_browser(
                state_path,
                DEFAULT_MAX_ACTIVE,
                Some(download_dir),
            )?);
            let show = MenuItem::with_id(app, "show", "Show Fetchpath", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "Quit Fetchpath", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show, &quit])?;
            let tray = TrayIconBuilder::new()
                .icon(app.default_window_icon().expect("app icon").clone())
                .tooltip("Fetchpath")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "show" => show_main_window(app),
                    "quit" => {
                        app.state::<Session>().cancel_all_and_join();
                        app.exit(0);
                    }
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        show_main_window(tray.app_handle());
                    }
                })
                .build(app)?;
            app.manage(tray);
            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                // Closing to the tray is the default because a running queue
                // should survive a stray click on the X. A user who turned that
                // off means the close button to close, so finish the transfers
                // down cleanly and exit rather than hiding.
                let jobs = window.app_handle().state::<Session>();
                if jobs.settings().close_to_tray {
                    api.prevent_close();
                    let _ = window.hide();
                } else {
                    jobs.cancel_all_and_join();
                    window.app_handle().exit(0);
                }
            }
        })
        .build(tauri::generate_context!())
        .expect("error while building Fetchpath");

    app.run(|_, event| {
        if let tauri::RunEvent::ExitRequested {
            code: None, api, ..
        } = event
        {
            api.prevent_exit();
        }
    });
}

fn show_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.set_focus();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn a_second_instance_is_refused_while_the_first_holds_the_lock() {
        use std::fs::OpenOptions;
        use std::os::windows::fs::OpenOptionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("instance.lock");

        // The first claim also creates the application-data directory.
        assert!(single_instance::claim(&path));
        assert!(path.exists());

        // A second process is modelled by an independent deny-sharing open of
        // the same path, because the in-process claim is held by a static.
        let contended = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .share_mode(0)
            .open(&path);
        assert_eq!(
            contended.unwrap_err().raw_os_error(),
            Some(32),
            "a second instance must see ERROR_SHARING_VIOLATION"
        );

        // Failing open: an unusable lock path must not stop the application. A
        // regular file makes an impossible parent directory.
        let blocker = dir.path().join("blocker");
        fs::write(&blocker, b"not a directory").unwrap();
        let unusable = blocker.join("child.lock");
        assert!(single_instance::claim(&unusable));
        assert!(!unusable.exists());
    }
}
