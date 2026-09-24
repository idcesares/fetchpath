//! The interface every client uses to reach the engine.
//!
//! Two implementations follow: one over the authenticated named pipe
//! (FP-052) and one that calls a session in the same process, for tests and
//! for the terminal's development before the pipe lands (FP-051). Clients are
//! written against this trait so they do not care which they have.

use crate::command::{Command, CommandEnvelope};
use crate::error::ProtocolError;
use crate::ids::ClientId;
use crate::message::{CommandResult, JobEvent, ProgressSample};
use std::time::Duration;

pub trait EngineClient: Send + Sync {
    /// Sends one command and waits for its reply. To retry after a lost
    /// reply, call again with the same envelope; the engine returns the
    /// original result instead of acting twice.
    fn execute(&self, envelope: &CommandEnvelope) -> Result<CommandResult, ProtocolError>;

    /// Sends a `SubscribeJob` or `SubscribeQueue` envelope. The result is
    /// `Subscribed` or `SnapshotBoundary`, and the stream continues from its
    /// position.
    fn subscribe(&self, envelope: &CommandEnvelope) -> Result<Subscription, ProtocolError>;

    /// Builds an envelope with a fresh command id and sends it once.
    fn send(&self, client_id: &ClientId, command: Command) -> Result<CommandResult, ProtocolError> {
        self.execute(&CommandEnvelope::new(client_id.clone(), command))
    }
}

pub struct Subscription {
    /// `Subscribed` or `SnapshotBoundary`.
    pub start: CommandResult,
    pub events: Box<dyn EventStream>,
}

impl std::fmt::Debug for Subscription {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Subscription")
            .field("start", &self.start)
            .finish_non_exhaustive()
    }
}

/// Events after a subscription's start position.
pub trait EventStream: Send {
    /// The next item, or `Ok(None)` when `timeout` passes first. An error
    /// ends the stream; resubscribe from the last `seq` or `cursor` seen.
    fn next_item(&mut self, timeout: Duration) -> Result<Option<StreamItem>, ProtocolError>;
}

// A wire message is built once and serialized; boxing its larger variants
// would only add noise at every construction site.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, PartialEq)]
pub enum StreamItem {
    Event(JobEvent),
    Progress(ProgressSample),
}
