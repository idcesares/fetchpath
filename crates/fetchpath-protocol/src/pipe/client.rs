//! A client's end of the pipe, and the pipe-backed [`EngineClient`].

use super::auth::{self, EngineSecret, Handshake, Nonce};
use super::{Limits, PipeName, Stream, connection_error, ffi, handshake_failed};
use crate::client::{EngineClient, EventStream, StreamItem, Subscription};
use crate::command::{Command, CommandEnvelope};
use crate::error::{Action, ProtocolError};
use crate::frame::decode_server_message;
use crate::ids::InstanceId;
use crate::message::{CommandResult, ServerMessage};
use crate::principal::Principal;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// One authenticated connection to the engine.
pub struct PipeClient {
    stream: Arc<Stream>,
    limits: Limits,
    instance_id: Option<InstanceId>,
}

impl PipeClient {
    /// The engine instance this connection reached, when the engine has one.
    pub fn instance_id(&self) -> Option<&InstanceId> {
        self.instance_id.as_ref()
    }

    /// Connects and runs the handshake. The engine must prove it holds the
    /// same secret before anything is sent, so a process squatting on the
    /// pipe name learns nothing.
    pub fn connect(
        name: &PipeName,
        secret: &EngineSecret,
        limits: Limits,
        connect_timeout: Duration,
    ) -> Result<Self, ProtocolError> {
        Self::connect_as(name, secret, limits, connect_timeout, &Principal::User)
    }

    /// Connects on behalf of `principal`, such as an agent host.
    pub fn connect_as(
        name: &PipeName,
        secret: &EngineSecret,
        limits: Limits,
        connect_timeout: Duration,
        principal: &Principal,
    ) -> Result<Self, ProtocolError> {
        Self::open(name, secret, limits, connect_timeout, None, principal)
    }

    /// Asks the engine to stop so a new one can start. Works whatever
    /// protocol version the engine speaks, because it is part of the
    /// handshake, and needs the engine secret like any connection.
    pub fn request_restart(
        name: &PipeName,
        secret: &EngineSecret,
        limits: Limits,
        connect_timeout: Duration,
    ) -> Result<(), ProtocolError> {
        Self::open(
            name,
            secret,
            limits,
            connect_timeout,
            Some("restart"),
            &Principal::User,
        )
        .map(drop)
    }

    fn open(
        name: &PipeName,
        secret: &EngineSecret,
        limits: Limits,
        connect_timeout: Duration,
        intent: Option<&str>,
        principal: &Principal,
    ) -> Result<Self, ProtocolError> {
        let handle = ffi::open_client(&name.wide(), connect_timeout).map_err(|error| {
            let detail = match error {
                ffi::OpenError::NotFound => "the engine is not running".to_owned(),
                ffi::OpenError::Busy => "the engine is busy; try again".to_owned(),
                ffi::OpenError::Other(error) => error.to_string(),
            };
            connection_error(
                "contract.engine_unavailable",
                format!("Fetchpath could not reach its engine: {detail}."),
            )
        })?;
        let stream = Stream::new(handle);
        let deadline = Instant::now() + limits.handshake_timeout;
        let client_nonce = Nonce::random()?;
        stream.write_frame(
            &Handshake::Hello {
                transport: auth::TRANSPORT.into(),
                version: auth::TRANSPORT_VERSION,
                client_nonce: client_nonce.to_hex(),
                intent: intent.map(str::to_owned),
                principal: (!principal.is_user()).then(|| principal.to_string()),
            },
            deadline.saturating_duration_since(Instant::now()),
        )?;
        let Handshake::Challenge {
            server_nonce,
            server_proof,
        } = stream.read_handshake(deadline)?
        else {
            return Err(stream.fail(handshake_failed("expected a challenge")));
        };
        let server_nonce = Nonce::from_hex(&server_nonce)
            .ok_or_else(|| stream.fail(handshake_failed("bad server nonce")))?;
        let genuine = auth::from_hex(&server_proof)
            .is_some_and(|proof| secret.verify_server(&client_nonce, &server_nonce, &proof));
        if !genuine {
            return Err(stream.fail(
                connection_error(
                    "auth.peer_not_engine",
                    "The process on the engine's pipe could not prove it is this user's Fetchpath engine.",
                )
                .with_action(Action::UpdateSoftware),
            ));
        }
        stream.write_frame(
            &Handshake::Proof {
                client_proof: auth::to_hex(&secret.client_proof(&client_nonce, &server_nonce)),
            },
            deadline.saturating_duration_since(Instant::now()),
        )?;
        match stream.read_handshake(deadline)? {
            Handshake::Welcome { instance_id } => Ok(Self {
                stream,
                limits,
                instance_id,
            }),
            _ => Err(stream.fail(handshake_failed("expected welcome"))),
        }
    }

    pub fn send(&self, envelope: &CommandEnvelope) -> Result<(), ProtocolError> {
        self.stream.write_frame(envelope, self.limits.write_timeout)
    }

    /// The next message, or `Ok(None)` when nothing started arriving within
    /// `timeout` (`None` waits for ever). The connection stays usable after
    /// a quiet period; a clean close is reported as a lost connection.
    pub fn receive(
        &self,
        timeout: Option<Duration>,
    ) -> Result<Option<ServerMessage>, ProtocolError> {
        match self.stream.read_frame(
            self.limits.max_frame_bytes,
            timeout,
            self.limits.frame_timeout,
            None,
        )? {
            super::Frame::Body(body) => decode_server_message(&body).map(Some),
            super::Frame::Idle => Ok(None),
            super::Frame::Closed => Err(self.stream.fail(connection_error(
                "contract.connection_lost",
                "The engine closed the connection.",
            ))),
        }
    }

    /// Sends a command and waits for its reply, skipping any other message.
    pub fn call(
        &self,
        envelope: &CommandEnvelope,
        timeout: Duration,
    ) -> Result<CommandResult, ProtocolError> {
        self.send(envelope)?;
        let deadline = Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let Some(message) = self.receive(Some(left))? else {
                return Err(connection_error(
                    "contract.connection_timed_out",
                    "The engine did not answer in time.",
                ));
            };
            match message {
                ServerMessage::Reply(reply)
                    if reply.command_id.as_ref() == Some(&envelope.command_id) =>
                {
                    return reply.into_result();
                }
                // A reply with no id is a connection-level refusal.
                ServerMessage::Reply(reply) if reply.command_id.is_none() => {
                    return reply.into_result();
                }
                _ => {}
            }
        }
    }
}

/// [`EngineClient`] over the pipe. Commands share one connection, opened on
/// first use and reopened once if it was lost; each subscription gets its
/// own connection so events never wait behind replies.
///
/// The first engine instance it reaches is the one it means for its whole
/// life (contract D6): every command names it, so an engine with another
/// data folder behind a later connection refuses instead of acting.
pub struct PipeEngineClient {
    name: PipeName,
    secret: EngineSecret,
    limits: Limits,
    connect_timeout: Duration,
    reply_timeout: Duration,
    principal: Principal,
    connection: Mutex<Option<PipeClient>>,
    instance: Mutex<Option<InstanceId>>,
}

impl PipeEngineClient {
    pub fn new(name: PipeName, secret: EngineSecret, limits: Limits) -> Self {
        Self {
            name,
            secret,
            limits,
            connect_timeout: Duration::from_secs(5),
            reply_timeout: Duration::from_secs(60),
            principal: Principal::User,
            connection: Mutex::new(None),
            instance: Mutex::new(None),
        }
    }

    /// The instance this client means: the one it was given, or the first
    /// one it reached.
    pub fn instance_id(&self) -> Option<InstanceId> {
        self.instance
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// Means `instance` on every command, whatever engine later answers.
    pub fn for_instance(self, instance: Option<InstanceId>) -> Self {
        *self
            .instance
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = instance;
        self
    }

    /// Acts for `principal` on every connection it opens.
    pub fn with_principal(mut self, principal: Principal) -> Self {
        self.principal = principal;
        self
    }

    fn open(&self) -> Result<PipeClient, ProtocolError> {
        let client = PipeClient::connect_as(
            &self.name,
            &self.secret,
            self.limits,
            self.connect_timeout,
            &self.principal,
        )?;
        let mut instance = self
            .instance
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if instance.is_none() {
            *instance = client.instance_id().cloned();
        }
        Ok(client)
    }

    /// The envelope naming this client's instance, unless it names one.
    fn stamped(&self, envelope: &CommandEnvelope) -> CommandEnvelope {
        let mut envelope = envelope.clone();
        if envelope.expected_instance_id.is_none() {
            envelope.expected_instance_id = self.instance_id();
        }
        envelope
    }
}

impl EngineClient for PipeEngineClient {
    fn execute(&self, envelope: &CommandEnvelope) -> Result<CommandResult, ProtocolError> {
        let mut connection = self
            .connection
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for attempt in 0..2 {
            if connection.is_none() {
                *connection = Some(self.open()?);
            }
            let client = connection.as_ref().expect("opened above");
            match client.call(&self.stamped(envelope), self.reply_timeout) {
                // Resending the same envelope is safe: the engine answers a
                // repeated command id with the original result.
                Err(error) if attempt == 0 && error.code.as_str() == "contract.connection_lost" => {
                    *connection = None;
                }
                Err(error) if error.scope == crate::error::ErrorScope::Connection => {
                    *connection = None;
                    return Err(error);
                }
                other => return other,
            }
        }
        unreachable!("the loop returns on its second attempt")
    }

    fn subscribe(&self, envelope: &CommandEnvelope) -> Result<Subscription, ProtocolError> {
        if !matches!(
            envelope.payload,
            Command::SubscribeJob { .. } | Command::SubscribeQueue { .. }
        ) {
            return Err(ProtocolError::malformed(
                "subscribe takes SubscribeJob or SubscribeQueue",
            ));
        }
        let client = self.open()?;
        let start = client.call(&self.stamped(envelope), self.reply_timeout)?;
        Ok(Subscription {
            start,
            events: Box::new(PipeEvents { client }),
        })
    }
}

struct PipeEvents {
    client: PipeClient,
}

impl EventStream for PipeEvents {
    fn next_item(&mut self, timeout: Duration) -> Result<Option<StreamItem>, ProtocolError> {
        match self.client.receive(Some(timeout))? {
            None => Ok(None),
            Some(ServerMessage::Event(event)) => Ok(Some(StreamItem::Event(event))),
            Some(ServerMessage::Progress(sample)) => Ok(Some(StreamItem::Progress(sample))),
            // Only an error reply ends a subscription; anything else is not
            // for this stream.
            Some(ServerMessage::Reply(reply)) => reply.into_result().map(|_| None),
        }
    }
}
