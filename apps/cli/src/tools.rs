//! `fetchpath tools`: the third-party programs Fetchpath uses but does not
//! ship. Video and audio need `yt-dlp` and `ffmpeg`; this shows whether they
//! are ready, and sets them up only after saying what will be downloaded,
//! from where, under which licence, and asking.
//!
//! The installer is the desktop's (`fetchpath_media::setup`): pinned
//! versions, each checked against the SHA-256 recorded in this build before
//! it is installed.

use crate::client::{self, Engine};
use crate::download::EXIT_USAGE;
use crate::queue;
use fetchpath_media::setup::{self, AvailableTool, ToolsStatus};
use fetchpath_protocol::ProtocolError;
use fetchpath_protocol::command::Command;
use fetchpath_protocol::launch::EngineHome;
use fetchpath_protocol::message::CommandResult;
use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

pub const USAGE: &str = "usage: fetchpath tools [status | install [--yes] | use FOLDER]";

/// Where a guided install puts the helpers: the data folder, as the desktop
/// does.
pub fn install_dir() -> Result<PathBuf, ProtocolError> {
    Ok(EngineHome::from_env()?.dir().join("media-tools"))
}

/// Whether the helpers are ready, as the engine will find them.
pub fn status(engine: &Engine) -> Result<ToolsStatus, ProtocolError> {
    let configured = match engine.send(Command::GetSettings)? {
        CommandResult::Settings { view } => view.settings.media_tools_dir,
        other => return Err(client::unexpected(&other)),
    };
    Ok(setup::status(configured.as_deref(), &install_dir()?))
}

/// The site a link is on, for naming a publisher.
fn host(url: &str) -> String {
    url::Url::parse(url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_owned))
        .unwrap_or_else(|| url.to_owned())
}

/// One line per helper: name, version, licence and where it comes from.
pub fn tool_lines(tools: &[AvailableTool]) -> Vec<String> {
    tools
        .iter()
        .map(|tool| {
            let source = if tool.url.contains("github.com") {
                // github.com/OWNER/REPO names the publisher.
                tool.url
                    .split('/')
                    .skip(3)
                    .take(2)
                    .collect::<Vec<_>>()
                    .join("/")
            } else {
                host(&tool.url)
            };
            format!(
                "{:<7} {:<18} {:<18} from {source}",
                tool.name, tool.version, tool.license
            )
        })
        .collect()
}

/// What installing means, said before anything is downloaded.
pub fn disclosure(dir: &Path) -> Vec<String> {
    let mut lines =
        vec!["Video and audio need two free programs that are not part of Fetchpath:".to_owned()];
    lines.extend(
        tool_lines(&setup::pinned_tools())
            .into_iter()
            .map(|line| format!("  {line}")),
    );
    lines.push(format!(
        "They are downloaded from their publishers, checked against the checksums \
         recorded in this Fetchpath, and put in {}.",
        dir.display()
    ));
    lines.push(
        "Fetchpath does not install them itself; they are yours to use under their licences."
            .to_owned(),
    );
    lines
}

/// How the helpers stand, for `fetchpath tools` and `/tools`.
pub fn status_lines(status: &ToolsStatus) -> Vec<String> {
    if status.ready {
        return vec![
            "Video and audio tools are ready:".to_owned(),
            format!(
                "  yt-dlp  {}  {}",
                status.yt_dlp_version.as_deref().unwrap_or("?"),
                status.yt_dlp_path.as_deref().unwrap_or_default()
            ),
            format!(
                "  ffmpeg  {}  {}",
                status
                    .ffmpeg_version
                    .as_deref()
                    .and_then(|version| version.split_whitespace().nth(2))
                    .unwrap_or("?"),
                status.ffmpeg_dir.as_deref().unwrap_or_default()
            ),
        ];
    }
    let mut lines = Vec::new();
    if let Some(problem) = &status.problem {
        lines.push(format!(
            "The video and audio tools were found but do not run: {problem}"
        ));
    } else {
        lines.push("Video and audio need yt-dlp and ffmpeg, which are not set up:".to_owned());
    }
    lines.extend(
        tool_lines(&status.available)
            .into_iter()
            .map(|line| format!("  {line}")),
    );
    lines
}

/// Points the engine at a folder holding the helpers.
pub fn record(engine: &Engine, dir: &str) -> Result<(), ProtocolError> {
    queue::settings_outcome(engine, &["media-tools-dir".to_owned(), dir.to_owned()]).map(|_| ())
}

/// Progress from an install running on its own thread.
pub enum Progress {
    Step {
        label: String,
        token: fetchpath_core::CancellationToken,
    },
    /// What is ready, or why the setup failed.
    Done(Result<Vec<String>, String>),
}

/// Installs into the data folder on a new thread, then points the engine at
/// it. The result is a line saying what is ready.
pub fn install_in_background() -> Result<mpsc::Receiver<Progress>, ProtocolError> {
    let dir = install_dir()?;
    let (send, receive) = mpsc::channel();
    std::thread::spawn(move || {
        let steps = send.clone();
        let result = setup::install_with(&dir, |step| {
            let _ = steps.send(Progress::Step {
                label: format!(
                    "{} {} ({} of {})",
                    step.tool.name, step.tool.version, step.number, step.count
                ),
                token: step.token,
            });
        })
        .and_then(|_| {
            let engine = Engine::connect().map_err(|error| error.message)?;
            record(&engine, &dir.display().to_string()).map_err(|error| error.message)?;
            let ready = status(&engine).map_err(|error| error.message)?;
            Ok(status_lines(&ready))
        });
        let _ = send.send(Progress::Done(result));
    });
    Ok(receive)
}

// ---------------------------------------------------------------- command

pub fn run(args: &[String]) -> i32 {
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    let outcome = match words.as_slice() {
        [] | ["status"] => show(),
        ["install"] => install(false),
        ["install", "--yes" | "-y"] => install(true),
        ["use", folder] => use_folder(folder),
        _ => {
            eprintln!("{USAGE}");
            return EXIT_USAGE;
        }
    };
    match outcome {
        Ok(code) => code,
        Err(error) => client::fail(&error, false),
    }
}

fn show() -> Result<i32, ProtocolError> {
    let engine = Engine::connect()?;
    let status = status(&engine)?;
    for line in status_lines(&status) {
        println!("{line}");
    }
    if !status.ready {
        println!("Set them up:              fetchpath tools install");
        println!("Or use copies you have:   fetchpath tools use FOLDER");
    }
    Ok(0)
}

fn use_folder(folder: &str) -> Result<i32, ProtocolError> {
    let accepted =
        setup::use_directory(Path::new(folder)).map_err(|message| client::input_error(&message))?;
    let engine = Engine::connect()?;
    record(&engine, &accepted)?;
    for line in status_lines(&status(&engine)?) {
        println!("{line}");
    }
    Ok(0)
}

fn install(yes: bool) -> Result<i32, ProtocolError> {
    let engine = Engine::connect()?;
    if status(&engine)?.ready {
        println!("Video and audio tools are already set up.");
        return Ok(0);
    }
    let dir = install_dir()?;
    for line in disclosure(&dir) {
        println!("{line}");
    }
    if !yes {
        if !std::io::stdin().is_terminal() {
            eprintln!("fetchpath: add --yes to install without being asked.");
            return Ok(EXIT_USAGE);
        }
        print!("Download and set them up now? [y/N] ");
        let _ = std::io::stdout().flush();
        let mut answer = String::new();
        let _ = std::io::stdin().lock().read_line(&mut answer);
        if !matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
            println!("Nothing was downloaded.");
            return Ok(0);
        }
    }
    client::catch_interrupt();
    let progress = install_in_background()?;
    let live = std::io::stderr().is_terminal();
    let mut current: Option<(String, fetchpath_core::CancellationToken)> = None;
    loop {
        match progress.recv_timeout(Duration::from_millis(250)) {
            Ok(Progress::Step { label, token }) => {
                if live {
                    eprintln!();
                }
                eprintln!("Downloading {label}");
                current = Some((label, token));
            }
            Ok(Progress::Done(Ok(ready))) => {
                if live {
                    eprintln!();
                }
                for line in ready {
                    println!("{line}");
                }
                return Ok(0);
            }
            Ok(Progress::Done(Err(message))) => {
                if live {
                    eprintln!();
                }
                eprintln!("fetchpath: {message}");
                return Ok(client::EXIT_ENGINE);
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if let Some((_, token)) = &current {
                    if client::interrupted() {
                        token.cancel();
                    }
                    if live {
                        let received = token.received();
                        let amount = match token.total() {
                            Some(total) if total > 0 => format!(
                                "{:5.1}%  {} of {}",
                                received as f64 / total as f64 * 100.0,
                                client::bytes(received),
                                client::bytes(total)
                            ),
                            _ => client::bytes(received),
                        };
                        eprint!("\r  {amount}      ");
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(client::input_error("The setup stopped unexpectedly."));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_disclosure_names_each_program_its_licence_and_publisher() {
        let lines = disclosure(Path::new(r"C:\Users\person\AppData\Roaming\fp\media-tools"));
        let text = lines.join("\n");
        for tool in setup::pinned_tools() {
            assert!(text.contains(&tool.name), "{text}");
            assert!(text.contains(&tool.version), "{text}");
            assert!(text.contains(&tool.license), "{text}");
        }
        assert!(text.contains("yt-dlp/yt-dlp"), "{text}");
        assert!(text.contains(r"fp\media-tools"), "{text}");
        assert!(text.contains("checksums"), "{text}");
    }

    #[test]
    fn a_missing_setup_lists_what_is_needed() {
        let status = ToolsStatus {
            available: setup::pinned_tools(),
            ..ToolsStatus::default()
        };
        let lines = status_lines(&status);
        assert!(lines[0].contains("not set up"));
        assert_eq!(lines.len(), 1 + setup::pinned_tools().len());
    }
}
