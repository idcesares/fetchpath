pub mod browser_bridge;
pub mod browser_setup;
pub mod engine_link;
mod lan_commands;
mod policy_commands;
mod share_commands;
mod single_instance;
pub mod view;

use engine_link::{EngineLink, Signal};
use fetchpath_protocol::command::{Command, DestinationDecision, JobFilter, JobInput};
use fetchpath_protocol::launch::EngineHome;
use fetchpath_protocol::message::{CommandResult, ControlOutcome};
use fetchpath_protocol::model;
use fetchpath_protocol::{JobId, JobSnapshot, ProtocolError, Timestamp};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, State, WindowEvent};

/// Longest folder path accepted from the interface.
const MAX_PATH_LENGTH: usize = 4_096;

pub(crate) type Engine<'a> = State<'a, Arc<EngineLink>>;

const TRAY_ID: &str = "fetchpath-tray";

/// The close button's setting, kept here so closing the window never waits
/// on the engine.
struct CloseToTray(AtomicBool);

/// One line for the tray: which Fetchpath, what it is downloading, what waits
/// for the person, and whether it stays on in the background. `None` when
/// the engine did not answer, so the tray keeps what it last confirmed.
fn tray_summary(link: &EngineLink) -> Option<String> {
    let stats = match link.send(Command::QueueStats).ok()? {
        CommandResult::QueueStats { stats } => stats,
        _ => return None,
    };
    let name = match link.send(Command::EngineStatus).ok()? {
        CommandResult::EngineStatus { status } => status.instance.map(|instance| instance.name),
        _ => None,
    };
    let always_on = matches!(
        link.send(Command::GetSettings),
        Ok(CommandResult::Settings { view }) if view.settings.hub_mode == Some(true)
    );
    let mut parts = vec![format!(
        "Fetchpath on {}",
        name.as_deref().unwrap_or("this computer")
    )];
    parts.push(match stats.running {
        0 if stats.queued + stats.scheduled == 0 => "nothing downloading".to_owned(),
        0 => format!("{} waiting to start", stats.queued + stats.scheduled),
        1 => "1 downloading".to_owned(),
        n => format!("{n} downloading"),
    });
    match stats.awaiting_approval {
        0 => {}
        1 => parts.push("1 needs your approval".to_owned()),
        n => parts.push(format!("{n} need your approval")),
    }
    if always_on {
        parts.push("always on".to_owned());
    }
    Some(parts.join(" · "))
}

/// Whether the engine is reachable, as last reported by the queue watcher.
struct Connection(std::sync::Mutex<serde_json::Value>);

impl Default for Connection {
    fn default() -> Self {
        Self(std::sync::Mutex::new(
            serde_json::json!({ "connected": true }),
        ))
    }
}

/// The engine's reachability now, for a page that starts listening after
/// the watcher's first report.
#[tauri::command]
fn engine_connection(connection: State<'_, Arc<Connection>>) -> serde_json::Value {
    connection
        .0
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}

/// The person asked to start the engine after it was stopped on purpose
/// or would not start: the window may start it again, and does within a
/// second.
#[tauri::command]
fn start_engine(engine: Engine<'_>) {
    engine.allow_start();
}

/// The person confirmed stopping the engine. Downloads stop with it and wait
/// in the queue; the window then reports the engine as stopped, from the
/// watcher, not from this call.
#[tauri::command]
async fn stop_engine(engine: Engine<'_>) -> Result<(), String> {
    let engine = Arc::clone(&engine);
    off_thread(move || engine.stop().map_err(text)).await
}

/// Closes the window the way its close button does: to the notification
/// area, or by ending the desktop, per the setting. Downloads continue.
#[tauri::command]
fn close_window(window: tauri::WebviewWindow) {
    let _ = window.close();
}

/// Runs a command's body on a blocking thread: it may wait on the pipe, or
/// up to ten seconds for an engine to start, and must not hold up Tauri's
/// async runtime meanwhile.
pub(crate) async fn off_thread<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(work)
        .await
        .map_err(|error| format!("Fetchpath could not finish that: {error}"))?
}

/// The interface shows errors as text.
pub(crate) fn text(error: ProtocolError) -> String {
    error.message
}

pub(crate) fn unexpected(result: &CommandResult) -> String {
    let kind = serde_json::to_value(result)
        .ok()
        .and_then(|value| {
            value
                .get("type")
                .and_then(|kind| kind.as_str().map(str::to_owned))
        })
        .unwrap_or_default();
    format!("Fetchpath's engine gave an unexpected answer ({kind}).")
}

pub(crate) fn job_id(value: &str) -> Result<JobId, String> {
    JobId::try_from(value).map_err(|_| "This download is no longer available.".to_string())
}

fn one(result: CommandResult) -> Result<JobSnapshot, String> {
    match result {
        CommandResult::Job { job } | CommandResult::Control { job, .. } => Ok(job),
        other => Err(unexpected(&other)),
    }
}

fn send_job(engine: &EngineLink, command: Command) -> Result<view::JobView, String> {
    let job = one(engine.send(command).map_err(text)?)?;
    Ok(view::job(&job, Timestamp::now()))
}

fn snapshot(engine: &EngineLink, id: &str) -> Result<JobSnapshot, String> {
    one(engine
        .send(Command::GetJob {
            job_id: job_id(id)?,
        })
        .map_err(text)?)
}

#[tauri::command]
async fn start_batch(
    drafts: Vec<view::JobDraft>,
    engine: Engine<'_>,
) -> Result<Vec<view::JobView>, String> {
    let engine = Arc::clone(&engine);
    off_thread(move || {
        let requests = drafts
            .iter()
            .map(view::JobDraft::request)
            .collect::<Result<Vec<_>, _>>()?;
        match engine
            .send(Command::CreateJobs { requests })
            .map_err(text)?
        {
            CommandResult::Jobs { jobs } => Ok(view::jobs(&jobs)),
            other => Err(unexpected(&other)),
        }
    })
    .await
}

/// A model or dataset repository resolved to one commit (FP-022).
#[tauri::command]
async fn inspect_repository(
    url: String,
    engine: Engine<'_>,
) -> Result<model::RepositoryView, String> {
    let engine = Arc::clone(&engine);
    off_thread(move || {
        let url = view::link(&url)?;
        match engine
            .send(Command::InspectRepository { url })
            .map_err(text)?
        {
            CommandResult::Repository { repository } => Ok(repository),
            other => Err(unexpected(&other)),
        }
    })
    .await
}

/// Queues every file of a repository, pinned to one commit, in a folder
/// named after it inside `folder`. Resolved again here, so what is queued is
/// what the provider states now, not what the window last showed.
#[tauri::command]
async fn add_repository(
    url: String,
    folder: String,
    engine: Engine<'_>,
) -> Result<Vec<view::JobView>, String> {
    let engine = Arc::clone(&engine);
    off_thread(move || {
        let url = view::link(&url)?;
        let repository = match engine
            .send(Command::InspectRepository { url })
            .map_err(text)?
        {
            CommandResult::Repository { repository } => repository,
            other => return Err(unexpected(&other)),
        };
        let mut jobs = Vec::new();
        // One job per file: each has its own checksum and subfolder.
        for request in repository.requests(&folder) {
            match engine.send(Command::CreateJob { request }).map_err(text)? {
                CommandResult::Job { job } => jobs.push(job),
                other => return Err(unexpected(&other)),
            }
        }
        Ok(view::jobs(&jobs))
    })
    .await
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct PageImageView {
    url: String,
    filename: String,
    title: Option<String>,
    content_type: Option<String>,
}

#[tauri::command]
async fn inspect_page_image(
    url: String,
    engine: Engine<'_>,
) -> Result<Option<PageImageView>, String> {
    let engine = Arc::clone(&engine);
    off_thread(move || {
        match engine
            .send(Command::InspectPageImage {
                url: view::link(&url)?,
            })
            .map_err(text)?
        {
            CommandResult::PageImage { image } => Ok(image.map(|image| PageImageView {
                url: image.url,
                filename: image.filename,
                title: image.title,
                content_type: image.content_type,
            })),
            other => Err(unexpected(&other)),
        }
    })
    .await
}

#[tauri::command]
async fn inspect_media(url: String, engine: Engine<'_>) -> Result<view::Inspection, String> {
    let engine = Arc::clone(&engine);
    off_thread(move || {
        match engine
            .send(Command::InspectMedia {
                url: view::link(&url)?,
            })
            .map_err(text)?
        {
            CommandResult::MediaInspection { inspection } => {
                Ok(view::Inspection::from(&inspection))
            }
            other => Err(unexpected(&other)),
        }
    })
    .await
}

#[tauri::command]
async fn start_media_download(
    draft: view::MediaDraft,
    engine: Engine<'_>,
) -> Result<view::JobView, String> {
    let engine = Arc::clone(&engine);
    off_thread(move || {
        send_job(
            &engine,
            Command::CreateJob {
                request: draft.request()?,
            },
        )
    })
    .await
}

#[tauri::command]
async fn start_torrent_download(
    draft: view::TorrentDraft,
    engine: Engine<'_>,
) -> Result<view::JobView, String> {
    let engine = Arc::clone(&engine);
    off_thread(move || {
        send_job(
            &engine,
            Command::CreateJob {
                request: draft.request()?,
            },
        )
    })
    .await
}

#[tauri::command]
async fn list_downloads(engine: Engine<'_>) -> Result<Vec<view::JobView>, String> {
    let engine = Arc::clone(&engine);
    off_thread(move || {
        match engine
            .send(Command::ListJobs {
                filter: JobFilter::All,
            })
            .map_err(text)?
        {
            CommandResult::Jobs { jobs } => Ok(view::jobs(&jobs)),
            other => Err(unexpected(&other)),
        }
    })
    .await
}

/// Media pages sent from the browser, taken once each by the interface.
#[tauri::command]
async fn take_link_reviews(engine: Engine<'_>) -> Result<Vec<String>, String> {
    let engine = Arc::clone(&engine);
    off_thread(
        move || match engine.send(Command::TakeLinkReviews).map_err(text)? {
            CommandResult::LinkReviews { urls } => Ok(urls.into_iter().map(String::from).collect()),
            other => Err(unexpected(&other)),
        },
    )
    .await
}

#[tauri::command]
async fn download_details(job_id: String, engine: Engine<'_>) -> Result<view::JobDetails, String> {
    let engine = Arc::clone(&engine);
    off_thread(move || {
        match engine
            .send(Command::JobDetails {
                job_id: self::job_id(&job_id)?,
            })
            .map_err(text)?
        {
            CommandResult::Details { details } => Ok(view::JobDetails::from(&details)),
            other => Err(unexpected(&other)),
        }
    })
    .await
}

#[tauri::command]
async fn cancel_download(
    job_id: String,
    engine: Engine<'_>,
) -> Result<view::CancelResponse, String> {
    let engine = Arc::clone(&engine);
    off_thread(move || {
        match engine
            .send(Command::Cancel {
                job_id: self::job_id(&job_id)?,
                retain_partial: false,
            })
            .map_err(text)?
        {
            CommandResult::Control { outcome, job } => Ok(view::CancelResponse {
                outcome: match outcome {
                    ControlOutcome::Accepted => "accepted",
                    ControlOutcome::TooLateToCancel | ControlOutcome::TooLate => "too_late",
                    ControlOutcome::AlreadyTerminal => "already_terminal",
                    _ => "no_op",
                },
                job: view::job(&job, Timestamp::now()),
            }),
            other => Err(unexpected(&other)),
        }
    })
    .await
}

#[tauri::command]
async fn start_now(job_id: String, engine: Engine<'_>) -> Result<view::JobView, String> {
    let engine = Arc::clone(&engine);
    off_thread(move || {
        send_job(
            &engine,
            Command::Start {
                job_id: self::job_id(&job_id)?,
            },
        )
    })
    .await
}

/// Retry, a new destination, a refreshed link or a corrected checksum. A new
/// link travels with its destination and checksum in one step; a new
/// destination alone is a destination decision.
#[tauri::command]
async fn retry_download(
    job_id: String,
    url: Option<String>,
    destination: Option<String>,
    checksum: Option<String>,
    engine: Engine<'_>,
) -> Result<view::JobView, String> {
    let engine = Arc::clone(&engine);
    off_thread(move || {
        let current = snapshot(&engine, &job_id)?;
        send_job(
            &engine,
            retry_command(&current, url, destination, checksum)?,
        )
    })
    .await
}

/// The command for a retry from the interface, sending only what changed.
fn retry_command(
    current: &JobSnapshot,
    url: Option<String>,
    destination: Option<String>,
    checksum: Option<String>,
) -> Result<Command, String> {
    let moved = destination
        .map(|path| path.trim().to_owned())
        .filter(|path| !path.is_empty() && Some(path) != current.destination.as_ref());
    let checksum = checksum
        .map(|sum| sum.trim().to_ascii_lowercase())
        .filter(|sum| sum.as_str() != current.expected_sha256.as_deref().unwrap_or_default());
    let job_id = current.job_id.clone();
    Ok(match (url.filter(|url| !url.trim().is_empty()), moved) {
        (Some(url), destination) => Command::RefreshSource {
            job_id,
            source: JobInput::Url {
                url: view::link(&url)?,
            },
            destination,
            expected_sha256: checksum,
        },
        (None, Some(path)) => Command::ResolveDestination {
            job_id,
            decision: DestinationDecision::ChooseNewPath {
                path,
                expected_sha256: checksum,
            },
        },
        (None, None) => Command::Retry {
            job_id,
            expected_sha256: checksum,
        },
    })
}

#[tauri::command]
async fn pause_download(job_id: String, engine: Engine<'_>) -> Result<view::JobView, String> {
    let engine = Arc::clone(&engine);
    off_thread(move || {
        send_job(
            &engine,
            Command::Pause {
                job_id: self::job_id(&job_id)?,
            },
        )
    })
    .await
}

#[tauri::command]
async fn resume_download(job_id: String, engine: Engine<'_>) -> Result<view::JobView, String> {
    let engine = Arc::clone(&engine);
    off_thread(move || {
        send_job(
            &engine,
            Command::Resume {
                job_id: self::job_id(&job_id)?,
            },
        )
    })
    .await
}

#[tauri::command]
async fn queue_stats(engine: Engine<'_>) -> Result<view::QueueStats, String> {
    let engine = Arc::clone(&engine);
    off_thread(
        move || match engine.send(Command::QueueStats).map_err(text)? {
            CommandResult::QueueStats { stats } => Ok(view::QueueStats::from(&stats)),
            other => Err(unexpected(&other)),
        },
    )
    .await
}

fn system_download_dir(app: &AppHandle) -> Option<String> {
    app.path()
        .download_dir()
        .ok()
        .map(|dir| dir.display().to_string())
}

fn settings_of(app: &AppHandle, result: CommandResult) -> Result<view::SettingsView, String> {
    match result {
        CommandResult::Settings { view } => {
            app.state::<CloseToTray>()
                .0
                .store(view.settings.close_to_tray, Ordering::SeqCst);
            Ok(view::SettingsView::new(&view, system_download_dir(app)))
        }
        other => Err(unexpected(&other)),
    }
}

#[tauri::command]
async fn get_settings(app: AppHandle, engine: Engine<'_>) -> Result<view::SettingsView, String> {
    let engine = Arc::clone(&engine);
    off_thread(move || settings_of(&app, engine.send(Command::GetSettings).map_err(text)?)).await
}

#[tauri::command]
async fn update_settings(
    next: view::Settings,
    app: AppHandle,
    engine: Engine<'_>,
) -> Result<view::SettingsView, String> {
    let engine = Arc::clone(&engine);
    off_thread(move || {
        settings_of(
            &app,
            engine
                .send(Command::UpdateSettings {
                    settings: next.to_engine(),
                })
                .map_err(text)?,
        )
    })
    .await
}

/// The folder new downloads are saved in: the user's choice, else Windows'
/// own Downloads folder.
#[tauri::command]
async fn default_destination_dir(
    app: AppHandle,
    engine: Engine<'_>,
) -> Result<Option<String>, String> {
    let engine = Arc::clone(&engine);
    off_thread(move || {
        let view = settings_of(&app, engine.send(Command::GetSettings).map_err(text)?)?;
        Ok(view
            .settings
            .default_destination_dir
            .or(view.system_download_dir))
    })
    .await
}

/// Opens Explorer with the finished file selected.
///
/// Only ever points at a destination the engine recorded for this job, so a
/// renderer message cannot turn this into a way to launch an arbitrary path.
#[tauri::command]
async fn reveal_download(job_id: String, engine: Engine<'_>) -> Result<(), String> {
    let engine = Arc::clone(&engine);
    off_thread(move || {
        let snapshot = snapshot(&engine, &job_id)?;
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
        // The queue already rejects one, which makes this a second fence rather
        // than the only one.
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
    })
    .await
}

/// Timeout for downloading and verifying the media helpers (`yt-dlp` and `ffmpeg`).
const MEDIA_INSTALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(600);

fn tools_result(result: CommandResult) -> Result<view::ToolsStatus, String> {
    match result {
        CommandResult::MediaTools { status } => Ok(view::ToolsStatus::from(status)),
        other => Err(unexpected(&other)),
    }
}

#[tauri::command]
async fn media_tools_status(engine: Engine<'_>) -> Result<view::ToolsStatus, String> {
    let engine = Arc::clone(&engine);
    off_thread(move || tools_result(engine.send(Command::MediaToolsStatus).map_err(text)?)).await
}

#[tauri::command]
async fn install_media_tools(engine: Engine<'_>) -> Result<view::ToolsStatus, String> {
    let engine = Arc::clone(&engine);
    off_thread(move || {
        tools_result(
            engine
                .send_dedicated(Command::InstallMediaTools, MEDIA_INSTALL_TIMEOUT)
                .map_err(text)?,
        )
    })
    .await
}

#[tauri::command]
async fn use_media_tools_dir(
    directory: String,
    engine: Engine<'_>,
) -> Result<view::ToolsStatus, String> {
    let engine = Arc::clone(&engine);
    off_thread(move || {
        if directory.len() > MAX_PATH_LENGTH {
            return Err("That folder path is too long.".into());
        }
        tools_result(
            engine
                .send(Command::UseMediaToolsDir { directory })
                .map_err(text)?,
        )
    })
    .await
}

/// Where the `fetchpath` command is, when the installer put it beside the app.
#[tauri::command]
fn cli_path() -> Option<String> {
    engine_link::engine_exe()
        .filter(|cli| cli.is_file())
        .map(|cli| cli.display().to_string())
}

/// Connects Codex or Claude Code after installing, or reads or removes that
/// connection, through the command line's own `agent-setup` so there is one
/// implementation. Only these verbs and hosts are passed on; the returned text
/// is the command's output, or its error line.
#[tauri::command]
async fn agent_host_setup(action: String, host: Option<String>) -> Result<String, String> {
    use std::os::windows::process::CommandExt;
    let mut args = vec!["agent-setup".to_owned()];
    match (action.as_str(), host.as_deref()) {
        ("status", None) => args.extend(["status".into(), "--json".into()]),
        ("add" | "remove" | "check", Some("codex" | "claude-code")) => {
            args.extend([action.clone(), host.clone().unwrap_or_default()]);
        }
        _ => return Err("Unknown agent setup request.".into()),
    }
    let cli = engine_link::engine_exe()
        .filter(|cli| cli.is_file())
        .ok_or("The fetchpath command is not installed beside this copy of Fetchpath.")?;
    off_thread(move || {
        let output = std::process::Command::new(cli)
            .args(&args)
            .stdin(std::process::Stdio::null())
            // CREATE_NO_WINDOW: no console flash.
            .creation_flags(0x0800_0000)
            .output()
            .map_err(|error| format!("Could not run fetchpath: {error}"))?;
        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
        } else {
            Err(String::from_utf8_lossy(&output.stderr).trim().to_owned())
        }
    })
    .await
}

#[tauri::command]
fn browser_setup_status(app: AppHandle) -> browser_setup::BrowserSetupStatus {
    browser_setup::status(app.path().resource_dir().ok().as_deref())
}

/// What setup installed, read-only (FP-099). `None` for a copy that setup did
/// not install, such as a development build. The components are changed by
/// running Fetchpath setup again, so Settings only says so.
#[tauri::command]
fn installed_components() -> Option<String> {
    fetchpath_protocol::install::installed_selection().map(|selection| selection.summary())
}

#[tauri::command]
fn reveal_extension_folder(app: AppHandle) -> Result<(), String> {
    browser_setup::reveal_extension_folder(app.path().resource_dir().ok().as_deref())
}

/// Whether Setup started this window to connect the browser: its finish page
/// passes `--connect-browser`, and the page then opens that part of Settings.
#[tauri::command]
fn connect_browser_requested() -> bool {
    std::env::args().any(|arg| arg == "--connect-browser")
}

#[tauri::command]
fn connect_browser(app: AppHandle, browser: String) -> Result<(), String> {
    browser_setup::connect_browser(&browser, app.path().resource_dir().ok().as_deref())
}

/// Opens only Fetchpath's own project page in the system browser.
#[tauri::command]
fn open_project_page() -> Result<(), String> {
    if !open_in_browser("https://github.com/idcesares/fetchpath") {
        return Err("Could not open the project page in your browser.".into());
    }
    Ok(())
}

/// Hands an http(s) address to the system browser.
fn open_in_browser(url: &str) -> bool {
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    let url: Vec<u16> = url.encode_utf16().chain(std::iter::once(0)).collect();
    let result = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            std::ptr::null(),
            url.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    };
    (result as isize) > 32
}

/// `http://fetchpath.localhost:PORT/...` exactly: a port of digits right
/// after the host, so no user info can move the link to another host.
fn is_web_ui_link(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("http://fetchpath.localhost:") else {
        return false;
    };
    let port = rest.split('/').next().unwrap_or("");
    (1..=5).contains(&port.len())
        && port.bytes().all(|b| b.is_ascii_digit())
        && rest.as_bytes().get(port.len()) == Some(&b'/')
}

/// Opens the queue in the browser (FP-104) through a single-use link the
/// engine issues. The link signs the browser in, so it goes only to the
/// browser and only when it names the engine's own loopback page.
fn open_web_ui_now(engine: &EngineLink) -> Result<(), String> {
    match engine.send(Command::OpenWebUi).map_err(text)? {
        CommandResult::WebUiLink { url } => {
            if !is_web_ui_link(url.expose()) {
                return Err("The engine answered with an unexpected web UI address.".into());
            }
            if open_in_browser(url.expose()) {
                Ok(())
            } else {
                Err("Could not open the web UI in your browser.".into())
            }
        }
        other => Err(unexpected(&other)),
    }
}

#[tauri::command]
async fn open_web_ui(engine: Engine<'_>) -> Result<(), String> {
    let engine = Arc::clone(&engine);
    off_thread(move || open_web_ui_now(&engine)).await
}

/// Ends every browser session; the browsers must be opened from here again.
#[tauri::command]
async fn sign_out_browsers(engine: Engine<'_>) -> Result<(), String> {
    let engine = Arc::clone(&engine);
    off_thread(
        move || match engine.send(Command::SignOutBrowsers).map_err(text)? {
            CommandResult::BrowsersSignedOut => Ok(()),
            other => Err(unexpected(&other)),
        },
    )
    .await
}

#[tauri::command]
async fn remove_download(job_id: String, engine: Engine<'_>) -> Result<(), String> {
    let engine = Arc::clone(&engine);
    off_thread(move || {
        match engine
            .send(Command::RemoveJob {
                job_id: self::job_id(&job_id)?,
            })
            .map_err(text)?
        {
            CommandResult::Removed { .. } => Ok(()),
            other => Err(unexpected(&other)),
        }
    })
    .await
}

/// Lets an agent's waiting request download (FP-066). Only the person's
/// own clients can; the engine refuses it from an agent.
#[tauri::command]
async fn approve_download(job_id: String, engine: Engine<'_>) -> Result<view::JobView, String> {
    let engine = Arc::clone(&engine);
    off_thread(move || {
        send_job(
            &engine,
            Command::ApproveJob {
                job_id: self::job_id(&job_id)?,
            },
        )
    })
    .await
}

/// Refuses an agent's waiting request; it ends cancelled with a reason the
/// agent can relay.
#[tauri::command]
async fn deny_download(job_id: String, engine: Engine<'_>) -> Result<view::JobView, String> {
    let engine = Arc::clone(&engine);
    off_thread(move || {
        send_job(
            &engine,
            Command::DenyJob {
                job_id: self::job_id(&job_id)?,
            },
        )
    })
    .await
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    if let Some(path) = single_instance::window_lock_path()
        && !single_instance::claim(&path)
    {
        single_instance::activate_running_window("Fetchpath");
        return;
    }

    let app = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            engine_connection,
            start_engine,
            stop_engine,
            close_window,
            start_batch,
            inspect_page_image,
            inspect_media,
            start_media_download,
            start_torrent_download,
            list_downloads,
            take_link_reviews,
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
            installed_components,
            cli_path,
            agent_host_setup,
            reveal_extension_folder,
            connect_browser,
            connect_browser_requested,
            open_project_page,
            open_web_ui,
            sign_out_browsers,
            approve_download,
            deny_download,
            policy_commands::list_inbox,
            policy_commands::accept_offer,
            policy_commands::decline_offer,
            policy_commands::list_agents,
            policy_commands::set_agent,
            policy_commands::revoke_agent,
            policy_commands::list_rules,
            policy_commands::add_rule,
            policy_commands::remove_rule,
            policy_commands::inspect_rules,
            policy_commands::cache_status,
            policy_commands::clear_cache,
            inspect_repository,
            add_repository,
            lan_commands::lan_status,
            lan_commands::set_lan_sharing,
            lan_commands::start_pairing,
            lan_commands::cancel_pairing,
            lan_commands::join_pairing,
            lan_commands::unpair_device,
            share_commands::share_status,
            share_commands::configure_sharing,
            share_commands::configure_relay,
            share_commands::set_sharing_enabled,
            share_commands::start_share_test,
            share_commands::create_share,
            share_commands::save_share_qr,
            share_commands::revoke_share
        ])
        .setup(|app| {
            // The desktop holds no queue: the engine does, and this window is
            // one of its clients. It starts the engine when none is running.
            let home = EngineHome::from_env().map_err(|error| error.message)?;
            let engine = Arc::new(EngineLink::new(home, engine_link::engine_exe()));
            app.manage(Arc::clone(&engine));
            app.manage(CloseToTray(AtomicBool::new(true)));
            let connection = Arc::new(Connection::default());
            app.manage(Arc::clone(&connection));
            let handle = app.handle().clone();
            // The tray is a management client of the engine (FP-101): its
            // first line says what the engine is doing, never more than the
            // engine confirmed.
            let status = MenuItem::with_id(
                app,
                "status",
                "Fetchpath: connecting to the engine",
                false,
                None::<&str>,
            )?;
            let show = MenuItem::with_id(app, "show", "Show Fetchpath", true, None::<&str>)?;
            let web = MenuItem::with_id(app, "web", "Open in browser", true, None::<&str>)?;
            let start = MenuItem::with_id(app, "start", "Start engine", false, None::<&str>)?;
            let stop = MenuItem::with_id(
                app,
                "stop",
                "Stop engine…",
                true,
                None::<&str>,
            )?;
            let quit = MenuItem::with_id(
                app,
                "quit",
                "Quit tray (downloads continue)",
                true,
                None::<&str>,
            )?;
            let quit_all = MenuItem::with_id(
                app,
                "quit_all",
                "Quit and stop engine",
                true,
                None::<&str>,
            )?;
            let tray_status = status.clone();
            let tray_start = start.clone();
            let tray_stop = stop.clone();
            let summary_link = Arc::clone(&engine);
            let show_summary = move |handle: &tauri::AppHandle, text: String| {
                let _ = tray_status.set_text(&text);
                if let Some(tray) = handle.tray_by_id(TRAY_ID) {
                    let _ = tray.set_tooltip(Some(text));
                }
            };
            // What the engine last said about its damaged inbox file; kept
            // so a later report of another state still carries it.
            let inbox_notice = std::sync::Mutex::new(None::<String>);
            // Runs for the life of the window.
            let _watcher = engine_link::watch(engine, move |signal| {
                if matches!(signal, Signal::Queue | Signal::Connected)
                    && let Some(text) = tray_summary(&summary_link)
                {
                    show_summary(&handle, text);
                }
                let mut state = match signal {
                    Signal::Queue => {
                        let _ = handle.emit("fetchpath://queue", ());
                        return;
                    }
                    Signal::Connected => serde_json::json!({ "connected": true }),
                    Signal::ReadOnly(reason) => {
                        serde_json::json!({ "connected": true, "readOnly": reason })
                    }
                    Signal::Disconnected { message, stopped } => {
                        *inbox_notice
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
                        serde_json::json!({ "connected": false, "stopped": stopped, "message": message })
                    }
                    Signal::InboxNotice(notice) => {
                        *inbox_notice
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner()) = notice;
                        connection
                            .0
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner())
                            .clone()
                    }
                };
                if state["connected"] == true {
                    let _ = tray_start.set_enabled(false);
                    let _ = tray_stop.set_enabled(true);
                    let notice = inbox_notice
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .clone();
                    match notice {
                        Some(notice) => state["inboxNotice"] = serde_json::json!(notice),
                        None => {
                            if let Some(fields) = state.as_object_mut() {
                                fields.remove("inboxNotice");
                            }
                        }
                    }
                }
                if state["connected"] != true {
                    let _ = tray_start.set_enabled(true);
                    let _ = tray_stop.set_enabled(false);
                    show_summary(
                        &handle,
                        if state["stopped"] == true {
                            "Fetchpath: engine stopped. Use Start engine in this menu.".into()
                        } else {
                            "Fetchpath: engine not running. Use Start engine in this menu.".into()
                        },
                    );
                }
                // Kept as well as sent: the page may not be listening yet.
                *connection
                    .0
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()) = state.clone();
                let _ = handle.emit("fetchpath://engine", state);
            });
            let menu = Menu::with_items(
                app,
                &[
                    &status,
                    &PredefinedMenuItem::separator(app)?,
                    &show,
                    &web,
                    &PredefinedMenuItem::separator(app)?,
                    &start,
                    &stop,
                    &PredefinedMenuItem::separator(app)?,
                    &quit,
                    &quit_all,
                ],
            )?;
            let tray = TrayIconBuilder::with_id(TRAY_ID)
                .icon(app.default_window_icon().expect("app icon").clone())
                .tooltip("Fetchpath")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "show" => show_main_window(app),
                    // A refusal (the web UI is off, the engine is down) is
                    // shown in the window, where it can be turned on.
                    "web" => {
                        let app = app.clone();
                        let engine = Arc::clone(&app.state::<Arc<EngineLink>>());
                        tauri::async_runtime::spawn(async move {
                            if let Err(message) = off_thread(move || open_web_ui_now(&engine)).await {
                                show_main_window(&app);
                                let _ = app.emit("fetchpath://web-ui-problem", message);
                            }
                        });
                    }
                    // The window and tray go; downloads carry on in the
                    // engine, which stops by itself once it has nothing left
                    // to do.
                    "quit" => app.exit(0),
                    // The person chose to stop the engine too: it goes first
                    // (off the event loop, it may wait on the pipe), then
                    // the desktop. Queued downloads wait for the next start.
                    "quit_all" => {
                        let app = app.clone();
                        let engine = Arc::clone(&app.state::<Arc<EngineLink>>());
                        tauri::async_runtime::spawn(async move {
                            let _ = off_thread(move || engine.stop().map_err(text)).await;
                            app.exit(0);
                        });
                    }
                    "start" => app.state::<Arc<EngineLink>>().allow_start(),
                    // Stopping loses nothing but is not undone by showing
                    // the window again, so the page asks first.
                    "stop" => {
                        show_main_window(app);
                        let _ = app.emit("fetchpath://confirm-stop", ());
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
            fit_main_window_to_monitor(app.handle());
            Ok(())
        })
        .on_window_event(|window, event| {
            // Shown again by a second launch or the tray: the page refreshes
            // at once instead of on its next, possibly slowed, timer tick.
            if let WindowEvent::Focused(true) = event {
                let _ = window.app_handle().emit("fetchpath://queue", ());
            }
            if let WindowEvent::CloseRequested { api, .. } = event {
                // Closing to the tray is the default, so the window is quick to
                // bring back. A user who turned that off means the close button
                // to close. Either way downloads carry on in the engine.
                let to_tray = window.app_handle().state::<CloseToTray>();
                if to_tray.0.load(Ordering::SeqCst) {
                    api.prevent_close();
                    // A second launch re-shows this window with raw Win32
                    // (single_instance), which leaves tao believing it is
                    // still hidden; hide() would then be a no-op. show()
                    // first brings tao's flag back in line with reality.
                    let _ = window.show();
                    let _ = window.hide();
                } else {
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

/// Sizes and centers the main window relative to the active monitor's logical
/// work area so it fits comfortably on both compact scaled laptops and large displays.
fn fit_main_window_to_monitor(app: &AppHandle) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    let Ok(Some(monitor)) = window.current_monitor() else {
        let _ = window.center();
        return;
    };
    let scale = monitor.scale_factor().max(0.5);
    let work = monitor.work_area();
    let avail_w = (f64::from(work.size.width) / scale).max(520.0);
    let avail_h = (f64::from(work.size.height) / scale).max(500.0);
    let max_w = (avail_w - 48.0).max(520.0);
    let max_h = (avail_h - 48.0).max(500.0);
    let target_w = (avail_w * 0.78).clamp(680.0, 1180.0).min(max_w);
    let target_h = (avail_h * 0.82).clamp(540.0, 820.0).min(max_h);
    let _ = window.set_size(tauri::LogicalSize::new(target_w.round(), target_h.round()));
    let _ = window.center();
}

fn show_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn web_ui_link_names_only_the_loopback_page() {
        assert!(is_web_ui_link("http://fetchpath.localhost:47474/open?t=x"));
        for bad in [
            "http://fetchpath.localhost:47474@other.host/open",
            "http://fetchpath.localhost:x@other.host/",
            "http://fetchpath.localhost:/open",
            "http://fetchpath.localhost:47474",
            "http://fetchpath.localhost.other:47474/",
            "https://fetchpath.localhost:47474/",
            "http://fetchpath.localhost:123456/",
        ] {
            assert!(!is_web_ui_link(bad), "{bad}");
        }
    }

    fn failed(destination: &str, checksum: Option<&str>) -> JobSnapshot {
        serde_json::from_value(serde_json::json!({
            "job_id": "3f1c2a9b-0000-4000-8000-000000000001",
            "kind": "file",
            "state": "failed",
            "job_revision": 3,
            "last_seq": 3,
            "source_display": "https://example.test/a.zip?…",
            "destination": destination,
            "expected_sha256": checksum,
            "progress": { "bytes_received": 0 },
            "created_at": "2026-09-25T10:00:00Z",
        }))
        .unwrap()
    }

    #[test]
    fn a_retry_from_the_interface_sends_only_what_changed() {
        let here = r"C:\Downloads\a.zip";
        let sum = "ab".repeat(32);
        let job = failed(here, Some(&sum));
        let retry = |url: Option<&str>, destination: Option<&str>, checksum: Option<&str>| {
            retry_command(
                &job,
                url.map(str::to_owned),
                destination.map(str::to_owned),
                checksum.map(str::to_owned),
            )
        };

        // The retry button, and the edit dialog with nothing changed.
        for command in [
            retry(None, None, None).unwrap(),
            retry(None, Some(here), Some(&sum.to_uppercase())).unwrap(),
        ] {
            assert!(matches!(
                command,
                Command::Retry {
                    expected_sha256: None,
                    ..
                }
            ));
        }
        // A corrected checksum; an emptied one removes it.
        let Command::Retry {
            expected_sha256: Some(new),
            ..
        } = retry(None, Some(here), Some(&"cd".repeat(32))).unwrap()
        else {
            panic!()
        };
        assert_eq!(new, "cd".repeat(32));
        assert!(matches!(
            retry(None, Some(here), Some("")).unwrap(),
            Command::Retry { expected_sha256: Some(cleared), .. } if cleared.is_empty()
        ));
        // A new destination alone.
        assert!(matches!(
            retry(None, Some(r"D:\b.zip"), Some(&sum)).unwrap(),
            Command::ResolveDestination {
                decision: DestinationDecision::ChooseNewPath { path, expected_sha256: None },
                ..
            } if path == r"D:\b.zip"
        ));
        // A new link carries the destination and checksum in one step.
        let Command::RefreshSource {
            source: JobInput::Url { url },
            destination,
            expected_sha256,
            ..
        } = retry(
            Some("https://example.test/a.zip?sig=2"),
            Some(r"D:\b.zip"),
            Some(""),
        )
        .unwrap()
        else {
            panic!()
        };
        assert_eq!(url.expose(), "https://example.test/a.zip?sig=2");
        assert_eq!(destination.as_deref(), Some(r"D:\b.zip"));
        assert_eq!(expected_sha256.as_deref(), Some(""));
        // A new destination and a corrected checksum, without a new link.
        assert!(matches!(
            retry(None, Some(r"D:\b.zip"), Some(&"cd".repeat(32))).unwrap(),
            Command::ResolveDestination {
                decision: DestinationDecision::ChooseNewPath {
                    path,
                    expected_sha256: Some(new),
                },
                ..
            } if path == r"D:\b.zip" && new == "cd".repeat(32)
        ));
    }

    #[test]
    fn the_window_lock_is_not_the_engines() {
        let path = single_instance::window_lock_path().unwrap();
        assert_eq!(path.file_name().unwrap(), "desktop-window.lock");
    }

    #[test]
    fn a_second_instance_is_refused_while_the_first_holds_the_lock() {
        use std::fs::OpenOptions;
        use std::os::windows::fs::OpenOptionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("desktop-window.lock");

        // The first claim also creates the application-data directory.
        assert!(single_instance::claim(&path));
        assert!(path.exists());

        // A second window is modelled by an independent deny-sharing open of
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
