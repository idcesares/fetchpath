//! `fetchpath setup` and `fetchpath doctor`: one-place environment readiness
//! check and opt-in guided setup for media tools (yt-dlp, ffmpeg, JS runtime),
//! torrent helper, browser integration, CLI PATH, and AI agent hosts.

use crate::agent_setup;
use crate::agents;
use crate::client::{self, Engine};
use crate::download::EXIT_USAGE;
use crate::tools;
use fetchpath_protocol::command::Command;
use fetchpath_protocol::message::CommandResult;
use serde::Serialize;
use serde_json::{Value, json};
use std::env;
use std::path::{Path, PathBuf};

const USAGE: &str = "Usage: fetchpath setup [--json]
       fetchpath doctor [--json]
       fetchpath setup [--media] [--agents] [--all] [--grant FOLDER] [--yes]

Without action flags, checks every Fetchpath component and requirement without
changing anything: engine & CLI, media tools (yt-dlp, ffmpeg, JS runtime),
torrent helper, browser bridge, and AI agent hosts/grants.

Opt-in setup actions:
  --media         Install pinned yt-dlp and ffmpeg (asks first unless --yes)
  --agents        Connect all detected AI agent hosts in user scope (no grants)
  --all           Run both --media and --agents
  --grant FOLDER  Explicitly grant FOLDER to connected AI agent hosts
  --yes, -y       Do not prompt before downloading pinned media tools";

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetupReport {
    pub version: &'static str,
    pub exe_path: Option<String>,
    pub cli_on_path: bool,
    pub engine_ready: bool,
    pub default_folder: Option<String>,
    pub default_folder_ready: bool,
    pub media: MediaCheck,
    pub torrent: TorrentCheck,
    pub browser: BrowserCheck,
    pub agents: AgentsCheck,
    pub next_steps: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaCheck {
    pub ready: bool,
    pub yt_dlp_version: Option<String>,
    pub ffmpeg_version: Option<String>,
    pub js_runtime_name: Option<String>,
    pub js_runtime_version: Option<String>,
    pub js_runtime_ready: bool,
    pub install_dir: String,
    pub problem: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TorrentCheck {
    pub ready: bool,
    pub helper_path: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserCheck {
    pub host_installed: bool,
    pub extension_present: bool,
    pub extension_dir: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentsCheck {
    pub detected: Vec<String>,
    pub connected: Vec<String>,
    pub ungranted_connected: Vec<String>,
    pub total_granted_agents: usize,
    pub skill_install: &'static str,
}

pub(crate) fn torrent_helper_path() -> Option<PathBuf> {
    if let Some(path) = env::var_os("FETCHPATH_TORRENT_HELPER").map(PathBuf::from)
        && path.is_file()
    {
        return Some(path);
    }
    let name = if cfg!(windows) {
        "fetchpath-torrent-helper.exe"
    } else {
        "fetchpath-torrent-helper"
    };
    env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join(name)))
        .filter(|path| path.is_file())
}

fn cli_on_path() -> bool {
    let target = if cfg!(windows) {
        "fetchpath.exe"
    } else {
        "fetchpath"
    };
    env::var_os("PATH")
        .map(|paths| env::split_paths(&paths).any(|dir| dir.join(target).is_file()))
        .unwrap_or(false)
}

fn browser_check() -> BrowserCheck {
    let exe_dir = env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf));
    let host_installed = exe_dir.as_ref().is_some_and(|dir| {
        let name = if cfg!(windows) {
            "fetchpath-browser-host.exe"
        } else {
            "fetchpath-browser-host"
        };
        dir.join(name).is_file()
    });
    let extension_dir = exe_dir.as_ref().and_then(|dir| {
        [
            dir.join("browser-extension"),
            dir.join("resources").join("browser-extension"),
            dir.join("extension"),
        ]
        .into_iter()
        .find(|candidate| candidate.join("manifest.json").is_file())
    });
    BrowserCheck {
        host_installed,
        extension_present: extension_dir.is_some(),
        extension_dir: extension_dir.map(|p| p.display().to_string()),
    }
}

pub(crate) fn inspect() -> SetupReport {
    let exe_path = env::current_exe().ok().map(|p| p.display().to_string());
    let engine = Engine::connect().ok();
    let engine_ready = engine.is_some();

    let (default_folder, configured_media_dir, agent_policies) = match &engine {
        Some(engine) => {
            let settings = match engine.send(Command::GetSettings) {
                Ok(CommandResult::Settings { view }) => Some(view.settings),
                _ => None,
            };
            let policies = match engine.send(Command::GetAgentPolicies) {
                Ok(CommandResult::AgentPolicies { policies }) => policies,
                _ => Vec::new(),
            };
            let folder = settings
                .as_ref()
                .and_then(|s| s.default_destination_dir.clone())
                .or_else(|| {
                    env::var_os("USERPROFILE")
                        .or_else(|| env::var_os("HOME"))
                        .map(|h| PathBuf::from(h).join("Downloads").display().to_string())
                });
            let media_dir = settings.and_then(|s| s.media_tools_dir);
            (folder, media_dir, policies)
        }
        None => {
            let folder = env::var_os("USERPROFILE")
                .or_else(|| env::var_os("HOME"))
                .map(|h| PathBuf::from(h).join("Downloads").display().to_string());
            (folder, None, Vec::new())
        }
    };

    let default_folder_ready = default_folder
        .as_deref()
        .map(Path::new)
        .is_some_and(Path::is_dir);

    let install_dir = tools::install_dir().unwrap_or_else(|_| PathBuf::from("media-tools"));
    let media_status =
        fetchpath_media::setup::status(configured_media_dir.as_deref(), &install_dir);
    let media = MediaCheck {
        ready: media_status.ready,
        yt_dlp_version: media_status.yt_dlp_version,
        ffmpeg_version: media_status
            .ffmpeg_version
            .as_deref()
            .and_then(|v| v.split_whitespace().nth(2))
            .map(str::to_owned)
            .or(media_status.ffmpeg_version),
        js_runtime_ready: media_status.js_runtime_name.is_some(),
        js_runtime_name: media_status.js_runtime_name,
        js_runtime_version: media_status.js_runtime_version,
        install_dir: media_status.install_dir,
        problem: media_status.problem,
    };

    let torrent_path = torrent_helper_path();
    let torrent = TorrentCheck {
        ready: torrent_path.is_some(),
        helper_path: torrent_path.map(|p| p.display().to_string()),
    };

    let browser = browser_check();

    let mut detected = Vec::new();
    let mut connected = Vec::new();
    if let Ok(status) = agent_setup::status_json()
        && let Some(hosts) = status["hosts"].as_array()
    {
        for host in hosts {
            let Some(id) = host["id"].as_str() else {
                continue;
            };
            if host["available"] == Value::Bool(true) {
                detected.push(id.to_owned());
            }
            if host["registered"] == Value::Bool(true) {
                connected.push(id.to_owned());
            }
        }
    }

    let granted_names: std::collections::BTreeSet<String> = agent_policies
        .iter()
        .filter(|a| !a.policy.folders.is_empty() || a.policy.is_full_access())
        .map(|a| a.agent.as_str().to_owned())
        .collect();
    let ungranted_connected: Vec<String> = connected
        .iter()
        .filter(|id| !granted_names.contains(*id))
        .cloned()
        .collect();

    let mut next_steps = Vec::new();
    if !media.ready {
        next_steps.push("Install video/audio helpers: fetchpath setup --media".into());
    } else if !media.js_runtime_ready {
        next_steps.push(
            "Optional for full YouTube format extraction: install Deno 2.3+ or Node.js 22+ on PATH."
                .into(),
        );
    }
    let unconnected: Vec<&String> = detected
        .iter()
        .filter(|id| !connected.contains(*id))
        .collect();
    if !unconnected.is_empty() {
        next_steps.push(format!(
            "Connect detected AI agent host(s) ({}): fetchpath setup --agents",
            unconnected
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if !ungranted_connected.is_empty() {
        next_steps.push(format!(
            "Grant a download folder to connected agent(s) ({}): fetchpath agents grant {} <FOLDER>",
            ungranted_connected.join(", "),
            ungranted_connected[0]
        ));
    }
    if !torrent.ready {
        next_steps.push(
            "Optional: re-run Fetchpath Setup with the Torrent component to enable magnet/torrent downloads."
                .into(),
        );
    }

    SetupReport {
        version: crate::VERSION,
        exe_path,
        cli_on_path: cli_on_path(),
        engine_ready,
        default_folder,
        default_folder_ready,
        media,
        torrent,
        browser,
        agents: AgentsCheck {
            detected,
            connected,
            ungranted_connected,
            total_granted_agents: granted_names.len(),
            skill_install: "npx skills add idcesares/fetchpath",
        },
        next_steps,
    }
}

fn format_row(badge: &str, label: &str, detail: &str) -> String {
    format!("  {badge:<10} {label:<18} {detail}")
}

/// Formats a `SetupReport` as human-readable lines for CLI output or TUI scrollback.
pub(crate) fn report_lines(report: &SetupReport) -> Vec<String> {
    let mut lines = vec![format!("Fetchpath {} setup check", report.version)];
    lines.push(format_row(
        if report.engine_ready {
            "[ok]"
        } else {
            "[warn]"
        },
        "Engine & CLI",
        if report.engine_ready {
            if report.cli_on_path {
                "engine reachable, fetchpath on PATH"
            } else {
                "engine reachable (fetchpath not on current PATH)"
            }
        } else {
            "engine could not be reached"
        },
    ));
    lines.push(format_row(
        if report.default_folder_ready {
            "[ok]"
        } else {
            "[warn]"
        },
        "Download folder",
        report.default_folder.as_deref().unwrap_or("not configured"),
    ));
    if report.media.ready {
        let js = match (
            &report.media.js_runtime_name,
            &report.media.js_runtime_version,
        ) {
            (Some(name), Some(ver)) => format!("{name} {ver}"),
            (Some(name), None) => name.clone(),
            (None, _) => {
                "no JS runtime (optional; install Deno or Node 22+ for full YouTube formats)".into()
            }
        };
        lines.push(format_row(
            "[ok]",
            "Media tools",
            &format!(
                "yt-dlp {} · ffmpeg {} · JS: {js}",
                report.media.yt_dlp_version.as_deref().unwrap_or("?"),
                report.media.ffmpeg_version.as_deref().unwrap_or("?"),
            ),
        ));
    } else {
        lines.push(format_row(
            "[missing]",
            "Media tools",
            &format!(
                "yt-dlp and ffmpeg not set up{}",
                report
                    .media
                    .js_runtime_name
                    .as_ref()
                    .map(|n| format!(" (JS runtime ready: {n})"))
                    .unwrap_or_default()
            ),
        ));
    }
    lines.push(format_row(
        if report.torrent.ready {
            "[ok]"
        } else {
            "[optional]"
        },
        "Torrent helper",
        if report.torrent.ready {
            "installed"
        } else {
            "not installed beside this fetchpath.exe"
        },
    ));
    lines.push(format_row(
        if report.browser.host_installed && report.browser.extension_present {
            "[ok]"
        } else {
            "[optional]"
        },
        "Browser bridge",
        if report.browser.host_installed && report.browser.extension_present {
            "native host and extension files present"
        } else {
            "install Desktop + Browser integration component to enable"
        },
    ));
    lines.push(format_row(
        if !report.agents.connected.is_empty() && report.agents.ungranted_connected.is_empty() {
            "[ok]"
        } else if !report.agents.detected.is_empty() {
            "[ready]"
        } else {
            "[optional]"
        },
        "AI agents (MCP)",
        &format!(
            "detected: {} | connected: {} | granted: {}",
            if report.agents.detected.is_empty() {
                "none".into()
            } else {
                report.agents.detected.join(", ")
            },
            if report.agents.connected.is_empty() {
                "none".into()
            } else {
                report.agents.connected.join(", ")
            },
            report.agents.total_granted_agents
        ),
    ));

    if !report.next_steps.is_empty() {
        lines.push(String::new());
        lines.push("Suggested next steps:".into());
        for step in &report.next_steps {
            lines.push(format!("  - {step}"));
        }
    } else {
        lines.push(String::new());
        lines.push("Everything checked is ready.".into());
    }
    lines
}

fn print_report(report: &SetupReport, as_json: bool) {
    if as_json {
        client::print_json(report);
        return;
    }
    for line in report_lines(report) {
        println!("{line}");
    }
}

pub fn run(args: &[String]) -> i32 {
    let mut as_json = false;
    let mut do_media = false;
    let mut do_agents = false;
    let mut yes = false;
    let mut grant_folder: Option<String> = None;

    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                println!("{USAGE}");
                return 0;
            }
            "--json" => as_json = true,
            "--media" => do_media = true,
            "--agents" => do_agents = true,
            "--all" => {
                do_media = true;
                do_agents = true;
            }
            "--yes" | "-y" => yes = true,
            "--grant" => match iter.next() {
                Some(folder) => grant_folder = Some(folder.clone()),
                None => {
                    eprintln!("fetchpath setup: --grant needs a folder path.\n{USAGE}");
                    return EXIT_USAGE;
                }
            },
            other if other.starts_with("--grant=") => {
                grant_folder = Some(other["--grant=".len()..].to_owned());
            }
            "check" | "status" => {}
            other => {
                eprintln!("fetchpath setup: unknown option {other}\n{USAGE}");
                return EXIT_USAGE;
            }
        }
    }

    if !do_media && !do_agents && grant_folder.is_none() {
        let report = inspect();
        print_report(&report, as_json);
        return 0;
    }

    if do_media {
        let code = if yes {
            tools::run(&["install".into(), "--yes".into()])
        } else {
            tools::run(&["install".into()])
        };
        if code != 0 {
            return code;
        }
    }

    if do_agents {
        match agent_setup::connect_detected() {
            Ok(connected) if connected.is_empty() => {
                println!(
                    "No supported AI agent hosts detected. Use `fetchpath agent-setup snippet json` for manual setup."
                );
            }
            Ok(connected) => {
                println!(
                    "Connected AI agent host(s): {}. Restart those hosts to load Fetchpath.",
                    connected.join(", ")
                );
            }
            Err(error) => {
                eprintln!("fetchpath setup: {error}");
                return 6;
            }
        }
    }

    if let Some(folder) = grant_folder {
        let report = inspect();
        if report.agents.connected.is_empty() {
            eprintln!(
                "fetchpath setup: no connected agent hosts to grant {folder} to. Run with --agents or `fetchpath agents grant NAME FOLDER`."
            );
            return EXIT_USAGE;
        }
        for host in &report.agents.connected {
            let code = agents::run(&["grant".into(), host.clone(), folder.clone()]);
            if code != 0 {
                return code;
            }
        }
    }

    let final_report = inspect();
    if as_json {
        client::print_json(&json!({ "setup": final_report }));
    } else {
        println!();
        print_report(&final_report, false);
    }
    0
}
