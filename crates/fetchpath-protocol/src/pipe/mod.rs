//! Protocol v1 over a per-user Windows named pipe (FP-052).
//!
//! The pipe admits only the current user's SID, refuses remote clients, and
//! is created as the first instance of its name, so a process that claimed
//! the name earlier makes the engine refuse to start rather than sit behind
//! an impostor. Objects without an explicit label are medium integrity with
//! no-write-up, so a lower-integrity process cannot write to it.
//!
//! Both ends then prove knowledge of the per-install secret ([`auth`]). Only
//! after that do protocol frames flow. Every read and write has a deadline,
//! and every limit is per connection: a bad peer closes its own connection
//! and nothing else.

pub mod auth;
mod client;
mod ffi;
mod server;

pub use auth::EngineSecret;
pub use client::{PipeClient, PipeEngineClient};
pub use server::{ConnectionSender, PendingConnection, PipeListener, ServerConnection};

use crate::error::{ErrorCode, ErrorScope, ProtocolError};
use crate::frame::MAX_FRAME_BYTES;
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// A pipe name under `\\.\pipe\`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PipeName(String);

impl PipeName {
    /// The engine's pipe for this install and user. The SID keeps users on
    /// one machine apart; the token is derived from the engine secret, so a
    /// process that cannot read the secret (another user, or a lower
    /// integrity level) cannot know the name in advance to claim it first.
    pub fn for_install(secret: &EngineSecret) -> Result<Self, ProtocolError> {
        let sid = current_user_sid()?;
        Ok(Self(format!(
            r"\\.\pipe\fetchpath-engine-v1-{sid}-{}",
            secret.pipe_name_token()
        )))
    }

    /// A pipe with a chosen final segment, for tests and diagnostics.
    pub fn named(segment: &str) -> Self {
        Self(format!(r"\\.\pipe\{segment}"))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn wide(&self) -> Vec<u16> {
        ffi::wide(&self.0)
    }
}

/// The current user's SID, for the pipe's and secret file's access lists.
pub fn current_user_sid() -> Result<String, ProtocolError> {
    ffi::current_user_sid().map_err(|error| {
        ProtocolError::new(
            ErrorCode::try_from("internal.identity_unavailable".to_owned()).expect("valid"),
            ErrorScope::Engine,
            format!("The current Windows user could not be identified: {error}"),
        )
    })
}

/// A file's owner, access list and integrity label as SDDL, for diagnostics
/// and tests.
pub fn file_security_sddl(path: &std::path::Path) -> Result<String, ProtocolError> {
    ffi::path_security_sddl(path).map_err(|error| {
        connection_error(
            "internal.security_unreadable",
            format!(
                "The security of {} could not be read: {error}",
                path.display()
            ),
        )
    })
}

/// Per-connection limits.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// Largest protocol frame accepted; never above [`MAX_FRAME_BYTES`].
    pub max_frame_bytes: usize,
    /// Commands received but not yet answered. One more closes the
    /// connection.
    pub max_pending_requests: usize,
    /// Connections open at once, authenticated or not. Further clients are
    /// disconnected at once.
    pub max_connections: usize,
    /// The whole handshake must finish within this.
    pub handshake_timeout: Duration,
    /// Once a frame has started, the rest must arrive within this.
    pub frame_timeout: Duration,
    /// With no frame at all for this long, the connection is closed.
    /// `None` keeps an idle connection, such as an event subscription.
    pub idle_timeout: Option<Duration>,
    /// A write that cannot finish within this closes the connection, so a
    /// client that stops reading cannot hold the engine.
    pub write_timeout: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_frame_bytes: MAX_FRAME_BYTES,
            max_pending_requests: 32,
            max_connections: 64,
            handshake_timeout: Duration::from_secs(5),
            frame_timeout: Duration::from_secs(10),
            idle_timeout: Some(Duration::from_secs(600)),
            write_timeout: Duration::from_secs(10),
        }
    }
}

pub(crate) fn connection_error(code: &'static str, message: impl Into<String>) -> ProtocolError {
    auth::auth_error(code, message)
}

/// One end of a connected pipe, shared by a reading and a writing thread.
/// After any timeout or error it refuses further use, because a partial
/// frame may be left behind.
struct Stream {
    handle: ffi::Handle,
    broken: AtomicBool,
    write_lock: std::sync::Mutex<()>,
}

impl Stream {
    fn new(handle: ffi::Handle) -> Arc<Self> {
        Arc::new(Self {
            handle,
            broken: AtomicBool::new(false),
            write_lock: std::sync::Mutex::new(()),
        })
    }

    fn fail(&self, error: ProtocolError) -> ProtocolError {
        self.broken.store(true, Ordering::SeqCst);
        error
    }

    fn check(&self) -> Result<(), ProtocolError> {
        if self.broken.load(Ordering::SeqCst) {
            Err(lost("the connection already failed"))
        } else {
            Ok(())
        }
    }

    /// Reads one frame's body.
    ///
    /// Waits up to `idle` for the frame to start; nothing arriving in that
    /// time is [`Frame::Idle`] and leaves the stream usable, since no byte
    /// was consumed. Once the first byte arrives, the rest must follow
    /// within `frame_timeout` and by `cap`, or the stream fails.
    fn read_frame(
        &self,
        max_bytes: usize,
        idle: Option<Duration>,
        frame_timeout: Duration,
        cap: Option<Instant>,
    ) -> Result<Frame, ProtocolError> {
        self.check()?;
        let mut prefix = [0_u8; 4];
        let mut filled = 0;
        let idle_deadline = idle.map(|value| Instant::now() + value);
        let mut deadline = earliest(idle_deadline, cap);
        let mut body: Option<Vec<u8>> = None;
        loop {
            let started = body.is_some() || filled > 0;
            let (buffer, done) = match body.as_mut() {
                None => (&mut prefix[filled..], filled == 4),
                Some(body) => {
                    let complete = filled == body.len();
                    (&mut body[filled..], complete)
                }
            };
            if done {
                match body {
                    Some(body) => return Ok(Frame::Body(body)),
                    None => {
                        let length = u32::from_le_bytes(prefix) as usize;
                        if length == 0 {
                            return Err(self.fail(ProtocolError::malformed("an empty frame")));
                        }
                        if length > max_bytes.min(MAX_FRAME_BYTES) {
                            return Err(self.fail(ProtocolError::too_large(length)));
                        }
                        body = Some(vec![0; length]);
                        filled = 0;
                        continue;
                    }
                }
            }
            let timeout = deadline.map(|at| at.saturating_duration_since(Instant::now()));
            let outcome = if timeout.is_some_and(|left| left.is_zero()) {
                Ok(ffi::Wait::TimedOut)
            } else {
                ffi::read(&self.handle, buffer, timeout)
            };
            match outcome {
                Ok(ffi::Wait::Done(0)) if !started => return Ok(Frame::Closed),
                Ok(ffi::Wait::Done(0)) => {
                    return Err(self.fail(lost("the other side closed the connection mid-frame")));
                }
                Ok(ffi::Wait::Done(read)) => {
                    if !started {
                        // The frame has started; the rest has its own deadline.
                        deadline = earliest(Some(Instant::now() + frame_timeout), cap);
                    }
                    filled += read as usize;
                }
                Ok(ffi::Wait::TimedOut) if !started && cap.is_none_or(|at| Instant::now() < at) => {
                    return Ok(Frame::Idle);
                }
                Ok(ffi::Wait::TimedOut) => return Err(self.fail(timed_out())),
                Err(error) => return Err(self.fail(lost(error))),
            }
        }
    }

    fn write_frame(
        &self,
        message: &impl Serialize,
        timeout: Duration,
    ) -> Result<(), ProtocolError> {
        self.check()?;
        self.write_unchecked(message, timeout)
    }

    /// Writes even after the read side failed, for the one refusal a peer
    /// is told before its connection closes.
    fn write_final(&self, message: &impl Serialize, timeout: Duration) {
        let _ = self.write_unchecked(message, timeout);
    }

    fn write_unchecked(
        &self,
        message: &impl Serialize,
        timeout: Duration,
    ) -> Result<(), ProtocolError> {
        let frame = crate::frame::encode_frame(message)?;
        let _guard = self
            .write_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        ffi::write_all(&self.handle, &frame, Some(timeout)).map_err(|error| {
            self.fail(if error.kind() == std::io::ErrorKind::TimedOut {
                timed_out()
            } else {
                lost(error)
            })
        })
    }

    /// One handshake frame, all of it by `deadline`.
    fn read_handshake<T: DeserializeOwned>(&self, deadline: Instant) -> Result<T, ProtocolError> {
        let left = deadline.saturating_duration_since(Instant::now());
        match self.read_frame(auth::MAX_HANDSHAKE_BYTES, Some(left), left, Some(deadline))? {
            Frame::Body(body) => {
                serde_json::from_slice(&body).map_err(|error| self.fail(handshake_failed(error)))
            }
            Frame::Idle => Err(self.fail(timed_out())),
            Frame::Closed => Err(self.fail(lost("the other side closed during the handshake"))),
        }
    }
}

enum Frame {
    Body(Vec<u8>),
    /// Nothing arrived in the idle window; no byte was consumed.
    Idle,
    /// The peer closed cleanly between frames.
    Closed,
}

fn earliest(first: Option<Instant>, second: Option<Instant>) -> Option<Instant> {
    match (first, second) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

fn lost(detail: impl std::fmt::Display) -> ProtocolError {
    connection_error(
        "contract.connection_lost",
        format!("The connection to the engine was lost: {detail}"),
    )
}

fn timed_out() -> ProtocolError {
    connection_error(
        "contract.connection_timed_out",
        "The other side did not send or read in time; the connection was closed.",
    )
}

fn handshake_failed(detail: impl std::fmt::Display) -> ProtocolError {
    connection_error(
        "auth.handshake_failed",
        format!("The engine connection could not be authenticated: {detail}"),
    )
}
