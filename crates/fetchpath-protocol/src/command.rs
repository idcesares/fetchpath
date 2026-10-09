//! Commands and their envelope (job contract §5).

use crate::SCHEMA_VERSION;
use crate::ids::{ClientId, CommandId, CredentialRef, InstanceId, JobId, Timestamp};
use crate::model::{EngineSettings, RuleSpec, SensitiveUrl};
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
    /// The engine instance the client means (contract D6). An engine with an
    /// identity refuses a change without it (see [`Command::changes_state`]),
    /// and any command naming another instance, with
    /// `contract.wrong_instance`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_instance_id: Option<InstanceId>,
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
            expected_instance_id: None,
            payload,
        }
    }

    pub fn expecting_revision(mut self, revision: u64) -> Self {
        self.expected_revision = Some(revision);
        self
    }

    pub fn for_instance(mut self, instance: InstanceId) -> Self {
        self.expected_instance_id = Some(instance);
        self
    }
}

/// The command set. Names are the contract's where it has one, plus the
/// engine-level queries and controls from the platform design §4. An engine that does not know a command answers
/// `contract.unknown_command`. Which principal may send which command is the
/// engine's decision (contract D1).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type")]
pub enum Command {
    // Job commands from the contract.
    CreateJob {
        request: JobRequest,
    },
    /// Several jobs at once, all or none: if any request is refused, none is
    /// created. File jobs only, from the person.
    CreateJobs {
        requests: Vec<JobRequest>,
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
        /// Replaces the expected SHA-256 before retrying; an empty string
        /// removes it. The person only. Identity replacement revokes earlier
        /// work (contract D2).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expected_sha256: Option<String>,
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
        /// A new full path to save to, changed in the same step. The person
        /// only; an agent uses `ResolveDestination`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        destination: Option<String>,
        /// As for `Retry`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expected_sha256: Option<String>,
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
    /// Looks at a link without downloading it: whether it is a file, a
    /// video or audio page, or a web page, with the file's name and size
    /// when the server states them.
    InspectLink {
        url: SensitiveUrl,
    },
    QueueStats,
    /// Where a signed-in device may save: the default folder and the rules'
    /// folders (contract D6).
    ListFolderChoices,
    /// A single-use sign-in link for the local web UI (FP-104). The engine
    /// host answers it, for the person only; it is never recorded.
    OpenWebUi,
    /// Signs out every browser of the local web UI (FP-104). The engine
    /// host answers it, for the person only.
    SignOutBrowsers,
    History {
        /// Matched against display links and file names.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        query: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        limit: Option<u32>,
    },
    GetSettings,
    /// Media pages the person sent from the browser, to be opened for a
    /// format choice. Each is returned once. The person only.
    TakeLinkReviews,
    /// The browser host has left captures in its inbox: take them in now
    /// rather than at the next tick (FP-056). Idempotent; the inbox stays the
    /// durable handoff. The browser host and the person only.
    TakeBrowserCaptures,
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

    // Smart rules (FP-064). The person only.
    /// Every rule, in the order they are tried.
    ListRules,
    /// Adds a rule: last, or at `position` counted from 1.
    AddRule {
        rule: Box<RuleSpec>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        position: Option<u32>,
    },
    RemoveRule {
        rule_id: u32,
    },

    // The content cache (FP-032). The person only.
    CacheStatus,
    /// Removes every cached file. Downloads already saved are not touched.
    ClearCache,

    /// Resolves a model or dataset repository link to one commit and its
    /// files (FP-022). Reads the provider's public listing; queues nothing.
    InspectRepository {
        url: SensitiveUrl,
    },

    // Paired devices and LAN sharing (FP-033). The person only. None of
    // them reaches the command ledger: pairing codes must never be stored,
    // and none of them changes the queue.
    LanStatus,
    SetLanSharing {
        enabled: bool,
    },
    /// Shows a single-use code for two minutes and waits for one device.
    StartPairing,
    CancelPairing,
    /// Pairs with a device showing `code` at `address` (`host:port`).
    JoinPairing {
        address: String,
        code: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },
    Unpair {
        key: String,
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
            Self::CreateJobs { .. } => "CreateJobs",
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
            Self::InspectLink { .. } => "InspectLink",
            Self::QueueStats => "QueueStats",
            Self::ListFolderChoices => "ListFolderChoices",
            Self::OpenWebUi => "OpenWebUi",
            Self::SignOutBrowsers => "SignOutBrowsers",
            Self::History { .. } => "History",
            Self::GetSettings => "GetSettings",
            Self::TakeLinkReviews => "TakeLinkReviews",
            Self::TakeBrowserCaptures => "TakeBrowserCaptures",
            Self::UpdateSettings { .. } => "UpdateSettings",
            Self::ApproveJob { .. } => "ApproveJob",
            Self::DenyJob { .. } => "DenyJob",
            Self::GetAgentPolicies => "GetAgentPolicies",
            Self::SetAgentPolicy { .. } => "SetAgentPolicy",
            Self::ListRules => "ListRules",
            Self::AddRule { .. } => "AddRule",
            Self::RemoveRule { .. } => "RemoveRule",
            Self::CacheStatus => "CacheStatus",
            Self::ClearCache => "ClearCache",
            Self::InspectRepository { .. } => "InspectRepository",
            Self::LanStatus => "LanStatus",
            Self::SetLanSharing { .. } => "SetLanSharing",
            Self::StartPairing => "StartPairing",
            Self::CancelPairing => "CancelPairing",
            Self::JoinPairing { .. } => "JoinPairing",
            Self::Unpair { .. } => "Unpair",
            Self::SubscribeJob { .. } => "SubscribeJob",
            Self::SubscribeQueue { .. } => "SubscribeQueue",
            Self::EngineStatus => "EngineStatus",
            Self::EngineShutdown => "EngineShutdown",
        }
    }

    /// True when the command changes engine state, and so goes through the
    /// durable command ledger. The LAN commands change device state, not the
    /// queue, and carry pairing codes, so they are not. `TakeLinkReviews` and `TakeBrowserCaptures`
    /// only take the browser inbox in and hand it out, as any query's intake
    /// does, and deal in links, which must never reach the ledger, so they
    /// are not.
    pub fn is_mutating(&self) -> bool {
        !matches!(
            self,
            Self::ListJobs { .. }
                | Self::TakeLinkReviews
                | Self::TakeBrowserCaptures
                | Self::GetJob { .. }
                | Self::JobDetails { .. }
                | Self::InspectMedia { .. }
                | Self::InspectLink { .. }
                | Self::QueueStats
                | Self::ListFolderChoices
                | Self::OpenWebUi
                | Self::SignOutBrowsers
                | Self::History { .. }
                | Self::GetSettings
                | Self::GetAgentPolicies
                | Self::ListRules
                | Self::CacheStatus
                | Self::InspectRepository { .. }
                | Self::LanStatus
                | Self::SetLanSharing { .. }
                | Self::StartPairing
                | Self::CancelPairing
                | Self::JoinPairing { .. }
                | Self::Unpair { .. }
                | Self::SubscribeJob { .. }
                | Self::SubscribeQueue { .. }
                | Self::EngineStatus
        )
    }

    /// True when the command changes anything a person would care which
    /// computer it happened on: every ledgered command, plus LAN sharing and
    /// pairing. Such a command must name its engine instance (contract D6).
    pub fn changes_state(&self) -> bool {
        self.is_mutating()
            || matches!(
                self,
                Self::SetLanSharing { .. }
                    | Self::StartPairing
                    | Self::CancelPairing
                    | Self::JoinPairing { .. }
                    | Self::Unpair { .. }
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
    Torrent {
        input: JobInput,
        destination: DestinationIntent,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        not_before: Option<Timestamp>,
        /// Peer discovery contacts trackers and/or the DHT. Absent means the
        /// default for the submitting principal: on for a person, off for an
        /// agent. A null is treated as absent. An agent that asks for it needs
        /// approval.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        discover_peers: Option<bool>,
        /// Serving pieces to peers is separately explicit and capped. It
        /// never defaults on.
        #[serde(default)]
        upload: bool,
    },
}

/// Where a job's bytes come from.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum JobInput {
    Url {
        url: SensitiveUrl,
    },
    /// A user's local torrent metadata file. The engine snapshots it before
    /// creating a job; the helper never reads this original path.
    TorrentFile {
        path: String,
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
    ChooseNewPath {
        path: String,
        /// As for `Retry`: a corrected expected SHA-256 in the same step, an
        /// empty string removing it. The person only (contract D2).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expected_sha256: Option<String>,
    },
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
