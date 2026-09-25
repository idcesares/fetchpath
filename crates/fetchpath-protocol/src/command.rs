//! Commands and their envelope (job contract §5).

use crate::SCHEMA_VERSION;
use crate::ids::{ClientId, CommandId, CredentialRef, JobId, Timestamp};
use crate::model::{EngineSettings, SensitiveUrl};
use crate::principal::{AgentName, AgentPolicy};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Every command a client sends. The engine deduplicates by
/// `(client_id, command_id)` and acknowledges only after the mutation commits,
/// so a client that lost the reply resends the same envelope unchanged.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CommandEnvelope {
    pub schema_version: u32,
    pub client_id: ClientId,
    pub command_id: CommandId,
    pub issued_at: Timestamp,
    /// The job revision the client last saw. A mismatch returns
    /// `contract.revision_conflict` without changing anything.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_revision: Option<u64>,
    pub payload: Command,
}

impl CommandEnvelope {
    /// A new command with a fresh `command_id`, issued now.
    pub fn new(client_id: ClientId, payload: Command) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            client_id,
            command_id: CommandId::random(),
            issued_at: Timestamp::now(),
            expected_revision: None,
            payload,
        }
    }

    pub fn expecting_revision(mut self, revision: u64) -> Self {
        self.expected_revision = Some(revision);
        self
    }
}

/// The command set. Names are the contract's where it has one, plus the
/// engine-level queries and controls from the platform design §4. Rules
/// arrive with FP-064; an engine that does not know a command answers
/// `contract.unknown_command`. Which principal may send which command is the
/// engine's decision (contract D1).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type")]
pub enum Command {
    // Job commands from the contract.
    CreateJob {
        request: JobRequest,
    },
    Start {
        job_id: JobId,
    },
    Pause {
        job_id: JobId,
    },
    Resume {
        job_id: JobId,
    },
    Cancel {
        job_id: JobId,
        /// Keep the partial file instead of removing staging bytes.
        #[serde(default)]
        retain_partial: bool,
    },
    Retry {
        job_id: JobId,
    },
    UpdatePolicy {
        job_id: JobId,
        patch: PolicyPatch,
    },
    ResolveDestination {
        job_id: JobId,
        decision: DestinationDecision,
    },
    SelectMedia {
        job_id: JobId,
        selection_id: String,
    },
    RefreshSource {
        job_id: JobId,
        source: JobInput,
    },
    RefreshMediaChoices {
        job_id: JobId,
    },
    /// Removes a finished or failed job from the list. Its history entry and
    /// ledger records follow the retention rules.
    RemoveJob {
        job_id: JobId,
    },

    // Queries.
    ListJobs {
        #[serde(default)]
        filter: JobFilter,
    },
    GetJob {
        job_id: JobId,
    },
    JobDetails {
        job_id: JobId,
    },
    InspectMedia {
        url: SensitiveUrl,
    },
    QueueStats,
    History {
        /// Matched against display links and file names.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        query: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        limit: Option<u32>,
    },
    GetSettings,
    UpdateSettings {
        settings: EngineSettings,
    },

    // Approvals and agent access (contract D1). The person only.
    /// Lets a job that is `awaiting_approval` run. Approving a size stop
    /// lifts the limit for that job only.
    ApproveJob {
        job_id: JobId,
    },
    /// Refuses a job that is `awaiting_approval`; it ends `cancelled`.
    DenyJob {
        job_id: JobId,
    },
    GetAgentPolicies,
    /// Sets one agent's access, or removes it with `null`, so the agent is
    /// back to asking for everything.
    SetAgentPolicy {
        agent: AgentName,
        #[serde(default)]
        policy: Option<AgentPolicy>,
    },

    // Event streams (contract §8).
    /// Replays durable events after `after_seq`, or answers with a snapshot
    /// boundary when they were compacted, then streams new ones.
    SubscribeJob {
        job_id: JobId,
        after_seq: u64,
    },
    /// The same for every job, ordered by the engine-wide `cursor`.
    SubscribeQueue {
        after_cursor: u64,
    },

    // The engine itself.
    EngineStatus,
    EngineShutdown,
}

impl Command {
    /// The wire name, as it appears in `type`.
    pub fn name(&self) -> &'static str {
        match self {
            Self::CreateJob { .. } => "CreateJob",
            Self::Start { .. } => "Start",
            Self::Pause { .. } => "Pause",
            Self::Resume { .. } => "Resume",
            Self::Cancel { .. } => "Cancel",
            Self::Retry { .. } => "Retry",
            Self::UpdatePolicy { .. } => "UpdatePolicy",
            Self::ResolveDestination { .. } => "ResolveDestination",
            Self::SelectMedia { .. } => "SelectMedia",
            Self::RefreshSource { .. } => "RefreshSource",
            Self::RefreshMediaChoices { .. } => "RefreshMediaChoices",
            Self::RemoveJob { .. } => "RemoveJob",
            Self::ListJobs { .. } => "ListJobs",
            Self::GetJob { .. } => "GetJob",
            Self::JobDetails { .. } => "JobDetails",
            Self::InspectMedia { .. } => "InspectMedia",
            Self::QueueStats => "QueueStats",
            Self::History { .. } => "History",
            Self::GetSettings => "GetSettings",
            Self::UpdateSettings { .. } => "UpdateSettings",
            Self::ApproveJob { .. } => "ApproveJob",
            Self::DenyJob { .. } => "DenyJob",
            Self::GetAgentPolicies => "GetAgentPolicies",
            Self::SetAgentPolicy { .. } => "SetAgentPolicy",
            Self::SubscribeJob { .. } => "SubscribeJob",
            Self::SubscribeQueue { .. } => "SubscribeQueue",
            Self::EngineStatus => "EngineStatus",
            Self::EngineShutdown => "EngineShutdown",
        }
    }

    /// True when the command changes engine state, and so goes through the
    /// durable command ledger.
    pub fn is_mutating(&self) -> bool {
        !matches!(
            self,
            Self::ListJobs { .. }
                | Self::GetJob { .. }
                | Self::JobDetails { .. }
                | Self::InspectMedia { .. }
                | Self::QueueStats
                | Self::History { .. }
                | Self::GetSettings
                | Self::GetAgentPolicies
                | Self::SubscribeJob { .. }
                | Self::SubscribeQueue { .. }
                | Self::EngineStatus
        )
    }
}

/// The immutable request a job is created from (contract §4). Changing any
/// of it later creates a new job.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum JobRequest {
    File {
        input: JobInput,
        destination: DestinationIntent,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        not_before: Option<Timestamp>,
        /// A SHA-256 as the person supplied it. Nothing that does not match
        /// it is published.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expected_sha256: Option<String>,
    },
    Media {
        input: JobInput,
        destination: DestinationIntent,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        not_before: Option<Timestamp>,
        /// The variant id from a media inspection.
        variant_id: String,
        quality_label: String,
    },
}

/// Where a job's bytes come from.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum JobInput {
    Url {
        url: SensitiveUrl,
    },
    /// A request already held by the engine, such as a browser capture. The
    /// secret stays where it is stored.
    CredentialRef {
        credential_ref: CredentialRef,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DestinationIntent {
    /// A full local path including the file name.
    pub path: String,
    #[serde(default)]
    pub conflict: ConflictPolicy,
}

/// What happens when the destination already exists.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ConflictPolicy {
    /// Stop and wait for a decision. Nothing is overwritten.
    #[default]
    Ask,
    /// Replace the existing file. Requires explicit intent from a person.
    ReplaceExisting,
}

/// The answer to a destination conflict.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum DestinationDecision {
    ChooseNewPath { path: String },
    ReplaceExisting,
    Cancel,
}

/// Mutable policy. Absent fields are left as they are.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PolicyPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule: Option<Schedule>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Schedule {
    /// Start as soon as a slot is free.
    Now,
    At {
        not_before: Timestamp,
    },
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum JobFilter {
    #[default]
    All,
    /// Not yet completed or cancelled.
    Active,
    Failed,
    Finished,
    /// Waiting for the person to approve or deny.
    AwaitingApproval,
}
