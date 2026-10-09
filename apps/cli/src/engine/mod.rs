//! `fetchpath engine`: the one owner of the queue, behind the authenticated
//! pipe (FP-053).
//!
//! Startup order is what keeps recovery ahead of every command: the
//! single-owner lock first, then the session (restart recovery runs as it
//! loads), one reconcile, and only then the pipe and the endpoint file that
//! tells clients where it is. A client cannot reach the engine before the
//! endpoint exists.

mod awake;
mod signin;

use crate::remote::Web;
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

/// How often an engine with no client and nothing of its own to do runs its
/// pass (FP-101 gate G3): an always-on engine waits mostly idle. Commands,
/// transfers and the browser host wake it at once; this only bounds how
/// late it notices time passing, such as an approval expiring.
const IDLE_TICK_INTERVAL: Duration = Duration::from_secs(5);

pub const USAGE: &str = "usage: fetchpath engine [status [--json] | stop [--for-update]]";

/// How long `engine stop --for-update` waits for the engine to let go of
/// its files before setup falls back to ending the process.
const UPDATE_STOP_WAIT: Duration = Duration::from_secs(60);

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
        Some("stop") => match &args[1..] {
            [] => stop(),
            [flag] if flag == "--for-update" => stop_for_update(UPDATE_STOP_WAIT),
            _ => usage(),
        },
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

/// Used by setup before it replaces or removes Fetchpath's files (FP-057).
/// Holds new engines off, asks the running one to stop whatever version it
/// is, and returns once the single-owner lock is free: the engine has saved
/// the queue and exited, so its executable is no longer in use.
fn stop_for_update(wait: Duration) -> i32 {
    let home = match home() {
        Ok(home) => home,
        Err(error) => {
            eprintln!("fetchpath: {}", error.message);
            return 1;
        }
    };
    if !home.dir().is_dir() {
        // No engine has ever run for this person; nothing to stop or hold.
        return 0;
    }
    if let Err(error) = home.hold_for_update() {
        eprintln!("fetchpath: the engine could not be held off during setup: {error}");
        return 1;
    }
    let deadline = Instant::now() + wait;
    loop {
        // Again each time round: an engine a client started just before the
        // hold was written may have taken the lock since.
        let _ = launch::request_restart(&home, Limits::default(), Duration::from_secs(5));
        match lock_free(&home) {
            Ok(true) => {
                println!("The Fetchpath engine is stopped.");
                return 0;
            }
            Ok(false) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(200))
            }
            Ok(false) => {
                eprintln!("fetchpath: the engine did not stop in time.");
                return 1;
            }
            Err(message) => {
                eprintln!("fetchpath: {message}");
                return 1;
            }
        }
    }
}

/// Whether no engine holds the single-owner lock. The probe opens it
/// exactly as an engine would and lets go at once.
fn lock_free(home: &EngineHome) -> Result<bool, String> {
    match claim(home) {
        Ok(Some(_probe)) => Ok(true),
        Ok(None) => Ok(false),
        Err(message) => Err(message),
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
    /// client still in its handshake keeps the engine from going idle. A
    /// signed-in web UI socket counts too.
    connections: Arc<AtomicUsize>,
    /// The loopback web UI, when the person turned it on (FP-104).
    web: Web,
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
            self.engine.wake();
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
    // Checked holding the lock, so setup's own lock probe cannot miss an
    // engine that started just before the hold was written.
    if home.update_held() {
        eprintln!("Fetchpath is being updated or removed; the engine will not start now.");
        return Ok(0);
    }

    // Restart recovery happens as the session loads, before any pipe exists.
    // Browser captures are taken in from the inbox beside the queue and
    // saved to Downloads, as the desktop did while it held the queue.
    let session =
        Session::load_with_browser(home.queue_path(), DEFAULT_MAX_ACTIVE, downloads_dir())
            .map_err(|error| format!("The download queue could not be opened: {error}"))?;
    // A cache that cannot be placed only means nothing is cached.
    if let (Ok(root), Ok(lan)) = (crate::lan::cache_root(), crate::lan::lan_dir()) {
        session.use_cache(root.clone());
        session.use_lan(lan, root);
    }
    let instance = home.instance_id().map_err(|error| error.message)?;
    let engine = Engine::with_instance(Arc::new(session), instance);
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

    let connections = Arc::new(AtomicUsize::new(0));
    let host = Arc::new(Host {
        web: Web::new(
            Arc::clone(&engine),
            home.dir().to_path_buf(),
            Arc::clone(&connections),
        ),
        engine,
        stop: AtomicBool::new(false),
        connections,
        settings: Mutex::new(()),
        default_home: home.is_default(),
        endpoint: home.endpoint_path(),
    });
    let settings = host.engine.session().settings();
    if settings.start_engine_at_sign_in {
        host.apply_sign_in(true);
    }
    // Off also removes sessions left by a run that was killed while on.
    host.web.apply(settings.web_ui);
    let ticker = {
        let host = Arc::clone(&host);
        std::thread::spawn(move || {
            let mut sampled = Instant::now();
            // Held by this thread, which lives as long as the engine serves.
            let mut awake = awake::Awake::default();
            while !host.stopping() {
                // Busy: sample and reconcile four times a second. Idle: let
                // wakeups do the work.
                let interval = if host.connections.load(Ordering::SeqCst) > 0 || host.has_own_work()
                {
                    TICK_INTERVAL
                } else {
                    IDLE_TICK_INTERVAL
                };
                host.engine
                    .wait_for_work(interval.saturating_sub(sampled.elapsed()));
                if host.stopping() {
                    break;
                }
                if sampled.elapsed() >= interval {
                    host.engine.tick();
                    sampled = Instant::now();
                } else {
                    host.engine.reconcile();
                }
                let session = host.engine.session();
                awake.set(session.hub_mode() && session.running_count() > 0);
            }
        })
    };

    let mut idle_since = Instant::now();
    while !host.stopping() {
        match listener.accept(Some(TICK_INTERVAL)) {
            Ok(Some(pending)) => {
                // Counted from acceptance, before the handshake.
                let counted = Connected::open(&host.connections);
                let host = Arc::clone(&host);
                std::thread::spawn(move || {
                    let _counted = counted;
                    connection(&host, pending);
                });
            }
            Ok(None) => {}
            // Transient: clients that open and close the pipe quickly.
            Err(_) => std::thread::sleep(Duration::from_millis(50)),
        }
        // Always on: never stops for being idle (FP-101).
        // Always on first: it never stops for being idle, and asking it
        // costs nothing, unlike counting the queue's own work.
        if host.engine.session().hub_mode()
            || host.connections.load(Ordering::SeqCst) > 0
            || host.has_own_work()
        {
            idle_since = Instant::now();
        } else if idle_since.elapsed() >= grace {
            host.begin_stop();
        }
    }
    host.begin_stop();

    // Close the pipe first, so a client arriving now finds no engine and
    // starts the next one instead of stalling in a handshake here. The web
    // UI port closes with it; its sessions stay for the next run.
    drop(listener);
    host.web.shutdown();
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
pub(crate) struct Connected(Arc<AtomicUsize>);

impl Connected {
    pub(crate) fn open(count: &Arc<AtomicUsize>) -> Self {
        count.fetch_add(1, Ordering::SeqCst);
        Self(Arc::clone(count))
    }
}

impl Drop for Connected {
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
    let Ok(connection) = pending.authenticate_for(host.engine.instance_id().cloned()) else {
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
                let before = host.engine.session().settings();
                // The web UI's own commands are answered here, not by the
                // engine, which has no listener.
                let outcome = host
                    .web
                    .answer(&principal, payload)
                    .unwrap_or_else(|| host.engine.execute_as(&principal, &envelope));
                let reply = match outcome {
                    Ok(result) => Reply::ok(envelope.command_id.clone(), result),
                    Err(error) => Reply::error(Some(envelope.command_id.clone()), error),
                };
                let succeeded = matches!(reply.result, ReplyResult::Ok(_));
                if settings_change && succeeded {
                    let after = host.engine.session().settings();
                    if after.start_engine_at_sign_in != before.start_engine_at_sign_in {
                        host.apply_sign_in(after.start_engine_at_sign_in);
                    }
                    if after.web_ui != before.web_ui {
                        host.web.apply(after.web_ui);
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
