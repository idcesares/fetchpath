//! The desktop's connection to the engine (FP-055). The desktop owns no
//! queue: every command goes to `fetchpath engine`, which this starts when it
//! is not running, and the window learns about changes from the engine's
//! queue event stream.
//!
//! When the engine goes away (it was stopped, restarted or killed) the next
//! command reaches a new one and is resent with the same command id, which
//! the engine's ledger makes safe: a change it already committed is not made
//! twice.

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
}

/// The connection is gone or the engine is going; another engine answers
/// the same command.
fn lost(error: &ProtocolError) -> bool {
    matches!(
        error.code.as_str(),
        "contract.engine_unavailable"
            | "contract.connection_lost"
            | "contract.connection_timed_out"
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
        }
    }

    fn missing_engine() -> ProtocolError {
        ProtocolError::new(
            ErrorCode::ENGINE_UNAVAILABLE,
            ErrorScope::Engine,
            "Fetchpath's engine (fetchpath.exe) is missing. Reinstall Fetchpath to restore it.",
        )
    }

    /// The current connection, or a new one to a running engine, starting
    /// it if needed.
    fn client(&self) -> Result<Arc<PipeEngineClient>, ProtocolError> {
        let mut held = self
            .client
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(client) = held.as_ref() {
            return Ok(Arc::clone(client));
        }
        let exe = self
            .exe
            .as_ref()
            .filter(|exe| exe.is_file())
            .ok_or_else(Self::missing_engine)?;
        let client = Arc::new(launch::attach_or_launch(
            &self.home,
            exe,
            Limits::default(),
            LAUNCH_WAIT,
        )?);
        *held = Some(Arc::clone(&client));
        Ok(client)
    }

    /// Forgets the connection so the next call finds the engine again.
    fn reset(&self, stale: &Arc<PipeEngineClient>) {
        let mut held = self
            .client
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if held
            .as_ref()
            .is_some_and(|client| Arc::ptr_eq(client, stale))
        {
            *held = None;
        }
    }

    /// Sends one command; if the engine went away, finds or starts the next
    /// one and sends the same envelope again.
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
        *self
            .client
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
    }
}

/// What the window is told.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Signal {
    /// Something in the queue changed.
    Queue,
    /// The engine is reachable (again).
    Connected,
    /// The engine cannot be reached; the watcher keeps trying.
    Disconnected(String),
}

/// How often at most the window is told the queue changed.
const COALESCE: Duration = Duration::from_millis(150);

/// Stops a watcher: it lets go of the engine within about a second.
pub struct Watcher(Arc<AtomicBool>);

impl Watcher {
    pub fn stop(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// Follows the queue until stopped: tells the window when anything changes,
/// and when the engine is lost or back, starting a new engine when it is
/// lost. While it follows, the engine counts a client and stays running.
pub fn watch(link: Arc<EngineLink>, notify: impl Fn(Signal) + Send + 'static) -> Watcher {
    let stop = Arc::new(AtomicBool::new(false));
    let stopped = Arc::clone(&stop);
    std::thread::spawn(move || {
        let mut connected: Option<bool> = None;
        while !stopped.load(Ordering::SeqCst) {
            let opened = link
                .send(Command::EngineStatus)
                .and_then(|result| match result {
                    CommandResult::EngineStatus { status } => Ok(status.queue_cursor),
                    other => Err(ProtocolError::malformed(format!(
                        "unexpected answer {}",
                        serde_json::to_string(&other).unwrap_or_default()
                    ))),
                })
                .and_then(|cursor| {
                    link.subscribe(Command::SubscribeQueue {
                        after_cursor: cursor,
                    })
                });
            let mut events = match opened {
                Ok(subscription) => subscription.events,
                Err(error) => {
                    if connected != Some(false) {
                        notify(Signal::Disconnected(error.message));
                        connected = Some(false);
                    }
                    link.forget();
                    std::thread::sleep(Duration::from_secs(1));
                    continue;
                }
            };
            // Only a lost stream leaves the loop below, and it records that.
            if connected != Some(true) {
                notify(Signal::Connected);
            }
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
                        notify(Signal::Disconnected(error.message));
                        connected = Some(false);
                        link.forget();
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
