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

use crate::remote::bind;
use fetchpath_session::engine::Engine;
use fetchpath_session::share::ShareStore;
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

struct Running {
    port: u16,
    cancel: CancellationToken,
    thread: JoinHandle<()>,
}

/// The engine host's handle on the share listener: follows the stored port.
pub struct Share {
    engine: Arc<Engine>,
    running: Mutex<Option<Running>>,
    failed: Mutex<Option<(u16, Instant)>>,
}

impl Share {
    pub fn new(engine: Arc<Engine>) -> Self {
        Self {
            engine,
            running: Mutex::new(None),
            failed: Mutex::new(None),
        }
    }

    /// Brings the listener in line with the stored setup: bound to the
    /// stored port while a tunnel address is set, closed otherwise. Cheap
    /// enough to run on every pass; it also restates the report the engine
    /// forgets when the address changes.
    pub fn reconcile(&self) {
        let wanted = self.engine.share_port();
        let mut running = lock(&self.running);
        if let (Some(started), Some(port)) = (running.as_ref(), wanted)
            && started.port == port
        {
            self.engine
                .set_share_listener(port, Ok(format!("127.0.0.1:{port}")));
            return;
        }
        if let Some(started) = running.take() {
            stop(started);
        }
        let Some(port) = wanted else {
            *lock(&self.failed) = None;
            return;
        };
        if matches!(*lock(&self.failed), Some((tried, at)) if tried == port && at.elapsed() < RETRY)
        {
            return;
        }
        match self.start(port) {
            Ok(started) => {
                *lock(&self.failed) = None;
                self.engine
                    .set_share_listener(port, Ok(format!("127.0.0.1:{port}")));
                *running = Some(started);
            }
            Err(message) => {
                *lock(&self.failed) = Some((port, Instant::now()));
                self.engine.set_share_listener(port, Err(message));
            }
        }
    }

    /// Closes the port and every connection.
    pub fn shutdown(&self) {
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
            port,
            cancel,
            thread,
        })
    }

    #[cfg(test)]
    pub(crate) fn port(&self) -> Option<u16> {
        lock(&self.running).as_ref().map(|started| started.port)
    }
}

fn stop(started: Running) {
    started.cancel.cancel();
    let _ = started.thread.join();
}

#[cfg(test)]
mod tests;
