//! The desktop's connection to the engine (FP-055). The desktop owns no
//! queue: every command goes to `fetchpath engine`, and the window learns
//! about changes from the engine's queue event stream.
//!
//! The window starts the engine when it opens, and again after the engine
//! dies. It does not start one that was stopped on purpose (`fetchpath
//! engine stop`, an installer): a stopping engine removes its endpoint file,
//! a dead one leaves it behind. Then, or after three failed starts in a row,
//! the window waits for the person to start it.
//!
//! When the connection is lost, a command is resent to the next engine with
//! the same command id; the engine's ledger makes that safe: a change it
//! already committed is not made twice.

use fetchpath_protocol::client::Subscription;
use fetchpath_protocol::command::{Command, CommandEnvelope};
use fetchpath_protocol::error::{ErrorCode, ErrorScope};
use fetchpath_protocol::launch::{self, EngineHome, LAUNCH_WAIT};
use fetchpath_protocol::message::CommandResult;
use fetchpath_protocol::pipe::{Limits, PipeEngineClient};
use fetchpath_protocol::{ClientId, EngineClient, ProtocolError};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Where the engine program is: `fetchpath.exe`, which the installer puts
/// beside the desktop app. Whether it is there is checked at each launch, so
/// a file restored while the window is open is found.
pub fn engine_exe() -> Option<PathBuf> {
    Some(
        std::env::current_exe()
            .ok()?
            .parent()?
            .join("fetchpath.exe"),
    )
}

pub struct EngineLink {
    home: EngineHome,
    exe: Option<PathBuf>,
    client_id: ClientId,
    client: Mutex<Option<Arc<PipeEngineClient>>>,
    /// One start at a time; held while starting, never while sending.
    starting: Mutex<()>,
    /// Whether this window may start the engine by itself.
    may_start: AtomicBool,
}

/// The connection is gone or the engine is going, so another engine may
/// answer the same command. A timeout is not: the engine may still be at
/// work on it (a long media inspection), and sending it again would repeat
/// that work.
fn lost(error: &ProtocolError) -> bool {
    matches!(
        error.code.as_str(),
        "contract.engine_unavailable"
            | "contract.connection_lost"
            | "auth.handshake_failed"
            | "auth.engine_secret_missing"
    )
}

impl EngineLink {
    pub fn new(home: EngineHome, exe: Option<PathBuf>) -> Self {
        Self {
            home,
            exe,
            client_id: ClientId::random(),
            client: Mutex::new(None),
            starting: Mutex::new(()),
            may_start: AtomicBool::new(true),
        }
    }

    fn held(&self) -> std::sync::MutexGuard<'_, Option<Arc<PipeEngineClient>>> {
        self.client
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn missing_engine() -> ProtocolError {
        ProtocolError::new(
            ErrorCode::ENGINE_UNAVAILABLE,
            ErrorScope::Engine,
            "Fetchpath's engine (fetchpath.exe) is missing. Reinstall Fetchpath to restore it.",
        )
    }

    /// Lets the window start the engine again, after the person asked.
    pub fn allow_start(&self) {
        self.may_start.store(true, Ordering::SeqCst);
    }

    fn may_start(&self) -> bool {
        self.may_start.load(Ordering::SeqCst)
    }

    /// The engine was stopped on purpose: it removed its endpoint file.
    fn stopped_on_purpose(&self) -> bool {
        !self.home.endpoint_path().exists()
    }

    /// The current connection, or a new one to a running engine, which is
    /// started first only while the window may start it.
    fn client(&self) -> Result<Arc<PipeEngineClient>, ProtocolError> {
        if let Some(client) = self.held().as_ref() {
            return Ok(Arc::clone(client));
        }
        let _starting = self
            .starting
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // Another caller may have connected while this one waited.
        if let Some(client) = self.held().as_ref() {
            return Ok(Arc::clone(client));
        }
        let client = Arc::new(if self.may_start() {
            let exe = self
                .exe
                .as_ref()
                .filter(|exe| exe.is_file())
                .ok_or_else(Self::missing_engine)?;
            launch::attach_or_launch(&self.home, exe, Limits::default(), LAUNCH_WAIT)?
        } else {
            launch::attach(&self.home, Limits::default())?
        });
        *self.held() = Some(Arc::clone(&client));
        Ok(client)
    }

    /// Forgets the connection so the next call finds the engine again.
    fn reset(&self, stale: &Arc<PipeEngineClient>) {
        let mut held = self.held();
        if held
            .as_ref()
            .is_some_and(|client| Arc::ptr_eq(client, stale))
        {
            *held = None;
        }
    }

    /// Sends one command; if the engine went away, finds the next one and
    /// sends the same envelope again.
    pub fn send(&self, command: Command) -> Result<CommandResult, ProtocolError> {
        let envelope = CommandEnvelope::new(self.client_id.clone(), command);
        let client = self.client()?;
        match client.execute(&envelope) {
            Err(error) if lost(&error) => {
                self.reset(&client);
                self.client()?.execute(&envelope)
            }
            other => other,
        }
    }

    fn subscribe(&self, command: Command) -> Result<Subscription, ProtocolError> {
        let envelope = CommandEnvelope::new(self.client_id.clone(), command);
        let client = self.client()?;
        client.subscribe(&envelope).inspect_err(|error| {
            if lost(error) {
                self.reset(&client);
            }
        })
    }

    fn forget(&self) {
        *self.held() = None;
    }
}

/// What the window is told.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Signal {
    /// Something in the queue changed.
    Queue,
    /// The engine is reachable (again).
    Connected,
    /// The engine cannot be reached. With `stopped`, the window will not
    /// start it by itself: it waits for the person, or for another client
    /// to start it. Otherwise it keeps trying.
    Disconnected { message: String, stopped: bool },
}

/// How often at most the window is told the queue changed.
const COALESCE: Duration = Duration::from_millis(150);

/// Failed starts in a row before the window stops trying on its own.
const MAX_FAILED_STARTS: u32 = 3;

/// Stops a watcher: it lets go of the engine within about a second.
pub struct Watcher(Arc<AtomicBool>);

impl Watcher {
    pub fn stop(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// Follows the queue until stopped: tells the window when anything changes,
/// and when the engine is lost or back. While it follows, the engine counts
/// a client and stays running.
pub fn watch(link: Arc<EngineLink>, notify: impl Fn(Signal) + Send + 'static) -> Watcher {
    let stop = Arc::new(AtomicBool::new(false));
    let stopped = Arc::clone(&stop);
    std::thread::spawn(move || {
        // What the window was last told, so a state is reported once.
        let mut told: Option<(bool, bool)> = None;
        let mut failed_starts = 0;
        let mut tell = |connected: bool, stopped: bool, message: String| {
            if told != Some((connected, stopped)) {
                told = Some((connected, stopped));
                notify(if connected {
                    Signal::Connected
                } else {
                    Signal::Disconnected { message, stopped }
                });
            }
        };
        while !stopped.load(Ordering::SeqCst) {
            let opened = link
                .send(Command::EngineStatus)
                .and_then(|result| match result {
                    CommandResult::EngineStatus { status } => Ok(status.queue_cursor),
                    _ => Err(ProtocolError::malformed(
                        "unexpected answer to EngineStatus",
                    )),
                })
                .and_then(|cursor| {
                    link.subscribe(Command::SubscribeQueue {
                        after_cursor: cursor,
                    })
                });
            let mut events = match opened {
                Ok(subscription) => subscription.events,
                Err(error) => {
                    link.forget();
                    if link.may_start() {
                        failed_starts += 1;
                        if failed_starts >= MAX_FAILED_STARTS {
                            link.may_start.store(false, Ordering::SeqCst);
                        }
                    }
                    tell(false, !link.may_start(), error.message);
                    std::thread::sleep(Duration::from_secs(1));
                    continue;
                }
            };
            failed_starts = 0;
            // Connected again, however it started: if this engine dies, the
            // window starts the next one.
            link.allow_start();
            tell(true, false, String::new());
            // Whatever happened while it was away is on screen at once.
            notify(Signal::Queue);
            let mut pending = false;
            let mut last = Instant::now();
            while !stopped.load(Ordering::SeqCst) {
                let wait = if pending {
                    COALESCE.saturating_sub(last.elapsed())
                } else {
                    Duration::from_secs(1)
                };
                match events.next_item(wait) {
                    Ok(Some(_)) => pending = true,
                    Ok(None) => {}
                    Err(error) => {
                        link.forget();
                        // A stopping engine removes its endpoint as it decides
                        // to stop, a moment before its connections close.
                        std::thread::sleep(Duration::from_millis(500));
                        if link.stopped_on_purpose() {
                            link.may_start.store(false, Ordering::SeqCst);
                        }
                        tell(false, !link.may_start(), error.message);
                        break;
                    }
                }
                if pending && last.elapsed() >= COALESCE {
                    notify(Signal::Queue);
                    pending = false;
                    last = Instant::now();
                }
            }
        }
    });
    Watcher(stop)
}
