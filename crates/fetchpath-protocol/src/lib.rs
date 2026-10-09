//! Fetchpath engine protocol, version 1.
//!
//! The wire form of [the job contract](../../../docs/architecture/JOB-CONTRACT.md)
//! between the engine and its clients: command envelopes, replies, durable
//! and ephemeral events, snapshots and errors; length-prefixed framing with a
//! hard size cap; JSON Schema for every type; and the [`EngineClient`]
//! interface. It holds no queue logic and no transport.
//!
//! Field names are snake_case. Readers ignore fields they do not know, so
//! adding an optional field keeps the version; anything else changes
//! [`SCHEMA_VERSION`].

pub mod client;
pub mod command;
pub mod describe;
pub mod error;
pub mod frame;
pub mod ids;
pub mod install;
#[cfg(windows)]
pub mod launch;
pub mod message;
pub mod model;
#[cfg(windows)]
pub mod pipe;
pub mod principal;
pub mod schema;
pub mod view;

/// The protocol's major version, carried by every message.
pub const SCHEMA_VERSION: u32 = 1;

pub use client::{EngineClient, EventStream, StreamItem, Subscription};
pub use command::{Command, CommandEnvelope, JobInput, JobRequest};
pub use error::{Action, ErrorCode, ErrorFamily, ErrorScope, ProtocolError};
pub use frame::{MAX_FRAME_BYTES, decode_command, decode_server_message, encode_frame};
pub use ids::{AttemptId, ClientId, CommandId, CredentialRef, InstanceId, JobId, Timestamp};
pub use message::{CommandResult, EventPayload, JobEvent, ProgressSample, Reply, ServerMessage};
pub use model::{JobSnapshot, JobState, SensitiveUrl};
pub use principal::{AgentName, AgentPolicy, Principal};
