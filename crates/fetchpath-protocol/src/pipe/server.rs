//! The engine's end of the pipe.

use super::auth::{self, EngineSecret, Handshake, Nonce};
use super::{Limits, PipeName, Stream, connection_error, ffi, handshake_failed};
use crate::command::CommandEnvelope;
use crate::error::{ErrorCode, ErrorScope, ProtocolError};
use crate::frame::decode_command;
use crate::message::{Reply, ServerMessage};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

const BUFFER_BYTES: u32 = 64 * 1024;

/// Accepts connections on the engine's pipe.
pub struct PipeListener {
    name: Vec<u16>,
    security: ffi::SecurityDescriptor,
    secret: Arc<EngineSecret>,
    limits: Limits,
    live: Arc<AtomicUsize>,
    next: ffi::PipeInstance,
}

impl PipeListener {
    /// Claims the pipe name. Fails with `contract.engine_already_running`
    /// when any process already holds an instance of it.
    pub fn bind(
        name: &PipeName,
        secret: EngineSecret,
        limits: Limits,
    ) -> Result<Self, ProtocolError> {
        let sid = super::current_user_sid()?;
        // Protected DACL: generic all for this user's SID and nobody else.
        // The explicit medium label forbids lower-integrity processes both
        // writing and reading: the default label only stops writes, and a
        // read-only open would still hold a connection slot.
        let security =
            ffi::SecurityDescriptor::from_sddl(&format!("D:P(A;;GA;;;{sid})S:(ML;;NWNR;;;ME)"))
                .map_err(|error| {
                    connection_error(
                        "internal.pipe_security",
                        format!("The pipe's access list could not be built: {error}"),
                    )
                })?;
        let wide = name.wide();
        let first = ffi::create_pipe_instance(&wide, &security, true, BUFFER_BYTES).map_err(|error| {
            if error.raw_os_error() == Some(5) {
                connection_error(
                    "contract.engine_already_running",
                    "Another process already holds the engine's pipe. Only one engine runs per user.",
                )
            } else {
                connection_error(
                    "internal.pipe_unavailable",
                    format!("The engine's pipe could not be created: {error}"),
                )
            }
        })?;
        Ok(Self {
            name: wide,
            security,
            secret: Arc::new(secret),
            limits,
            live: Arc::new(AtomicUsize::new(0)),
            next: first,
        })
    }

    /// Waits for the next client, up to `timeout` (`None` waits for ever).
    /// `Ok(None)` on timeout. A client over the connection limit is
    /// disconnected at once and waiting continues.
    pub fn accept(
        &mut self,
        timeout: Option<Duration>,
    ) -> Result<Option<PendingConnection>, ProtocolError> {
        let deadline = timeout.map(|value| Instant::now() + value);
        let mut failures = 0;
        loop {
            let left = deadline.map(|at| at.saturating_duration_since(Instant::now()));
            let connected = match ffi::connect(&self.next, left) {
                Ok(connected) => connected,
                // Fresh instances failing too is not a departed client.
                Err(error) if failures >= 3 => {
                    return Err(connection_error(
                        "internal.pipe_unavailable",
                        format!("Waiting for a client failed: {error}"),
                    ));
                }
                Err(_) => {
                    failures += 1;
                    // A client that opened and closed the instance before it
                    // was waited on leaves it unusable (ERROR_NO_DATA). Put a
                    // fresh instance in its place so one departing client
                    // cannot wedge the listener, then keep waiting.
                    ffi::disconnect(&self.next);
                    self.next = self.fresh_instance()?;
                    if deadline.is_some_and(|at| Instant::now() >= at) {
                        return Ok(None);
                    }
                    continue;
                }
            };
            if !connected {
                return Ok(None);
            }
            // Keep an instance listening before handing this one off.
            let fresh = self.fresh_instance()?;
            let instance = std::mem::replace(&mut self.next, fresh);
            let guard = LiveGuard::claim(&self.live, self.limits.max_connections);
            let Some(guard) = guard else {
                ffi::disconnect(&instance);
                continue;
            };
            return Ok(Some(PendingConnection {
                stream: Stream::new(instance.handle),
                secret: Arc::clone(&self.secret),
                limits: self.limits,
                _live: guard,
            }));
        }
    }

    fn fresh_instance(&self) -> Result<ffi::PipeInstance, ProtocolError> {
        ffi::create_pipe_instance(&self.name, &self.security, false, BUFFER_BYTES).map_err(
            |error| {
                connection_error(
                    "internal.pipe_unavailable",
                    format!("The engine's pipe could not be reopened: {error}"),
                )
            },
        )
    }

    /// The pipe's owner, access list and integrity label as SDDL.
    pub fn security_sddl(&self) -> Result<String, ProtocolError> {
        ffi::handle_security_sddl(&self.next.handle).map_err(|error| {
            connection_error(
                "internal.security_unreadable",
                format!("The pipe's security could not be read: {error}"),
            )
        })
    }

    /// Connections open now, authenticated or not.
    pub fn live_connections(&self) -> usize {
        self.live.load(Ordering::SeqCst)
    }
}

/// Counts a connection for as long as it exists.
struct LiveGuard(Arc<AtomicUsize>);

impl LiveGuard {
    fn claim(live: &Arc<AtomicUsize>, limit: usize) -> Option<Self> {
        live.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |count| {
            (count < limit).then_some(count + 1)
        })
        .ok()
        .map(|_| Self(Arc::clone(live)))
    }
}

impl Drop for LiveGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// A connected client that has not proved itself yet. Run
/// [`PendingConnection::authenticate`] on the connection's own thread so a
/// slow client cannot delay others.
pub struct PendingConnection {
    stream: Arc<Stream>,
    secret: Arc<EngineSecret>,
    limits: Limits,
    _live: LiveGuard,
}

impl PendingConnection {
    pub fn authenticate(self) -> Result<ServerConnection, ProtocolError> {
        let deadline = Instant::now() + self.limits.handshake_timeout;
        let hello: Handshake = self.stream.read_handshake(deadline)?;
        let Handshake::Hello {
            transport,
            version,
            client_nonce,
        } = hello
        else {
            return Err(self.stream.fail(handshake_failed("expected hello")));
        };
        if transport != auth::TRANSPORT || version != auth::TRANSPORT_VERSION {
            return Err(self.stream.fail(handshake_failed("unsupported transport")));
        }
        let client_nonce = Nonce::from_hex(&client_nonce)
            .ok_or_else(|| self.stream.fail(handshake_failed("bad client nonce")))?;
        let server_nonce = Nonce::random()?;
        let challenge = Handshake::Challenge {
            server_nonce: server_nonce.to_hex(),
            server_proof: auth::to_hex(&self.secret.server_proof(&client_nonce, &server_nonce)),
        };
        self.stream.write_frame(
            &challenge,
            deadline.saturating_duration_since(Instant::now()),
        )?;
        let proof: Handshake = self.stream.read_handshake(deadline)?;
        let Handshake::Proof { client_proof } = proof else {
            return Err(self.stream.fail(handshake_failed("expected proof")));
        };
        let valid = auth::from_hex(&client_proof).is_some_and(|proof| {
            self.secret
                .verify_client(&client_nonce, &server_nonce, &proof)
        });
        if !valid {
            return Err(self.stream.fail(handshake_failed(
                "the client does not hold the engine secret",
            )));
        }
        self.stream.write_frame(
            &Handshake::Welcome,
            deadline.saturating_duration_since(Instant::now()),
        )?;
        Ok(ServerConnection {
            sender: ConnectionSender {
                stream: Arc::clone(&self.stream),
                pending: Arc::new(AtomicUsize::new(0)),
                limits: self.limits,
            },
            stream: self.stream,
            limits: self.limits,
            idle_timeout: std::sync::Mutex::new(self.limits.idle_timeout),
            _live: self._live,
        })
    }
}

/// An authenticated client. Read commands with [`ServerConnection::receive`]
/// on one thread; send replies and events through [`ConnectionSender`]s from
/// any thread.
pub struct ServerConnection {
    stream: Arc<Stream>,
    sender: ConnectionSender,
    limits: Limits,
    idle_timeout: std::sync::Mutex<Option<Duration>>,
    _live: LiveGuard,
}

impl ServerConnection {
    /// Changes how long this connection may stay silent. The engine turns it
    /// off for a connection that subscribed to events, because a subscriber
    /// only listens after its subscribe command.
    pub fn set_idle_timeout(&self, timeout: Option<Duration>) {
        *self
            .idle_timeout
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = timeout;
    }

    pub fn sender(&self) -> ConnectionSender {
        self.sender.clone()
    }

    /// The next command, `Ok(None)` when the client closed cleanly.
    ///
    /// A command-level refusal (an unknown command) is answered here and
    /// reading continues. A connection-level one (unreadable, oversize, other
    /// major version) is answered where possible and ends the connection, as
    /// does a client with too many unanswered commands.
    pub fn receive(&self) -> Result<Option<CommandEnvelope>, ProtocolError> {
        loop {
            let frame = self.stream.read_frame(
                self.limits.max_frame_bytes,
                *self
                    .idle_timeout
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()),
                self.limits.frame_timeout,
                None,
            );
            let frame = match frame {
                Err(error)
                    if error.code == ErrorCode::MESSAGE_TOO_LARGE
                        || error.code == ErrorCode::MALFORMED_MESSAGE =>
                {
                    // The stream is still writable: say why before closing.
                    self.stream.write_final(
                        &ServerMessage::Reply(Reply::error(None, error.clone())),
                        self.limits.write_timeout,
                    );
                    return Err(error);
                }
                other => other?,
            };
            let body = match frame {
                super::Frame::Body(body) => body,
                super::Frame::Closed => return Ok(None),
                super::Frame::Idle => {
                    return Err(self.stream.fail(connection_error(
                        "contract.connection_timed_out",
                        "The connection was idle for too long and was closed.",
                    )));
                }
            };
            match decode_command(&body) {
                Ok(envelope) => {
                    let pending = self.sender.pending.fetch_add(1, Ordering::SeqCst) + 1;
                    if pending > self.limits.max_pending_requests {
                        let error = connection_error(
                            "resource.pending_limit",
                            format!(
                                "More than {} commands are waiting for replies on this connection.",
                                self.limits.max_pending_requests
                            ),
                        );
                        let _ = self.sender.send_raw(&ServerMessage::Reply(Reply::error(
                            Some(envelope.command_id),
                            error.clone(),
                        )));
                        return Err(self.stream.fail(error));
                    }
                    return Ok(Some(envelope));
                }
                Err(rejected) => {
                    let scope = rejected.error.scope;
                    let error = rejected.error.clone();
                    let _ = self
                        .sender
                        .send_raw(&ServerMessage::Reply(rejected.into_reply()));
                    if scope != ErrorScope::Command {
                        return Err(self.stream.fail(error));
                    }
                }
            }
        }
    }
}

/// Sends frames on one connection. Cheap to clone; frames never interleave.
#[derive(Clone)]
pub struct ConnectionSender {
    stream: Arc<Stream>,
    pending: Arc<AtomicUsize>,
    limits: Limits,
}

impl ConnectionSender {
    /// Sends a reply, event or progress sample. A reply to a received
    /// command releases its pending slot.
    pub fn send(&self, message: &ServerMessage) -> Result<(), ProtocolError> {
        self.send_raw(message)?;
        if let ServerMessage::Reply(Reply {
            command_id: Some(_),
            ..
        }) = message
        {
            let _ = self
                .pending
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |count| {
                    count.checked_sub(1)
                });
        }
        Ok(())
    }

    fn send_raw(&self, message: &ServerMessage) -> Result<(), ProtocolError> {
        self.stream.write_frame(message, self.limits.write_timeout)
    }
}
