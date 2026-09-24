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
use fetchpath_protocol::launch::{self, EngineHome};
use fetchpath_protocol::message::{Reply, ServerMessage};
use fetchpath_protocol::pipe::{
    EngineSecret, Limits, PendingConnection, PipeListener, PipeName, current_user_sid, endpoint,
};
use fetchpath_protocol::{ClientId, EngineClient, ProtocolError, StreamItem};
use fetchpath_session::engine::{Engine, TICK_INTERVAL};
use fetchpath_session::{DEFAULT_MAX_ACTIVE, Session};
use std::fs::File;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

/// How long the engine stays with no client and nothing of its own to do.
pub const IDLE_GRACE: Duration = Duration::from_secs(60);

pub const USAGE: &str = "usage: fetchpath engine [status | stop]";

pub fn run(args: &[String]) -> i32 {
    match args.first().map(String::as_str) {
        None => host(IDLE_GRACE),
        Some("--idle-grace-ms") => match args.get(1).and_then(|value| value.parse::<u64>().ok()) {
            Some(ms) => host(Duration::from_millis(ms)),
            None => usage(),
        },
        Some("status") => status(),
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

fn status() -> i32 {
    let outcome = home()
        .and_then(|home| launch::attach(&home, Limits::default()))
        .and_then(|client| client.send(&ClientId::random(), Command::EngineStatus));
    match outcome {
        Ok(result) => {
            println!(
                "{}",
                serde_json::to_string_pretty(&result).expect("serializable")
            );
            0
        }
        Err(error) if error.code.as_str() == "contract.engine_unavailable" => {
            println!("The Fetchpath engine is not running.");
            1
        }
        Err(error) => {
            eprintln!("fetchpath: {}", error.message);
            1
        }
    }
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
    stop: AtomicBool,
    connections: AtomicUsize,
}

impl Host {
    /// Work the engine does on its own: a job queued, scheduled (including
    /// an automatic retry) or running. Paused jobs and failures waiting for a
    /// person change only when a person acts, which starts a client.
    fn has_own_work(&self) -> bool {
        self.engine.session().has_own_work()
    }
}

fn host(grace: Duration) -> i32 {
    match serve(grace) {
        Ok(code) => code,
        Err(message) => {
            eprintln!("fetchpath engine: {message}");
            1
        }
    }
}

fn serve(grace: Duration) -> Result<i32, String> {
    let home = home().map_err(|error| error.message)?;
    let Some(_lock) = claim(&home)? else {
        eprintln!("The Fetchpath engine is already running.");
        return Ok(0);
    };

    // Restart recovery happens as the session loads, before any pipe exists.
    let session = Session::load_with_browser(home.queue_path(), DEFAULT_MAX_ACTIVE, None)
        .map_err(|error| format!("The download queue could not be opened: {error}"))?;
    let engine = Engine::new(Arc::new(session));
    engine.tick();

    let sid = current_user_sid().map_err(|error| error.message)?;
    let secret =
        EngineSecret::load_or_create(&home.secret_path(), &sid).map_err(|error| error.message)?;
    let name = PipeName::fresh().map_err(|error| error.message)?;
    let mut listener =
        PipeListener::bind(&name, secret, Limits::default()).map_err(|error| error.message)?;
    endpoint::publish(&home.endpoint_path(), &name, &sid).map_err(|error| error.message)?;
    if engine.session().settings().start_engine_at_sign_in {
        signin::apply(true);
    }

    let host = Arc::new(Host {
        engine,
        stop: AtomicBool::new(false),
        connections: AtomicUsize::new(0),
    });
    let ticker = {
        let host = Arc::clone(&host);
        std::thread::spawn(move || {
            while !host.stop.load(Ordering::SeqCst) {
                host.engine.tick();
                std::thread::sleep(TICK_INTERVAL);
            }
        })
    };

    let mut idle_since = Instant::now();
    while !host.stop.load(Ordering::SeqCst) {
        match listener.accept(Some(TICK_INTERVAL)) {
            Ok(Some(pending)) => {
                let host = Arc::clone(&host);
                std::thread::spawn(move || connection(&host, pending));
            }
            Ok(None) => {}
            // Transient: clients that open and close the pipe quickly.
            Err(_) => std::thread::sleep(Duration::from_millis(50)),
        }
        if host.connections.load(Ordering::SeqCst) > 0 || host.has_own_work() {
            idle_since = Instant::now();
        } else if idle_since.elapsed() >= grace {
            host.stop.store(true, Ordering::SeqCst);
        }
    }

    let _ = ticker.join();
    // As quitting the desktop does: running work stops at its checkpoints
    // and the queue is saved, ready to continue on the next start.
    host.engine.session().cancel_all_and_join();
    drop(listener);
    Ok(0)
}

/// Counts an authenticated connection while it lives.
struct Connected<'a>(&'a AtomicUsize);

impl Drop for Connected<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

fn connection(host: &Arc<Host>, pending: PendingConnection) {
    let Ok(connection) = pending.authenticate() else {
        return;
    };
    if connection.restart_requested() {
        host.stop.store(true, Ordering::SeqCst);
        return;
    }
    host.connections.fetch_add(1, Ordering::SeqCst);
    let _counted = Connected(&host.connections);
    let sender = connection.sender();
    while let Ok(Some(envelope)) = connection.receive() {
        match &envelope.payload {
            Command::SubscribeJob { .. } | Command::SubscribeQueue { .. } => {
                match host.engine.subscribe(&envelope) {
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
                            while !host.stop.load(Ordering::SeqCst) {
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
                let signin_before = host.engine.session().settings().start_engine_at_sign_in;
                let shutdown = matches!(payload, Command::EngineShutdown);
                let settings = matches!(payload, Command::UpdateSettings { .. });
                let reply = match host.engine.execute(&envelope) {
                    Ok(result) => Reply::ok(envelope.command_id.clone(), result),
                    Err(error) => Reply::error(Some(envelope.command_id.clone()), error),
                };
                let succeeded = matches!(
                    reply.result,
                    fetchpath_protocol::message::ReplyResult::Ok(_)
                );
                let _ = sender.send(&ServerMessage::Reply(reply));
                if settings && succeeded {
                    let after = host.engine.session().settings().start_engine_at_sign_in;
                    if after != signin_before {
                        signin::apply(after);
                    }
                }
                if shutdown && succeeded {
                    host.stop.store(true, Ordering::SeqCst);
                }
            }
        }
    }
}
