pub mod browser_bridge;
pub mod browser_setup;
pub mod engine_link;
pub use fetchpath_media::setup as media_setup;
pub mod view;

use engine_link::{EngineLink, Signal};
use fetchpath_protocol::command::{Command, DestinationDecision, JobFilter, JobInput};
use fetchpath_protocol::launch::EngineHome;
use fetchpath_protocol::message::{CommandResult, ControlOutcome};
use fetchpath_protocol::model::{self, RuleSpec};
use fetchpath_protocol::principal::{AgentName, AgentPolicy};
use fetchpath_protocol::{JobId, JobSnapshot, ProtocolError, SensitiveUrl, Timestamp};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, State, WindowEvent};

/// Longest folder path accepted from the interface.
const MAX_PATH_LENGTH: usize = 4_096;

type Engine<'a> = State<'a, Arc<EngineLink>>;

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
async fn off_thread<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(work)
        .await
        .map_err(|error| format!("Fetchpath could not finish that: {error}"))?
}

/// The interface shows errors as text.
fn text(error: ProtocolError) -> String {
    error.message
}

fn unexpected(result: &CommandResult) -> String {
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

fn job_id(value: &str) -> Result<JobId, String> {
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

/// Changes settings starting from what the engine holds now.
fn change_settings(
    app: &AppHandle,
    engine: &EngineLink,
    change: impl FnOnce(&mut view::Settings),
) -> Result<view::SettingsView, String> {
    let current = settings_of(app, engine.send(Command::GetSettings).map_err(text)?)?;
    let mut next = current.settings;
    change(&mut next);
    settings_of(
        app,
        engine
            .send(Command::UpdateSettings {
                settings: next.to_engine(),
            })
            .map_err(text)?,
    )
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

#[tauri::command]
async fn media_tools_status(
    app: AppHandle,
    engine: Engine<'_>,
) -> Result<media_setup::ToolsStatus, String> {
    let engine = Arc::clone(&engine);
    off_thread(move || tools_status(&app, &engine)).await
}

fn tools_status(app: &AppHandle, engine: &EngineLink) -> Result<media_setup::ToolsStatus, String> {
    let install_dir = media_tools_install_dir()?;
    let view = settings_of(app, engine.send(Command::GetSettings).map_err(text)?)?;
    Ok(media_setup::status(
        view.settings.media_tools_dir.as_deref(),
        &install_dir,
    ))
}

#[tauri::command]
async fn install_media_tools(
    app: AppHandle,
    engine: Engine<'_>,
) -> Result<media_setup::ToolsStatus, String> {
    let engine = Arc::clone(&engine);
    off_thread(move || {
        let install_dir = media_tools_install_dir()?;
        media_setup::install(&install_dir)?;
        change_settings(&app, &engine, |settings| {
            settings.media_tools_dir = Some(install_dir.display().to_string());
        })?;
        tools_status(&app, &engine)
    })
    .await
}

#[tauri::command]
async fn use_media_tools_dir(
    directory: String,
    app: AppHandle,
    engine: Engine<'_>,
) -> Result<media_setup::ToolsStatus, String> {
    let engine = Arc::clone(&engine);
    off_thread(move || {
        if directory.len() > MAX_PATH_LENGTH {
            return Err("That folder path is too long.".into());
        }
        let accepted = media_setup::use_directory(&PathBuf::from(&directory))?;
        change_settings(&app, &engine, |settings| {
            settings.media_tools_dir = Some(accepted);
        })?;
        tools_status(&app, &engine)
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

/// Opens only Fetchpath's own project page in the system browser.
#[tauri::command]
fn open_project_page() -> Result<(), String> {
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    let url: Vec<u16> = "https://github.com/idcesares/fetchpath"
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
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
    if (result as isize) <= 32 {
        return Err("Could not open the project page in your browser.".into());
    }
    Ok(())
}

/// The media helpers live in the engine's data folder, where it looks.
fn media_tools_install_dir() -> Result<PathBuf, String> {
    EngineHome::from_env()
        .map(|home| home.dir().join("media-tools"))
        .map_err(|error| {
            format!(
                "Could not resolve the Fetchpath data folder: {}",
                error.message
            )
        })
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

fn agent_views(result: CommandResult) -> Result<Vec<view::AgentView>, String> {
    match result {
        CommandResult::AgentPolicies { policies } => Ok(view::agents(&policies)),
        other => Err(unexpected(&other)),
    }
}

fn agent_name(name: &str) -> Result<AgentName, String> {
    AgentName::try_from(name.trim())
        .map_err(|message| format!("That name cannot be used: {message}."))
}

#[tauri::command]
async fn list_agents(engine: Engine<'_>) -> Result<Vec<view::AgentView>, String> {
    let engine = Arc::clone(&engine);
    off_thread(move || agent_views(engine.send(Command::GetAgentPolicies).map_err(text)?)).await
}

/// Sets one agent's folders and limits. Its unfinished downloads outside
/// the new folders wait for approval again (engine, D4).
#[tauri::command]
async fn set_agent(
    name: String,
    folders: Vec<String>,
    max_bytes: u64,
    max_new_jobs_per_hour: u32,
    automatic: Option<bool>,
    engine: Engine<'_>,
) -> Result<Vec<view::AgentView>, String> {
    let engine = Arc::clone(&engine);
    off_thread(move || {
        let command = Command::SetAgentPolicy {
            agent: agent_name(&name)?,
            policy: Some(AgentPolicy {
                folders,
                max_bytes,
                max_new_jobs_per_hour,
                automatic: automatic.unwrap_or(false),
            }),
        };
        agent_views(engine.send(command).map_err(text)?)
    })
    .await
}

/// Removes an agent's access; everything it has not finished waits for the
/// person again.
#[tauri::command]
async fn revoke_agent(name: String, engine: Engine<'_>) -> Result<Vec<view::AgentView>, String> {
    let engine = Arc::clone(&engine);
    off_thread(move || {
        let command = Command::SetAgentPolicy {
            agent: agent_name(&name)?,
            policy: None,
        };
        agent_views(engine.send(command).map_err(text)?)
    })
    .await
}

fn rule_views(result: CommandResult) -> Result<Vec<view::RuleView>, String> {
    match result {
        CommandResult::Rules { rules } => Ok(view::rules(&rules)),
        other => Err(unexpected(&other)),
    }
}

#[tauri::command]
async fn list_rules(engine: Engine<'_>) -> Result<Vec<view::RuleView>, String> {
    let engine = Arc::clone(&engine);
    off_thread(move || rule_views(engine.send(Command::ListRules).map_err(text)?)).await
}

/// Adds a rule last. The engine checks and normalizes it (FP-064).
#[tauri::command]
async fn add_rule(rule: RuleSpec, engine: Engine<'_>) -> Result<Vec<view::RuleView>, String> {
    let engine = Arc::clone(&engine);
    off_thread(move || {
        let command = Command::AddRule {
            rule: Box::new(rule),
            position: None,
        };
        rule_views(engine.send(command).map_err(text)?)
    })
    .await
}

#[tauri::command]
async fn remove_rule(rule_id: u32, engine: Engine<'_>) -> Result<Vec<view::RuleView>, String> {
    let engine = Arc::clone(&engine);
    off_thread(move || rule_views(engine.send(Command::RemoveRule { rule_id }).map_err(text)?))
        .await
}

fn lan_view(result: CommandResult) -> Result<model::LanView, String> {
    match result {
        CommandResult::Lan { lan } => Ok(lan),
        other => Err(unexpected(&other)),
    }
}

/// Paired computers and sharing, kept by the engine (FP-033).
#[tauri::command]
async fn lan_status(engine: Engine<'_>) -> Result<model::LanView, String> {
    let engine = Arc::clone(&engine);
    off_thread(move || lan_view(engine.send(Command::LanStatus).map_err(text)?)).await
}

#[tauri::command]
async fn set_lan_sharing(enabled: bool, engine: Engine<'_>) -> Result<model::LanView, String> {
    let engine = Arc::clone(&engine);
    off_thread(move || {
        lan_view(
            engine
                .send(Command::SetLanSharing { enabled })
                .map_err(text)?,
        )
    })
    .await
}

#[tauri::command]
async fn start_pairing(engine: Engine<'_>) -> Result<model::LanView, String> {
    let engine = Arc::clone(&engine);
    off_thread(move || lan_view(engine.send(Command::StartPairing).map_err(text)?)).await
}

#[tauri::command]
async fn cancel_pairing(engine: Engine<'_>) -> Result<model::LanView, String> {
    let engine = Arc::clone(&engine);
    off_thread(move || lan_view(engine.send(Command::CancelPairing).map_err(text)?)).await
}

#[tauri::command]
async fn join_pairing(
    address: String,
    code: String,
    label: Option<String>,
    engine: Engine<'_>,
) -> Result<model::PairedDevice, String> {
    let engine = Arc::clone(&engine);
    off_thread(move || {
        let command = Command::JoinPairing {
            address,
            code,
            label: label.filter(|label| !label.trim().is_empty()),
        };
        match engine.send(command).map_err(text)? {
            CommandResult::Joined { device } => Ok(device),
            other => Err(unexpected(&other)),
        }
    })
    .await
}

#[tauri::command]
async fn unpair_device(key: String, engine: Engine<'_>) -> Result<model::LanView, String> {
    let engine = Arc::clone(&engine);
    off_thread(move || lan_view(engine.send(Command::Unpair { key }).map_err(text)?)).await
}

fn cache_view(result: CommandResult) -> Result<model::CacheView, String> {
    match result {
        CommandResult::Cache { cache } => Ok(cache),
        other => Err(unexpected(&other)),
    }
}

/// How much the content cache holds, and its quota bounds (FP-032).
#[tauri::command]
async fn cache_status(engine: Engine<'_>) -> Result<model::CacheView, String> {
    let engine = Arc::clone(&engine);
    off_thread(move || cache_view(engine.send(Command::CacheStatus).map_err(text)?)).await
}

#[tauri::command]
async fn clear_cache(engine: Engine<'_>) -> Result<model::CacheView, String> {
    let engine = Arc::clone(&engine);
    off_thread(move || cache_view(engine.send(Command::ClearCache).map_err(text)?)).await
}

/// How the rules decide for a link. The engine reads the link's headers
/// (not its body) so a size rule can decide.
#[tauri::command]
async fn inspect_rules(url: String, engine: Engine<'_>) -> Result<view::RuleAdvice, String> {
    let engine = Arc::clone(&engine);
    off_thread(move || {
        let url = SensitiveUrl::try_from(url)
            .map_err(|message| format!("That link cannot be used: {message}."))?;
        match engine.send(Command::InspectLink { url }).map_err(text)? {
            CommandResult::LinkInspection { inspection } => {
                Ok(view::rule_advice(inspection.rules.as_ref()))
            }
            other => Err(unexpected(&other)),
        }
    })
    .await
}

/// One Fetchpath window per Windows user account.
///
/// The queue belongs to the engine, which holds its own single-owner lock; this
/// guard only keeps a second window (and a second tray icon) from opening.
/// It is a deny-sharing handle on `desktop-window.lock` in the same folder:
/// whoever opens it first keeps it for the life of the process.
///
/// It fails open. Only a sharing or locking violation means "another window is
/// open"; any other error (an unwritable directory, a missing `%APPDATA%`)
/// lets the application start, because two windows are only untidy: both are
/// clients of the one engine.
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
            // Hidden-to-tray needs SW_SHOW; minimized needs SW_RESTORE.
            ShowWindow(window, SW_SHOW);
            ShowWindow(window, SW_RESTORE);
            SetForegroundWindow(window);
        }
    }
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
            reveal_extension_folder,
            open_project_page,
            approve_download,
            deny_download,
            list_agents,
            set_agent,
            revoke_agent,
            list_rules,
            add_rule,
            remove_rule,
            inspect_rules,
            cache_status,
            clear_cache,
            inspect_repository,
            add_repository,
            lan_status,
            set_lan_sharing,
            start_pairing,
            cancel_pairing,
            join_pairing,
            unpair_device
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
            let tray_status = status.clone();
            let summary_link = Arc::clone(&engine);
            let show_summary = move |handle: &tauri::AppHandle, text: String| {
                let _ = tray_status.set_text(&text);
                if let Some(tray) = handle.tray_by_id(TRAY_ID) {
                    let _ = tray.set_tooltip(Some(text));
                }
            };
            // Runs for the life of the window.
            let _watcher = engine_link::watch(engine, move |signal| {
                if matches!(signal, Signal::Queue | Signal::Connected)
                    && let Some(text) = tray_summary(&summary_link)
                {
                    show_summary(&handle, text);
                }
                let state = match signal {
                    Signal::Queue => {
                        let _ = handle.emit("fetchpath://queue", ());
                        return;
                    }
                    Signal::Connected => serde_json::json!({ "connected": true }),
                    Signal::ReadOnly(reason) => {
                        serde_json::json!({ "connected": true, "readOnly": reason })
                    }
                    Signal::Disconnected { message, stopped } => {
                        serde_json::json!({ "connected": false, "stopped": stopped, "message": message })
                    }
                };
                if state["connected"] != true {
                    show_summary(
                        &handle,
                        if state["stopped"] == true {
                            "Fetchpath: engine stopped. Open Fetchpath to start it.".into()
                        } else {
                            "Fetchpath: engine not running. Open Fetchpath to start it.".into()
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
            let show = MenuItem::with_id(app, "show", "Show Fetchpath", true, None::<&str>)?;
            let quit = MenuItem::with_id(
                app,
                "quit",
                "Close desktop and tray (downloads continue)",
                true,
                None::<&str>,
            )?;
            let stop = MenuItem::with_id(
                app,
                "stop",
                "Stop engine (stops downloads)…",
                true,
                None::<&str>,
            )?;
            let menu = Menu::with_items(
                app,
                &[
                    &status,
                    &PredefinedMenuItem::separator(app)?,
                    &show,
                    &PredefinedMenuItem::separator(app)?,
                    &quit,
                    &stop,
                ],
            )?;
            let tray = TrayIconBuilder::with_id(TRAY_ID)
                .icon(app.default_window_icon().expect("app icon").clone())
                .tooltip("Fetchpath")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "show" => show_main_window(app),
                    // The window and tray go; downloads carry on in the
                    // engine, which stops by itself once it has nothing left
                    // to do.
                    "quit" => app.exit(0),
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
            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                // Closing to the tray is the default, so the window is quick to
                // bring back. A user who turned that off means the close button
                // to close. Either way downloads carry on in the engine.
                let to_tray = window.app_handle().state::<CloseToTray>();
                if to_tray.0.load(Ordering::SeqCst) {
                    api.prevent_close();
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
