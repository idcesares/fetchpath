//! What the engine sends: replies, durable events and progress samples
//! (job contract §5 and §8).

use crate::SCHEMA_VERSION;
use crate::error::ProtocolError;
use crate::ids::{AttemptId, CommandId, JobId, Timestamp};
use crate::model::{
    CacheView, EngineStatus, IntegrityOutcome, JobDetails, JobSnapshot, JobState, LinkInspection,
    MediaInspection, Progress, QueueStats, Rule, SensitiveUrl, SettingsView, WaitingReason,
};
use crate::principal::AgentAccess;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Every frame from the engine to a client.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "message", rename_all = "snake_case")]
pub enum ServerMessage {
    Reply(Reply),
    Event(JobEvent),
    Progress(ProgressSample),
}

/// The answer to one command. `command_id` is absent only when the command
/// could not be read far enough to find it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Reply {
    pub schema_version: u32,
    #[serde(default)]
    pub command_id: Option<CommandId>,
    pub result: ReplyResult,
}

impl Reply {
    pub fn ok(command_id: CommandId, result: CommandResult) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            command_id: Some(command_id),
            result: ReplyResult::Ok(result),
        }
    }

    pub fn error(command_id: Option<CommandId>, error: ProtocolError) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            command_id,
            result: ReplyResult::Error(error),
        }
    }

    pub fn into_result(self) -> Result<CommandResult, ProtocolError> {
        match self.result {
            ReplyResult::Ok(result) => Ok(result),
            ReplyResult::Error(error) => Err(error),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "outcome", content = "value", rename_all = "snake_case")]
// A wire message is built once and serialized; boxing its larger variants
// would only add noise at every construction site.
#[allow(clippy::large_enum_variant)]
pub enum ReplyResult {
    Ok(CommandResult),
    Error(ProtocolError),
}

/// A successful command's result. A repeated command returns its original
/// result even if the job has moved on since (contract §5).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type")]
pub enum CommandResult {
    /// One job, after the command.
    Job {
        job: JobSnapshot,
    },
    /// `Pause` and `Cancel`: whether the request was taken, per the contract's
    /// command/state matrix. `accepted` means intent is recorded; the
    /// outcome arrives later as a state event.
    Control {
        outcome: ControlOutcome,
        job: JobSnapshot,
    },
    Jobs {
        jobs: Vec<JobSnapshot>,
    },
    Details {
        details: JobDetails,
    },
    MediaInspection {
        inspection: MediaInspection,
    },
    LinkInspection {
        inspection: LinkInspection,
    },
    QueueStats {
        stats: QueueStats,
    },
    Settings {
        view: SettingsView,
    },
    Removed {
        job_id: JobId,
    },
    /// `TakeLinkReviews`: links as the browser sent them, never shown whole.
    LinkReviews {
        urls: Vec<SensitiveUrl>,
    },
    /// The subscription starts here: every retained event after `position`
    /// follows, then new ones.
    Subscribed {
        position: StreamPosition,
    },
    /// The requested events were compacted. These snapshots are atomic with
    /// the stream, which continues after `position`.
    SnapshotBoundary {
        jobs: Vec<JobSnapshot>,
        position: StreamPosition,
    },
    EngineStatus {
        status: EngineStatus,
    },
    /// Every agent the person configured, sorted by name.
    AgentPolicies {
        policies: Vec<AgentAccess>,
    },
    /// Every smart rule, in the order they are tried.
    Rules {
        rules: Vec<Rule>,
    },
    /// The content cache's use and bounds.
    Cache {
        cache: CacheView,
    },
    /// The engine is stopping after finishing in-flight work safely.
    ShuttingDown,
    /// Browser captures waiting in the inbox were taken in (FP-056).
    CapturesTaken,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ControlOutcome {
    Accepted,
    /// Already in the requested state.
    NoOp,
    /// The job already finished or was cancelled.
    AlreadyTerminal,
    /// Pause after the publication fence.
    TooLate,
    /// Cancel after the publication fence.
    TooLateToCancel,
    /// A cancellation is already under way with its own cleanup policy.
    CancelInProgress,
    #[serde(other)]
    Unknown,
}

/// Where an event stream resumes.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "stream", rename_all = "snake_case")]
pub enum StreamPosition {
    Job { job_id: JobId, after_seq: u64 },
    Queue { after_cursor: u64 },
}

/// A durable job event. `seq` rises by exactly one per job; `cursor` orders
/// events across all jobs. Delivery may repeat, so clients deduplicate by
/// `(job_id, seq)`.
#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub struct JobEvent {
    pub schema_version: u32,
    pub job_id: JobId,
    pub seq: u64,
    pub cursor: u64,
    pub job_revision: u64,
    pub occurred_at: Timestamp,
    #[serde(flatten)]
    pub payload: EventPayload,
    #[serde(default)]
    pub correlation: Correlation,
}

/// Event kinds this build reads. Any other kind, from a newer engine, reads
/// as [`EventPayload::Unknown`] with its `seq` intact, so the stream has no
/// gap and the client does not fail.
pub const KNOWN_EVENT_KINDS: &[&str] = &[
    "job_created",
    "state_changed",
    "policy_changed",
    "source_changed",
    "media_choices_ready",
    "checkpoint_committed",
    "integrity_changed",
    "waiting",
    "warning",
    "error_recorded",
    "publication_completed",
    "job_removed",
];

impl<'de> Deserialize<'de> for JobEvent {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Raw {
            schema_version: u32,
            job_id: JobId,
            seq: u64,
            cursor: u64,
            job_revision: u64,
            occurred_at: Timestamp,
            kind: String,
            #[serde(default)]
            public_payload: Option<serde_json::Value>,
            #[serde(default)]
            correlation: Correlation,
        }

        let raw = Raw::deserialize(deserializer)?;
        let payload = if KNOWN_EVENT_KINDS.contains(&raw.kind.as_str()) {
            let mut tagged = serde_json::Map::new();
            tagged.insert("kind".into(), raw.kind.into());
            if let Some(content) = raw.public_payload {
                tagged.insert("public_payload".into(), content);
            }
            serde_json::from_value(serde_json::Value::Object(tagged))
                .map_err(serde::de::Error::custom)?
        } else {
            EventPayload::Unknown
        };
        Ok(Self {
            schema_version: raw.schema_version,
            job_id: raw.job_id,
            seq: raw.seq,
            cursor: raw.cursor,
            job_revision: raw.job_revision,
            occurred_at: raw.occurred_at,
            payload,
            correlation: raw.correlation,
        })
    }
}

/// The contract's durable event families, with redacted public payloads.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", content = "public_payload", rename_all = "snake_case")]
// A wire message is built once and serialized; boxing its larger variants
// would only add noise at every construction site.
#[allow(clippy::large_enum_variant)]
pub enum EventPayload {
    JobCreated {
        job: JobSnapshot,
    },
    StateChanged {
        previous: JobState,
        state: JobState,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        waiting_reason: Option<WaitingReason>,
    },
    PolicyChanged {
        #[serde(default)]
        not_before: Option<Timestamp>,
    },
    SourceChanged {
        source_display: String,
    },
    MediaChoicesReady {
        inspection: MediaInspection,
    },
    CheckpointCommitted {
        bytes_checkpointed: u64,
    },
    IntegrityChanged {
        integrity: IntegrityOutcome,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        observed_sha256: Option<String>,
    },
    Waiting {
        reason: WaitingReason,
    },
    Warning {
        message_key: String,
        message: String,
    },
    ErrorRecorded {
        error: ProtocolError,
    },
    PublicationCompleted {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        destination: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        observed_sha256: Option<String>,
    },
    JobRemoved,
    /// An event kind this client does not know, from a newer engine. Its
    /// `seq` still counts, so the stream has no gap.
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Correlation {
    #[serde(default)]
    pub command_id: Option<CommandId>,
    #[serde(default)]
    pub attempt_id: Option<AttemptId>,
}

/// An ephemeral progress sample. It never consumes `seq` and may be dropped
/// or coalesced for a slow client; the current aggregate is in every
/// snapshot.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ProgressSample {
    pub schema_version: u32,
    pub job_id: JobId,
    pub sample_cursor: u64,
    pub job_revision: u64,
    pub occurred_at: Timestamp,
    pub kind: ProgressKind,
    pub public_payload: Progress,
    #[serde(default)]
    pub correlation: Correlation,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProgressKind {
    #[default]
    ProgressSampled,
}
