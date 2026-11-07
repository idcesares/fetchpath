//! The share listener (FP-117): a second loopback port that serves public
//! link tickets and nothing else.
//!
//! The person's tunnel (Cloudflare Tunnel, Tailscale Funnel, ...) forwards to
//! this port; it is never the web UI's port, and it answers `GET /s/<id>`
//! only. The engine's session owns the tickets, the frozen file and the
//! encryption; this module moves the encrypted stream between a socket and a
//! ticket and reports where it is bound. It binds the port stored in the
//! sharing setup and no other: a taken port is a visible failure, not a quiet
//! move to a port the tunnel does not know.

mod body;
mod http;
mod listener;
mod page;
mod relay;
mod relay_key;

pub(crate) use relay::tls_dial;
pub(crate) use relay_key::KeyFile;

use crate::remote::bind;
use fetchpath_session::engine::{Engine, RelayPlan};
use fetchpath_session::share::ShareStore;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

/// How soon a port that was taken is tried again.
const RETRY: Duration = Duration::from_secs(5);

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// What the running listener's connections share.
struct Shared {
    store: Arc<ShareStore>,
    page: page::Page,
    cancel: CancellationToken,
}

/// What a running listener or relay connection was started for. A relay
/// connection is bound to a generation of the setup (address, mode, key).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Kind {
    Port(u16),
    Relay(u64),
}

struct Running {
    kind: Kind,
    cancel: CancellationToken,
    thread: JoinHandle<()>,
}

/// The engine host's handle on the share listener: follows the stored port.
pub struct Share {
    engine: Arc<Engine>,
    running: Mutex<Option<Running>>,
    failed: Mutex<Option<(Kind, Instant)>>,
    relay: Option<(Arc<relay_key::KeyFile>, relay::Dial)>,
    /// Set by `shutdown`: a pass that was already on its way starts nothing.
    closed: AtomicBool,
}

impl Share {
    pub fn new(engine: Arc<Engine>) -> Self {
        Self {
            engine,
            running: Mutex::new(None),
            failed: Mutex::new(None),
            relay: None,
            closed: AtomicBool::new(false),
        }
    }

    /// A host that can also enroll with a relay (FP-118): `keys` holds the
    /// sealed enrollment key and `dial` makes the connection.
    pub(crate) fn with_relay(
        engine: Arc<Engine>,
        keys: Arc<relay_key::KeyFile>,
        dial: relay::Dial,
    ) -> Self {
        Self {
            relay: Some((keys, dial)),
            ..Self::new(engine)
        }
    }

    /// Brings the listener in line with the stored setup: bound to the
    /// stored port while a tunnel address is set, closed otherwise. Cheap
    /// enough to run on every pass; it also restates the report the engine
    /// forgets when the address changes.
    pub fn reconcile(&self) {
        // Read the setup under the lock, so two passes cannot act on a setup
        // that changed between them.
        let mut running = lock(&self.running);
        if self.closed.load(Ordering::SeqCst) {
            return;
        }
        let relay_plan = self.engine.share_relay();
        if let (Some(plan), None) = (&relay_plan, &self.relay) {
            self.engine.set_share_relay(
                plan.generation,
                Err("This engine cannot reach a relay.".into()),
            );
        }
        let plan = relay_plan.filter(|_| self.relay.is_some());
        let wanted = match &plan {
            Some(plan) => Some(Kind::Relay(plan.generation)),
            None => self.engine.share_port().map(Kind::Port),
        };
        if let (Some(started), Some(kind)) = (running.as_ref(), wanted)
            && started.kind == kind
        {
            // A relay connection reports for itself.
            if let Kind::Port(port) = kind {
                self.engine
                    .set_share_listener(port, Ok(format!("127.0.0.1:{port}")));
            }
            return;
        }
        if let Some(started) = running.take() {
            stop(started);
        }
        let Some(kind) = wanted else {
            *lock(&self.failed) = None;
            return;
        };
        if matches!(*lock(&self.failed), Some((tried, at)) if tried == kind && at.elapsed() < RETRY)
        {
            return;
        }
        let started = match (kind, plan) {
            (Kind::Port(port), _) => self.start(port),
            (Kind::Relay(_), Some(plan)) => self.start_relay(&plan),
            (Kind::Relay(_), None) => return,
        };
        match started {
            Ok(started) => {
                *lock(&self.failed) = None;
                if let Kind::Port(port) = kind {
                    self.engine
                        .set_share_listener(port, Ok(format!("127.0.0.1:{port}")));
                }
                *running = Some(started);
            }
            Err(message) => {
                *lock(&self.failed) = Some((kind, Instant::now()));
                match kind {
                    Kind::Port(port) => self.engine.set_share_listener(port, Err(message)),
                    Kind::Relay(generation) => {
                        self.engine.set_share_relay(generation, Err(message));
                    }
                }
            }
        }
    }

    /// Closes the port and every connection.
    pub fn shutdown(&self) {
        self.closed.store(true, Ordering::SeqCst);
        if let Some(started) = lock(&self.running).take() {
            stop(started);
        }
    }

    fn start(&self, port: u16) -> Result<Running, String> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| format!("The share listener could not start: {error}"))?;
        let bound = {
            let _context = runtime.enter();
            bind::bind_exact(port)?
        };
        let cancel = CancellationToken::new();
        let shared = Arc::new(Shared {
            store: self.engine.share_store(),
            page: page::Page::build(),
            cancel: cancel.clone(),
        });
        let thread = std::thread::Builder::new()
            .name("share".into())
            .spawn(move || {
                runtime.block_on(listener::run(shared, bound.listeners));
                runtime.shutdown_timeout(Duration::from_secs(2));
            })
            .map_err(|error| format!("The share listener could not start: {error}"))?;
        Ok(Running {
            kind: Kind::Port(port),
            cancel,
            thread,
        })
    }

    fn start_relay(&self, plan: &RelayPlan) -> Result<Running, String> {
        let (keys, dial) = self
            .relay
            .as_ref()
            .ok_or("This engine cannot reach a relay.")?;
        let cancel = CancellationToken::new();
        let shared = Arc::new(Shared {
            store: self.engine.share_store(),
            page: page::Page::build(),
            cancel: cancel.clone(),
        });
        let thread = relay::start(
            Arc::clone(&self.engine),
            shared,
            Arc::clone(keys),
            Arc::clone(dial),
            plan.clone(),
        )?;
        Ok(Running {
            kind: Kind::Relay(plan.generation),
            cancel,
            thread,
        })
    }

    #[cfg(test)]
    pub(crate) fn port(&self) -> Option<u16> {
        lock(&self.running)
            .as_ref()
            .and_then(|started| match started.kind {
                Kind::Port(port) => Some(port),
                Kind::Relay(_) => None,
            })
    }
}

fn stop(started: Running) {
    started.cancel.cancel();
    let _ = started.thread.join();
}

#[cfg(test)]
mod tests;
