//! Public and crate-internal data types for the download session.

use crate::settings;
use fetchpath_protocol::model::JobPriority;
use fetchpath_protocol::principal::{ApprovalReason, Principal};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// What the inbox may offer from a completed download (contract D8).
pub(crate) struct PublishedOutput {
    pub path: PathBuf,
    pub owner: Principal,
    pub public_origin: bool,
    pub origin: Option<String>,
}

/// A job an agent asked for outside its policy (contract D1). While it is
/// pending nothing about the job starts; a denial ends it cancelled. A
/// request the agent withdrew keeps its reasons, so retrying it asks the
/// person again instead of slipping past them.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Approval {
    pub(crate) reasons: Vec<ApprovalReason>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub(crate) denied: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub(crate) withdrawn: bool,
    /// When the wait began (FP-101). Absent from a request saved before
    /// expiry existed, which then counts from when the download was made.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) requested_at_ms: Option<u64>,
    /// Nobody decided within the expiry period (contract D6).
    #[serde(default, skip_serializing_if = "is_false")]
    pub(crate) expired: bool,
}

/// How long a request waits for the person before it expires (contract D6,
/// decision O3).
pub const APPROVAL_EXPIRY_MS: u64 = 7 * 24 * 60 * 60 * 1000;

pub(crate) fn is_false(value: &bool) -> bool {
    !*value
}

/// Who a new job is for and whether it must wait for approval.
#[derive(Clone, Debug, Default)]
pub(crate) struct Origin {
    pub principal: Principal,
    pub approval: Vec<ApprovalReason>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JobDraft {
    pub url: String,
    pub destination: String,
    pub not_before_ms: Option<u64>,
    /// A SHA-256 as the person pasted it. Normalized when the draft is queued.
    #[serde(default)]
    pub checksum: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaDraft {
    pub url: String,
    pub variant_id: String,
    pub quality_label: String,
    pub destination: String,
    pub not_before_ms: Option<u64>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TorrentPolicy {
    pub discover_peers: bool,
    pub upload: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TorrentDraft {
    pub source: String,
    pub destination: String,
    pub not_before_ms: Option<u64>,
    pub policy: TorrentPolicy,
    /// `destination` is the root of an engine-named folder (set by the engine).
    #[serde(skip)]
    pub auto: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JobSnapshot {
    pub job_id: String,
    pub source: String,
    pub state: String,
    /// Queued, but its drive has no room above the disk reserve (FP-101).
    #[serde(default, skip_serializing_if = "is_false")]
    pub waiting_for_space: bool,
    pub bytes_received: u64,
    /// Engine-confirmed total. Absent whenever the source never stated a
    /// length, which the interface shows as an unknown size rather than a
    /// percentage it cannot support.
    #[serde(default)]
    pub total_bytes: Option<u64>,
    /// Smoothed transfer rate, present only while bytes are actually moving.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes_per_second: Option<u64>,
    /// Remaining time, present only when both a total and a rate are known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eta_seconds: Option<u64>,
    /// How many automatic retries this download has already consumed.
    #[serde(default)]
    pub attempt: u32,
    pub destination: Option<String>,
    pub observed_sha256: Option<String>,
    /// The SHA-256 the person supplied, normalized. When present the engine
    /// publishes nothing that does not match it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_sha256: Option<String>,
    pub cleanup_pending: bool,
    pub error: Option<String>,
    /// The stable code of a failure, such as `source.transfer_failed`. Set
    /// only on failed rows; clients act on it and on `action`, never on the
    /// message (finding F3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
    pub action: Option<String>,
    pub retryable: bool,
    pub created_at_ms: u64,
    pub not_before_ms: Option<u64>,
    pub finished_at_ms: Option<u64>,
    #[serde(default = "default_job_kind")]
    pub kind: String,
    #[serde(default)]
    pub quality_label: Option<String>,
    /// Completed from this computer's cache rather than a transfer (FP-032).
    #[serde(default, skip_serializing_if = "is_false")]
    pub reused_from_cache: bool,
    /// The fingerprint of the paired device it came from (FP-034).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_paired_device: Option<String>,
    /// Queue scheduling priority for this job (FP-150). Absent means normal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<JobPriority>,
    /// Optional per-job bandwidth ceiling in bytes/second (FP-150).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_bytes_per_second: Option<u64>,
}

/// Aggregate queue figures, for the statistics panel.
///
/// Every field is counted from the same reconciled snapshot, so the totals
/// agree with the rows on screen rather than describing a slightly different
/// moment.
#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueStats {
    pub(crate) running: usize,
    pub(crate) queued: usize,
    pub(crate) scheduled: usize,
    pub(crate) paused: usize,
    pub(crate) completed: usize,
    pub(crate) failed: usize,
    /// Bytes received by downloads that are running right now.
    pub(crate) active_bytes: u64,
    /// Bytes received by downloads that finished and are still in the list.
    pub(crate) completed_bytes: u64,
    /// Sum of the per-download rates. This is throughput actually observed, not
    /// a link-capacity measurement.
    pub(crate) combined_bytes_per_second: u64,
    pub(crate) max_active_downloads: usize,
    pub(crate) awaiting_approval: usize,
}

/// Where the content cache lives, and the bounds of its quota (FP-032).
pub const MIN_CACHE_QUOTA_BYTES: u64 = settings::MIN_CACHE_QUOTA_BYTES;
pub const MAX_CACHE_QUOTA_BYTES: u64 = settings::MAX_CACHE_QUOTA_BYTES;

/// One byte range in flight: received into memory, not yet written.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SegmentView {
    pub(crate) start: u64,
    /// Inclusive.
    pub(crate) end: u64,
    pub(crate) received: u64,
}

/// What the details window shows: the row itself plus its ranges in flight.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JobDetails {
    pub(crate) job: JobSnapshot,
    pub(crate) segments: Vec<SegmentView>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CancelResponse {
    pub(crate) outcome: &'static str,
    pub(crate) job: JobSnapshot,
}

pub(crate) fn default_job_kind() -> String {
    "file".into()
}
