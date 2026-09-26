//! The interactive terminal (FP-059): `fetchpath` with no arguments.
//!
//! Inline mode keeps normal scrollback: finished downloads print there as
//! one-line receipts, and a live panel with a prompt sits below them.
//! `--plain` (automatic under `NO_COLOR`, a dumb `TERM` or a screen reader)
//! prints append-only lines instead. Both are clients of the engine; leaving
//! never stops a download.

mod dashboard;
mod flows;
mod inline;
mod jobs_menu;
mod line;
pub(crate) mod live;
mod menu;
mod plain;
mod prompt;
mod review;
mod settings_menu;
mod setup;
mod view;

use crate::client::{self, Engine};
use crate::download::EXIT_USAGE;
use fetchpath_protocol::command::{Command, JobFilter};
use fetchpath_protocol::message::{CommandResult, EventPayload};
use fetchpath_protocol::{EventStream, ProtocolError, StreamItem};
use live::{Live, Notice};
use std::io::IsTerminal;
use std::time::{Duration, Instant};

pub fn run(args: &[String], help: &str) -> i32 {
    let mut plain = false;
    for arg in args {
        match arg.as_str() {
            "--plain" => plain = true,
            // Scripts get the help, never an interactive screen.
            "--json" => {
                eprintln!("{help}");
                return EXIT_USAGE;
            }
            other => {
                eprintln!("fetchpath: unknown option {other}\nRun `fetchpath --help` for usage.");
                return EXIT_USAGE;
            }
        }
    }
    if !(std::io::stdin().is_terminal() && std::io::stdout().is_terminal()) {
        eprintln!("{help}");
        return EXIT_USAGE;
    }
    let session = match Session::open() {
        Ok(session) => session,
        Err(error) => return client::fail(&error, false),
    };
    if plain || plain_by_environment() {
        plain::run(session)
    } else {
        inline::run(session)
    }
}

/// `NO_COLOR` with any value, a dumb terminal, or a screen reader running.
fn plain_by_environment() -> bool {
    let set = |name: &str| std::env::var_os(name).is_some_and(|value| !value.is_empty());
    set("NO_COLOR")
        || std::env::var("TERM").is_ok_and(|term| term == "dumb")
        || screen_reader_running()
}

#[cfg(windows)]
fn screen_reader_running() -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::{SPI_GETSCREENREADER, SystemParametersInfoW};
    let mut running: i32 = 0;
    // SAFETY: SPI_GETSCREENREADER writes one BOOL through the pointer,
    // which points at a live, writable i32.
    let ok = unsafe {
        SystemParametersInfoW(SPI_GETSCREENREADER, 0, (&mut running as *mut i32).cast(), 0)
    };
    ok != 0 && running != 0
}

#[cfg(not(windows))]
fn screen_reader_running() -> bool {
    false
}

/// The engine connection, the queue stream and the queue as last seen.
pub struct Session {
    engine: Engine,
    events: Box<dyn EventStream>,
    live: Live,
    /// Setting names for completion, in the command line's spelling.
    settings: Vec<String>,
    /// Whether the video tools are ready, checked once in the background
    /// (running the helpers takes a moment).
    tools_check: Option<std::sync::mpsc::Receiver<fetchpath_media::setup::ToolsStatus>>,
    /// What the check found, once it has.
    tools_ready: Option<bool>,
    /// Recent speeds per download, for the dashboard's graph.
    speeds: dashboard::Speeds,
}

impl Session {
    fn open() -> Result<Self, ProtocolError> {
        let engine = Engine::connect()?;
        let mut live = Live::default();
        let (events, _) = Self::sync(&engine, &mut live)?;
        let settings = crate::queue::setting_names(&engine).unwrap_or_default();
        let (found, tools_check) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            if let Ok(status) = Engine::connect().and_then(|engine| crate::tools::status(&engine)) {
                let _ = found.send(status);
            }
        });
        Ok(Self {
            engine,
            events,
            live,
            settings,
            tools_check: Some(tools_check),
            tools_ready: None,
            speeds: dashboard::Speeds::default(),
        })
    }

    /// Subscribes from the engine's current cursor, then merges a full
    /// listing into `live`. Events the listing already reflects are skipped
    /// by `seq`, so nothing between the two is lost or applied twice.
    /// Returns receipts for jobs that finished since `live` last saw them.
    fn sync(
        engine: &Engine,
        live: &mut Live,
    ) -> Result<(Box<dyn EventStream>, Vec<Notice>), ProtocolError> {
        let cursor = match engine.send(Command::EngineStatus)? {
            CommandResult::EngineStatus { status } => status.queue_cursor,
            other => return Err(client::unexpected(&other)),
        };
        let subscription = engine.subscribe(Command::SubscribeQueue {
            after_cursor: cursor,
        })?;
        let jobs = engine.jobs(JobFilter::All)?;
        if live.jobs().is_empty() {
            *live = Live::new(jobs);
            return Ok((subscription.events, Vec::new()));
        }
        let gone: Vec<_> = live
            .jobs()
            .iter()
            .filter(|held| !jobs.iter().any(|job| job.job_id == held.job_id))
            .map(|held| held.job_id.clone())
            .collect();
        for job_id in gone {
            live.remove(&job_id);
        }
        // Refresh puts unknown jobs at the front, so walk the listing
        // oldest first to keep the engine's order.
        let mut notices = Vec::new();
        for job in jobs.into_iter().rev() {
            notices.extend(live.refresh(job));
        }
        Ok((subscription.events, notices))
    }

    /// Reconnects after the stream ends, starting the engine again if it
    /// has gone, and returns receipts for anything that finished meanwhile.
    fn reconnect(&mut self) -> Result<Vec<Notice>, ProtocolError> {
        let engine = Engine::connect()?;
        let (events, notices) = Self::sync(&engine, &mut self.live)?;
        self.engine = engine;
        self.events = events;
        Ok(notices)
    }

    /// Applies what the engine has sent, waiting up to `wait` for the first
    /// item and never holding the caller much longer than that.
    fn poll(&mut self, wait: Duration) -> Result<Vec<Notice>, ProtocolError> {
        let started = Instant::now();
        let mut notices = Vec::new();
        // The pipe treats a zero wait as "do not read", so never ask for
        // less than a millisecond.
        const LEAST: Duration = Duration::from_millis(1);
        let mut wait = wait.max(LEAST);
        loop {
            let item = match self.events.next_item(wait) {
                Ok(item) => item,
                Err(_) => {
                    notices.extend(self.reconnect()?);
                    return Ok(notices);
                }
            };
            wait = LEAST;
            match item {
                None => break,
                Some(StreamItem::Progress(sample)) => {
                    self.speeds.record(
                        &sample.job_id,
                        Instant::now(),
                        sample.public_payload.rate_bytes_per_second,
                    );
                    self.live.progress(&sample);
                }
                Some(StreamItem::Event(event)) => {
                    let (new, found) = self.live.apply(&event);
                    notices.extend(found);
                    // Fetch fields no event carries: retry times, errors
                    // with their actions, the saved path.
                    let refresh = matches!(
                        event.payload,
                        EventPayload::StateChanged { .. }
                            | EventPayload::ErrorRecorded { .. }
                            | EventPayload::PolicyChanged { .. }
                            | EventPayload::PublicationCompleted { .. }
                    );
                    if new && refresh {
                        match self.engine.job(&event.job_id) {
                            Ok(job) => notices.extend(self.live.refresh(job)),
                            Err(error) if error.code.as_str() == "contract.unknown_job" => {
                                self.live.remove(&event.job_id);
                            }
                            Err(error) => return Err(error),
                        }
                    }
                }
            }
            if started.elapsed() > Duration::from_millis(100) {
                break;
            }
        }
        Ok(notices)
    }

    /// Once, when the background check finds the video tools missing, what
    /// to say about it.
    fn tools_notice(&mut self) -> Option<Vec<String>> {
        let status = self.tools_check.as_ref()?.try_recv();
        match status {
            Ok(status) => {
                self.tools_check = None;
                self.tools_ready = Some(status.ready);
                (!status.ready).then(|| {
                    vec![
                        "Saving video and audio needs yt-dlp and ffmpeg, free programs that are not part of Fetchpath and are not set up yet.".to_owned(),
                        "Type /tools install to see what would be downloaded and set them up.".to_owned(),
                    ]
                })
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => None,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.tools_check = None;
                None
            }
        }
    }

    fn run_line(&self, text: &str, menus: bool) -> line::Reply {
        line::run(&self.engine, self.live.jobs(), text, menus)
    }
}
