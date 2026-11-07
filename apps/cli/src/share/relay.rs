//! The engine's relay connection (FP-118).
//!
//! In relay mode nothing listens locally: the engine dials the relay, proves
//! it holds the enrollment key, announces the ids of its live tickets and
//! serves the visitors the relay hands back, one request per connection,
//! with the same handler as the local listener. The key is read from its
//! sealed file when the connection starts and never leaves this module.

use super::relay_key::KeyFile;
use super::{Shared, listener};
use fetchpath_relay::client::{self, Connector, Handler, Settings, Status};
use fetchpath_relay::wire::EnrollmentKey;
use fetchpath_session::engine::{Engine, RelayPlan};
use std::io;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;
use tokio::sync::watch;

/// The name the engine enrolls under. Fixed on purpose: the computer's name
/// is personal data and the relay only logs this label.
const NAME: &str = "fetchpath";
/// How often the live ticket ids are compared with what the relay was told.
const IDS_POLL: Duration = Duration::from_millis(250);

/// Makes the connector for one setup; replaced in tests to reach a relay on
/// loopback without TLS.
pub(crate) type Dial = Arc<dyn Fn(&RelayPlan) -> io::Result<Connector> + Send + Sync>;

pub(crate) fn tls_dial() -> Dial {
    Arc::new(|plan| {
        fetchpath_relay::connect::tls_connector(
            &plan.host,
            format!("{}:{}", plan.host, plan.port),
            Vec::new(),
        )
    })
}

/// Starts the connection on a thread of its own; it ends when
/// `shared.cancel` does. Reports to the engine under `plan.generation`.
pub(super) fn start(
    engine: Arc<Engine>,
    shared: Arc<Shared>,
    keys: Arc<KeyFile>,
    dial: Dial,
    plan: RelayPlan,
) -> Result<JoinHandle<()>, String> {
    let key = match keys.load() {
        Ok(Some(key)) => key,
        Ok(None) => return Err("No relay key is stored. Enroll with the relay's key.".into()),
        Err(_) => {
            return Err("The stored relay key cannot be read. Enroll with the key again.".into());
        }
    };
    let connect = dial(&plan).map_err(|error| format!("The relay cannot be reached: {error}"))?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("The relay connection could not start: {error}"))?;
    std::thread::Builder::new()
        .name("relay".into())
        .spawn(move || {
            runtime.block_on(run(engine, shared, key, connect, plan));
            runtime.shutdown_timeout(Duration::from_secs(2));
        })
        .map_err(|error| format!("The relay connection could not start: {error}"))
}

fn describe(status: &Status, plan: &RelayPlan) -> Option<Result<String, String>> {
    match status {
        Status::Connecting => None,
        Status::Enrolled => Some(Ok(format!("relay {}", plan.host))),
        Status::Refused => Some(Err(
            "The relay refused this engine's key. Enroll again with the relay's key.".into(),
        )),
        Status::Unreachable => Some(Err("The relay is not reachable right now.".into())),
    }
}

async fn run(
    engine: Arc<Engine>,
    shared: Arc<Shared>,
    key: EnrollmentKey,
    connect: Connector,
    plan: RelayPlan,
) {
    let store = Arc::clone(&shared.store);
    let cancel = shared.cancel.clone();
    let handler: Handler = {
        let shared = Arc::clone(&shared);
        Arc::new(move |conn| {
            let shared = Arc::clone(&shared);
            Box::pin(listener::serve(shared, conn, false))
        })
    };
    let (ids_sender, ids) = watch::channel(sorted(store.live_ids()));
    let (status_sender, mut status) = watch::channel(Status::Connecting);
    let connection = client::run(
        Settings::new(key, NAME),
        connect,
        ids,
        handler,
        status_sender,
    );
    let follow = async {
        let mut poll = tokio::time::interval(IDS_POLL);
        loop {
            tokio::select! {
                changed = status.changed() => {
                    if changed.is_err() {
                        break;
                    }
                    let current = status.borrow_and_update().clone();
                    if let Some(result) = describe(&current, &plan) {
                        engine.set_share_relay(plan.generation, result);
                    }
                }
                _ = poll.tick() => {
                    let live = sorted(store.live_ids());
                    ids_sender.send_if_modified(|announced| {
                        let changed = *announced != live;
                        if changed {
                            *announced = live;
                        }
                        changed
                    });
                }
            }
        }
    };
    tokio::select! {
        () = cancel.cancelled() => {}
        () = connection => {}
        () = follow => {}
    }
}

fn sorted(mut ids: Vec<String>) -> Vec<String> {
    ids.sort();
    ids
}
