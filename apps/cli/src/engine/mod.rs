//! `fetchpath engine`: the one owner of the queue, behind the authenticated
//! pipe (FP-053).
//!
//! Startup order is what keeps recovery ahead of every command: the
//! single-owner lock first, then the session (restart recovery runs as it
//! loads), one reconcile, and only then the pipe and the endpoint file that
//! tells clients where it is. A client cannot reach the engine before the
//! endpoint exists.

mod signin;

use fetchpath_protocol::command::Command;
use fetchpath_protocol::error::{ErrorCode, ErrorScope};
use fetchpath_protocol::launch::{self, EngineHome};
use fetchpath_protocol::message::{Reply, ReplyResult, ServerMessage};
use fetchpath_protocol::pipe::{
    EngineSecret, Limits, PendingConnection, PipeListener, PipeName, current_user_sid, endpoint,
};
use fetchpath_protocol::{ClientId, EngineClient, ProtocolError, StreamItem};
use fetchpath_session::engine::{Engine, TICK_INTERVAL};
use fetchpath_session::{DEFAULT_MAX_ACTIVE, Session};
use std::fs::File;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How long the engine stays with no client and nothing of its own to do.
pub const IDLE_GRACE: Duration = Duration::from_secs(60);

pub const USAGE: &str = "usage: fetchpath engine [status [--json] | stop]";

pub fn run(args: &[String]) -> i32 {
    match args.first().map(String::as_str) {
        None => host(IDLE_GRACE),
        Some("--idle-grace-ms") => match args.get(1).and_then(|value| value.parse::<u64>().ok()) {
            Some(ms) => host(Duration::from_millis(ms)),
            None => usage(),
        },
        Some("status") => match &args[1..] {
            [] => crate::queue::engine_status(false),
            [flag] if flag == "--json" => crate::queue::engine_status(true),
            _ => usage(),
        },
        Some("stop") => stop(),
        Some(_) => usage(),
    }
}

fn usage() -> i32 {
    eprintln!("{USAGE}");
    2
}

fn home() -> Result<EngineHome, ProtocolError> {
    EngineHome::from_env()
}

fn stop() -> i32 {
    let outcome = home()
        .and_then(|home| launch::attach(&home, Limits::default()))
        .and_then(|client| client.send(&ClientId::random(), Command::EngineShutdown));
    match outcome {
        Ok(_) => {
            println!("The Fetchpath engine is stopping.");
            0
        }
        Err(error) if error.code.as_str() == "contract.engine_unavailable" => {
            println!("The Fetchpath engine is not running.");
            0
        }
        Err(error) => {
            eprintln!("fetchpath: {}", error.message);
            1
        }
    }
}

/// Holds the single-owner lock: a handle nobody else may open. Only a
/// sharing or lock violation means another owner; any other failure also
/// stops the engine, because two owners of one queue would corrupt it.
fn claim(home: &EngineHome) -> Result<Option<File>, String> {
    use std::os::windows::fs::OpenOptionsExt;
    const ERROR_SHARING_VIOLATION: i32 = 32;
    const ERROR_LOCK_VIOLATION: i32 = 33;
    std::fs::create_dir_all(home.dir())
        .map_err(|error| format!("The Fetchpath data folder could not be created: {error}"))?;
    match std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .share_mode(0)
        .open(home.lock_path())
    {
        Ok(file) => Ok(Some(file)),
        Err(error)
            if matches!(
                error.raw_os_error(),
                Some(ERROR_SHARING_VIOLATION | ERROR_LOCK_VIOLATION)
            ) =>
        {
            Ok(None)
        }
        Err(error) => Err(format!(
            "The Fetchpath engine could not claim {}: {error}",
            home.lock_path().display()
        )),
    }
}

struct Host {
    engine: Arc<Engine>,
    /// Set once the engine decides to stop. From then on no command is
    /// carried out; clients are told the engine is unavailable.
    stop: AtomicBool,
    /// Connections accepted and not yet closed, authenticated or not, so a
    /// client still in its handshake keeps the engine from going idle.
    connections: AtomicUsize,
    /// Serializes a settings change with the sign-in start it implies.
    settings: Mutex<()>,
    /// Whether this engine uses the default data folder. Only then does it
    /// touch the person's sign-in start.
    default_home: bool,
    /// The endpoint file this engine published.
    endpoint: std::path::PathBuf,
}

impl Host {
    /// Work the engine does on its own: a job queued, scheduled (including
    /// an automatic retry) or running. Paused jobs and failures waiting for a
    /// person change only when a person acts, which starts a client.
    fn has_own_work(&self) -> bool {
        self.engine.session().has_own_work()
    }

    /// Decides to stop, and says so to clients by removing the endpoint
    /// file at once, while this engine still holds the lock, so no other
    /// engine's endpoint can be removed. A client that loses its connection
    /// and finds no endpoint knows the engine was stopped on purpose; one
    /// that finds it left behind knows the engine died, and may start
    /// another (FP-055).
    fn begin_stop(&self) {
        if !self.stop.swap(true, Ordering::SeqCst) {
            let _ = std::fs::remove_file(&self.endpoint);
        }
    }

    fn stopping(&self) -> bool {
        self.stop.load(Ordering::SeqCst) || INTERRUPTED.load(Ordering::SeqCst)
    }

    fn apply_sign_in(&self, enabled: bool) {
        if self.default_home {
            signin::apply(enabled);
        }
    }
}

/// Set by Ctrl+C on an engine run in a terminal.
static INTERRUPTED: AtomicBool = AtomicBool::new(false);

fn host(grace: Duration) -> i32 {
    let _ = ctrlc::set_handler(|| INTERRUPTED.store(true, Ordering::SeqCst));
    match serve(grace) {
        Ok(code) => code,
        Err(message) => {
            eprintln!("fetchpath engine: {message}");
            1
        }
    }
}

/// Stops the engine's work cleanly: no job starts any more, running ones
/// stop at their checkpoints, and the queue is saved, as quitting the
/// desktop does.
fn wind_down(engine: &Engine) {
    engine.session().halt();
    engine.session().cancel_all_and_join();
}

fn serve(grace: Duration) -> Result<i32, String> {
    let home = home().map_err(|error| error.message)?;
    let Some(_lock) = claim(&home)? else {
        eprintln!("The Fetchpath engine is already running.");
        return Ok(0);
    };

    // Restart recovery happens as the session loads, before any pipe exists.
    // Browser captures are taken in from the inbox beside the queue and
    // saved to Downloads, as the desktop did while it held the queue.
    let session =
        Session::load_with_browser(home.queue_path(), DEFAULT_MAX_ACTIVE, downloads_dir())
            .map_err(|error| format!("The download queue could not be opened: {error}"))?;
    let engine = Engine::new(Arc::new(session));
    engine.tick();

    let opened = (|| {
        let sid = current_user_sid().map_err(|error| error.message)?;
        let secret = EngineSecret::load_or_create(&home.secret_path(), &sid)
            .map_err(|error| error.message)?;
        let name = PipeName::fresh().map_err(|error| error.message)?;
        let listener =
            PipeListener::bind(&name, secret, Limits::default()).map_err(|error| error.message)?;
        endpoint::publish(&home.endpoint_path(), &name, &sid).map_err(|error| error.message)?;
        Ok::<_, String>(listener)
    })();
    let mut listener = match opened {
        Ok(listener) => listener,
        Err(message) => {
            // The first reconcile may already have started work.
            wind_down(&engine);
            return Err(message);
        }
    };

    let host = Arc::new(Host {
        engine,
        stop: AtomicBool::new(false),
        connections: AtomicUsize::new(0),
        settings: Mutex::new(()),
        default_home: std::env::var_os("FETCHPATH_APP_DATA_DIR").is_none(),
        endpoint: home.endpoint_path(),
    });
    if host.engine.session().settings().start_engine_at_sign_in {
        host.apply_sign_in(true);
    }
    let ticker = {
        let host = Arc::clone(&host);
        std::thread::spawn(move || {
            while !host.stopping() {
                host.engine.tick();
                std::thread::sleep(TICK_INTERVAL);
            }
        })
    };

    let mut idle_since = Instant::now();
    while !host.stopping() {
        match listener.accept(Some(TICK_INTERVAL)) {
            Ok(Some(pending)) => {
                // Counted from acceptance, before the handshake.
                host.connections.fetch_add(1, Ordering::SeqCst);
                let host = Arc::clone(&host);
                std::thread::spawn(move || {
                    let _counted = Connected(&host.connections);
                    connection(&host, pending);
                });
            }
            Ok(None) => {}
            // Transient: clients that open and close the pipe quickly.
            Err(_) => std::thread::sleep(Duration::from_millis(50)),
        }
        if host.connections.load(Ordering::SeqCst) > 0 || host.has_own_work() {
            idle_since = Instant::now();
        } else if idle_since.elapsed() >= grace {
            host.begin_stop();
        }
    }
    host.begin_stop();

    // Close the pipe first, so a client arriving now finds no engine and
    // starts the next one instead of stalling in a handshake here.
    drop(listener);
    let _ = ticker.join();
    wind_down(&host.engine);
    Ok(0)
}

/// The person's Downloads folder, wherever they moved it.
fn downloads_dir() -> Option<std::path::PathBuf> {
    use windows_sys::Win32::System::Com::CoTaskMemFree;
    use windows_sys::Win32::UI::Shell::{FOLDERID_Downloads, SHGetKnownFolderPath};
    let mut raw: windows_sys::core::PWSTR = std::ptr::null_mut();
    // SAFETY: the out pointer is valid; the returned buffer is freed with
    // CoTaskMemFree whether or not the call succeeded, as documented.
    let path = unsafe {
        let status = SHGetKnownFolderPath(&FOLDERID_Downloads, 0, std::ptr::null_mut(), &mut raw);
        let path = (status == 0 && !raw.is_null()).then(|| {
            let length = (0..).take_while(|&index| *raw.add(index) != 0).count();
            String::from_utf16_lossy(std::slice::from_raw_parts(raw, length))
        });
        CoTaskMemFree(raw as *const core::ffi::c_void);
        path
    };
    path.map(std::path::PathBuf::from)
        .filter(|path| path.is_dir())
}

/// Counts a connection while it lives.
struct Connected<'a>(&'a AtomicUsize);

impl Drop for Connected<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

fn stopping_error() -> ProtocolError {
    ProtocolError::new(
        ErrorCode::ENGINE_UNAVAILABLE,
        ErrorScope::Engine,
        "The Fetchpath engine is stopping. Try again in a moment.",
    )
}

fn connection(host: &Arc<Host>, pending: PendingConnection) {
    let Ok(connection) = pending.authenticate() else {
        return;
    };
    let principal = connection.principal().clone();
    // Only the person's own clients may replace the engine.
    if connection.restart_requested() {
        if principal.is_user() {
            host.begin_stop();
        }
        return;
    }
    let sender = connection.sender();
    while let Ok(Some(envelope)) = connection.receive() {
        if host.stopping() {
            let _ = sender.send(&ServerMessage::Reply(Reply::error(
                Some(envelope.command_id.clone()),
                stopping_error(),
            )));
            return;
        }
        match &envelope.payload {
            Command::SubscribeJob { .. } | Command::SubscribeQueue { .. } => {
                match host.engine.subscribe_as(&principal, &envelope) {
                    Ok(subscription) => {
                        if sender
                            .send(&ServerMessage::Reply(Reply::ok(
                                envelope.command_id.clone(),
                                subscription.start,
                            )))
                            .is_err()
                        {
                            return;
                        }
                        // A subscriber only listens from here on.
                        connection.set_idle_timeout(None);
                        let mut events = subscription.events;
                        let sender = sender.clone();
                        let host = Arc::clone(host);
                        std::thread::spawn(move || {
                            while !host.stopping() {
                                let message = match events.next_item(Duration::from_millis(500)) {
                                    Ok(None) => continue,
                                    Ok(Some(StreamItem::Event(event))) => {
                                        ServerMessage::Event(event)
                                    }
                                    Ok(Some(StreamItem::Progress(sample))) => {
                                        ServerMessage::Progress(sample)
                                    }
                                    Err(error) => {
                                        let _ = sender
                                            .send(&ServerMessage::Reply(Reply::error(None, error)));
                                        return;
                                    }
                                };
                                if sender.send(&message).is_err() {
                                    return;
                                }
                            }
                        });
                    }
                    Err(error) => {
                        let _ = sender.send(&ServerMessage::Reply(Reply::error(
                            Some(envelope.command_id.clone()),
                            error,
                        )));
                    }
                }
            }
            payload => {
                let shutdown = matches!(payload, Command::EngineShutdown);
                let settings_change = matches!(payload, Command::UpdateSettings { .. });
                let serialized = settings_change.then(|| {
                    host.settings
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                });
                let before = host.engine.session().settings().start_engine_at_sign_in;
                let reply = match host.engine.execute_as(&principal, &envelope) {
                    Ok(result) => Reply::ok(envelope.command_id.clone(), result),
                    Err(error) => Reply::error(Some(envelope.command_id.clone()), error),
                };
                let succeeded = matches!(reply.result, ReplyResult::Ok(_));
                if settings_change && succeeded {
                    let after = host.engine.session().settings().start_engine_at_sign_in;
                    if after != before {
                        host.apply_sign_in(after);
                    }
                }
                drop(serialized);
                if shutdown && succeeded {
                    host.begin_stop();
                }
                if sender.send(&ServerMessage::Reply(reply)).is_err() {
                    return;
                }
            }
        }
    }
}
