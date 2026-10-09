//! The loopback web UI (FP-104): a listener inside the engine process that
//! serves the same queue view as the desktop to a browser on this computer.
//!
//! Off until the person turns it on. It binds loopback only, hands out a
//! single-use link only to the person's own clients, and treats a signed-in
//! browser as a `device` principal, which the engine holds to viewing and
//! submitting (contract D6). The listener existing never keeps the engine
//! alive; a signed-in socket counts as a client like an open desktop window.

mod assets;
mod bind;
mod http;
mod listener;
mod sessions;
mod socket;
#[cfg(test)]
mod tests;

use fetchpath_protocol::command::{Command, CommandEnvelope};
use fetchpath_protocol::error::{Action, ErrorCode, ErrorScope, ProtocolError};
use fetchpath_protocol::message::CommandResult;
use fetchpath_protocol::principal::{DeviceId, Principal};
use fetchpath_protocol::{ClientId, SensitiveUrl};
use fetchpath_session::engine::Engine;
use sessions::{Sessions, TICKET_TTL, Tickets};
use std::collections::HashMap;
#[cfg(test)]
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

/// Browser sockets open at once.
const MAX_SOCKETS: usize = 32;

/// The canonical host: the only one a session cookie is set on. Browsers
/// resolve `*.localhost` to loopback on their own. A cookie ignores ports, so
/// another local server on this name still receives it; the name only keeps
/// the cookie off `localhost` and `127.0.0.1` servers.
const CANONICAL_HOST: &str = "fetchpath.localhost";

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

struct Live {
    device: String,
    token: CancellationToken,
}

/// What the running listener's connections share.
struct Shared {
    engine: Arc<Engine>,
    port: u16,
    tickets: Mutex<Tickets>,
    sessions: Mutex<Sessions>,
    sockets: Mutex<HashMap<u64, Live>>,
    next_socket: AtomicU64,
    connections: Arc<AtomicUsize>,
    /// Ends the listener and every connection.
    cancel: CancellationToken,
}

impl Shared {
    fn canonical_host(&self) -> String {
        format!("{CANONICAL_HOST}:{}", self.port)
    }

    /// Whether `host` (lowercase, with port) names this listener. A page on
    /// any other name reaching it is a DNS rebinding attempt.
    fn host_allowed(&self, host: &str) -> bool {
        let port = self.port;
        [
            format!("{CANONICAL_HOST}:{port}"),
            format!("localhost:{port}"),
            format!("127.0.0.1:{port}"),
            format!("[::1]:{port}"),
        ]
        .iter()
        .any(|allowed| allowed == host)
    }

    fn open_link(&self) -> Result<SensitiveUrl, ProtocolError> {
        let ticket = lock(&self.tickets).issue().map_err(internal)?;
        SensitiveUrl::try_from(format!(
            "http://{}/open?ticket={ticket}",
            self.canonical_host()
        ))
        .map_err(internal)
    }

    /// A socket for `device`, unless too many are open or its session ended
    /// since the request was checked. Sockets are locked before sessions
    /// everywhere, so a sign-out cannot slip between the check and the entry.
    fn register(&self, device: &DeviceId) -> Option<(u64, CancellationToken)> {
        let mut sockets = lock(&self.sockets);
        if sockets.len() >= MAX_SOCKETS || !lock(&self.sessions).has_device(device) {
            return None;
        }
        let id = self.next_socket.fetch_add(1, Ordering::Relaxed);
        let token = self.cancel.child_token();
        sockets.insert(
            id,
            Live {
                device: device.to_string(),
                token: token.clone(),
            },
        );
        Some((id, token))
    }

    /// Trades a launch ticket for a new session, under the sockets lock like
    /// `sign_out`, so a sign-out cannot land between the two.
    fn sign_in(&self, ticket: &str) -> Option<sessions::Created> {
        let _sockets = lock(&self.sockets);
        if !lock(&self.tickets).redeem(ticket) {
            return None;
        }
        lock(&self.sessions).create(sessions::now_ms()).ok()
    }

    fn unregister(&self, id: u64) {
        lock(&self.sockets).remove(&id);
    }

    fn close_devices(&self, devices: &[String]) {
        for live in lock(&self.sockets).values() {
            if devices.contains(&live.device) {
                live.token.cancel();
            }
        }
    }

    /// Ends every session and closes every socket; the listener stays.
    fn sign_out(&self) {
        let sockets = lock(&self.sockets);
        lock(&self.sessions).clear();
        lock(&self.tickets).clear();
        for live in sockets.values() {
            live.token.cancel();
        }
    }

    fn envelope(&self, client: &ClientId, command: Command) -> CommandEnvelope {
        let envelope = CommandEnvelope::new(client.clone(), command);
        match self.engine.instance_id() {
            Some(instance) => envelope.for_instance(instance.clone()),
            None => envelope,
        }
    }
}

fn internal(message: impl std::fmt::Display) -> ProtocolError {
    ProtocolError::new(
        ErrorCode::INTERNAL_UNKNOWN,
        ErrorScope::Command,
        message.to_string(),
    )
}

struct Running {
    shared: Arc<Shared>,
    #[cfg(test)]
    addresses: Vec<SocketAddr>,
    thread: JoinHandle<()>,
}

/// The engine host's handle on the web UI: starts and stops the listener
/// with the setting, and answers the two commands only the host can.
pub struct Web {
    engine: Arc<Engine>,
    dir: PathBuf,
    connections: Arc<AtomicUsize>,
    ticket_ttl: Duration,
    running: Mutex<Option<Running>>,
}

impl Web {
    pub fn new(engine: Arc<Engine>, dir: PathBuf, connections: Arc<AtomicUsize>) -> Self {
        sessions::forget_legacy_file(&dir);
        Self {
            engine,
            dir,
            connections,
            ticket_ttl: TICKET_TTL,
            running: Mutex::new(None),
        }
    }

    /// Turns the listener on or off. Either way its sessions end with it.
    pub fn apply(&self, enabled: bool) {
        let mut running = lock(&self.running);
        if enabled {
            if running.is_none() {
                match self.start() {
                    Ok(started) => *running = Some(started),
                    Err(message) => eprintln!("fetchpath engine: {message}"),
                }
            }
        } else {
            if let Some(started) = running.take() {
                stop(started);
            }
        }
    }

    /// Closes the port and every socket, keeping the sessions.
    pub fn shutdown(&self) {
        if let Some(started) = lock(&self.running).take() {
            stop(started);
        }
    }

    fn start(&self) -> Result<Running, String> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| format!("The web UI could not start: {error}"))?;
        let bound = {
            let _context = runtime.enter();
            bind::bind(&self.dir)?
        };
        #[cfg(test)]
        let addresses: Vec<SocketAddr> = bound
            .listeners
            .iter()
            .filter_map(|listener| listener.local_addr().ok())
            .collect();
        let shared = Arc::new(Shared {
            engine: Arc::clone(&self.engine),
            port: bound.port,
            tickets: Mutex::new(Tickets::new(self.ticket_ttl)),
            sessions: Mutex::new(Sessions::default()),
            sockets: Mutex::new(HashMap::new()),
            next_socket: AtomicU64::new(0),
            connections: Arc::clone(&self.connections),
            cancel: CancellationToken::new(),
        });
        let thread = {
            let shared = Arc::clone(&shared);
            std::thread::Builder::new()
                .name("web-ui".into())
                .spawn(move || {
                    runtime.block_on(listener::run(shared, bound.listeners));
                    runtime.shutdown_timeout(Duration::from_secs(2));
                })
                .map_err(|error| format!("The web UI could not start: {error}"))?
        };
        Ok(Running {
            shared,
            #[cfg(test)]
            addresses,
            thread,
        })
    }

    /// The commands only this host answers, or `None` for every other. Only
    /// the person's own clients may open the web UI or sign browsers out; an
    /// agent, the extension or a signed-in browser is refused here.
    pub fn answer(
        &self,
        principal: &Principal,
        command: &Command,
    ) -> Option<Result<CommandResult, ProtocolError>> {
        if !matches!(command, Command::OpenWebUi | Command::SignOutBrowsers) {
            return None;
        }
        if !principal.is_user() {
            return Some(Err(not_permitted(principal, command)));
        }
        let running = lock(&self.running);
        Some(match command {
            Command::OpenWebUi => match running.as_ref() {
                Some(started) => started
                    .shared
                    .open_link()
                    .map(|url| CommandResult::WebUiLink { url }),
                None => Err(ProtocolError::new(
                    ErrorCode::try_from("contract.unsupported".to_owned())
                        .expect("a valid built-in code"),
                    ErrorScope::Command,
                    "The web UI is off. Turn it on first (fetchpath web on).",
                )),
            },
            _ => {
                if let Some(started) = running.as_ref() {
                    started.shared.sign_out();
                }
                Ok(CommandResult::BrowsersSignedOut)
            }
        })
    }

    #[cfg(test)]
    fn addresses(&self) -> Vec<SocketAddr> {
        lock(&self.running)
            .as_ref()
            .map(|started| started.addresses.clone())
            .unwrap_or_default()
    }
}

fn stop(started: Running) {
    started.shared.cancel.cancel();
    let _ = started.thread.join();
}

/// Built as the engine's own policy builds its refusal, since that one is
/// private to the session crate.
fn not_permitted(principal: &Principal, command: &Command) -> ProtocolError {
    ProtocolError::new(
        ErrorCode::try_from("policy.not_permitted".to_owned()).expect("a valid built-in code"),
        ErrorScope::Command,
        format!(
            "{} cannot use {}; only the person can.",
            match principal {
                Principal::Browser => "The browser extension".to_owned(),
                Principal::Agent(name) => format!("The agent {name}"),
                Principal::Device(_) => "A browser session".to_owned(),
                Principal::User => "This client".to_owned(),
            },
            command.name()
        ),
    )
    .with_action(Action::ChangePolicy)
}
