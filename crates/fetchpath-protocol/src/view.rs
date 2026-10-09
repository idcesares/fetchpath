//! The shapes every client's interface reads for jobs, shared by the desktop
//! and the browser listener (FP-104).

use crate::command::{ConflictPolicy, DestinationIntent, JobInput, JobRequest};
use crate::error::Action;
use crate::model::{self, JobKind, JobState};
use crate::principal::ApprovalReason;
use crate::{JobSnapshot, SensitiveUrl, Timestamp};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JobView {
    pub job_id: String,
    pub source: String,
    pub state: &'static str,
    pub bytes_received: u64,
    pub total_bytes: Option<u64>,
    pub bytes_per_second: Option<u64>,
    pub eta_seconds: Option<u64>,
    pub attempt: u32,
    pub destination: Option<String>,
    pub observed_sha256: Option<String>,
    pub expected_sha256: Option<String>,
    pub cleanup_pending: bool,
    pub error: Option<String>,
    pub action: Option<&'static str>,
    pub retryable: bool,
    pub created_at_ms: u64,
    pub not_before_ms: Option<u64>,
    pub finished_at_ms: Option<u64>,
    pub kind: &'static str,
    pub quality_label: Option<String>,
    /// The agent that asked for it, when an agent did (FP-066).
    pub agent: Option<String>,
    /// Why it waits for the person: `outside_granted_folders`, `size_limit`
    /// or `rate_limit`.
    pub approval_reasons: Vec<&'static str>,
    /// Completed from this computer's cache, not transferred (FP-032).
    pub reused_from_cache: bool,
    /// The fingerprint of the paired computer it came from (FP-034).
    pub from_paired_device: Option<String>,
    /// Queued until its drive has room above the disk reserve (FP-101).
    pub waiting_for_space: bool,
}

fn ms(at: Timestamp) -> u64 {
    u64::try_from(at.unix_ms()).unwrap_or(0)
}

/// The interface's state names. A queued job with a start still ahead is
/// scheduled; the engine's finer running phases all read as downloading.
fn state(job: &JobSnapshot, now: Timestamp) -> &'static str {
    match job.state {
        JobState::Queued if job.not_before.is_some_and(|at| at > now) => "scheduled",
        JobState::Queued | JobState::Ready | JobState::WaitingForSelection => "queued",
        JobState::Probing
        | JobState::Running
        | JobState::Pausing
        | JobState::Verifying
        | JobState::Publishing => "running",
        JobState::Paused => "paused",
        JobState::Cancelling => "cancelling",
        JobState::Cancelled => "cancelled",
        JobState::Completed => "completed",
        JobState::Failed => "failed",
        JobState::WaitingForSource => "needs_source",
        JobState::AwaitingApproval => "awaiting_approval",
        // From a newer engine: shown as needing attention, with no action.
        JobState::Unknown => "failed",
    }
}

fn action(action: Action) -> Option<&'static str> {
    Some(match action {
        Action::Retry => "retry",
        Action::CorrectInput | Action::EditLink => "edit_link",
        Action::Recapture => "recapture",
        Action::ChooseNewPath => "choose_new_path",
        Action::CheckChecksum => "check_checksum",
        Action::ConfigureMediaTools => "configure_media_tools",
        Action::RefreshSource | Action::RefreshMediaChoices | Action::CreateReplacementJob => {
            "refresh_source"
        }
        _ => return None,
    })
}

pub fn job(job: &JobSnapshot, now: Timestamp) -> JobView {
    JobView {
        job_id: job.job_id.to_string(),
        source: job.source_display.clone(),
        state: state(job, now),
        waiting_for_space: job.waiting_reason == Some(model::WaitingReason::StorageReserve),
        bytes_received: job.progress.bytes_received,
        total_bytes: job.progress.bytes_total,
        bytes_per_second: job.progress.rate_bytes_per_second,
        eta_seconds: job.progress.eta_seconds,
        attempt: job.attempt,
        destination: job.destination.clone(),
        observed_sha256: job.observed_sha256.clone(),
        expected_sha256: job.expected_sha256.clone(),
        cleanup_pending: job.cleanup_pending,
        error: job.error.as_ref().map(|error| error.message.clone()),
        action: job
            .error
            .as_ref()
            .and_then(|error| error.action)
            .and_then(action),
        retryable: job.error.as_ref().is_some_and(|error| error.retryable),
        created_at_ms: ms(job.created_at),
        not_before_ms: job.not_before.map(ms),
        finished_at_ms: job.finished_at.map(ms),
        kind: match job.kind {
            JobKind::Media => "media",
            JobKind::Torrent => "torrent",
            _ => "file",
        },
        quality_label: job.quality_label.clone(),
        reused_from_cache: job.reused_from_cache,
        from_paired_device: job.from_paired_device.clone(),
        agent: job.principal.agent().map(ToString::to_string),
        approval_reasons: job
            .approval
            .as_ref()
            .map(|approval| {
                approval
                    .reasons
                    .iter()
                    .map(|reason| match reason {
                        ApprovalReason::OutsideGrantedFolders => "outside_granted_folders",
                        ApprovalReason::SizeLimit => "size_limit",
                        ApprovalReason::RateLimit => "rate_limit",
                        ApprovalReason::PeerDiscovery => "peer_discovery",
                        ApprovalReason::PeerUpload => "peer_upload",
                        ApprovalReason::Unknown => "unknown",
                    })
                    .collect()
            })
            .unwrap_or_default(),
    }
}

pub fn jobs(jobs: &[JobSnapshot]) -> Vec<JobView> {
    let now = Timestamp::now();
    jobs.iter().map(|snapshot| job(snapshot, now)).collect()
}

/// A file draft from the Add download dialog.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JobDraft {
    pub url: String,
    pub destination: String,
    pub not_before_ms: Option<u64>,
    #[serde(default)]
    pub checksum: Option<String>,
}

pub fn link(url: &str) -> Result<SensitiveUrl, String> {
    SensitiveUrl::try_from(url.trim().to_owned())
        .map_err(|reason| format!("That address cannot be used: {reason}."))
}

pub fn at(ms: Option<u64>) -> Option<Timestamp> {
    ms.map(|ms| Timestamp::from_unix_ms(i64::try_from(ms).unwrap_or(i64::MAX)))
}

pub fn destination(path: &str) -> DestinationIntent {
    DestinationIntent {
        path: path.to_owned(),
        conflict: ConflictPolicy::Ask,
    }
}

impl JobDraft {
    pub fn request(&self) -> Result<JobRequest, String> {
        Ok(JobRequest::File {
            input: JobInput::Url {
                url: link(&self.url)?,
            },
            destination: destination(&self.destination),
            not_before: at(self.not_before_ms),
            // An empty field means no checksum, as the queue always took it.
            expected_sha256: self.checksum.clone().filter(|sum| !sum.trim().is_empty()),
        })
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueStats {
    pub running: u64,
    pub queued: u64,
    pub scheduled: u64,
    pub paused: u64,
    pub completed: u64,
    pub failed: u64,
    pub active_bytes: u64,
    pub completed_bytes: u64,
    pub combined_bytes_per_second: u64,
    pub max_active_downloads: u64,
}

impl From<&model::QueueStats> for QueueStats {
    fn from(stats: &model::QueueStats) -> Self {
        Self {
            running: stats.running,
            queued: stats.queued,
            scheduled: stats.scheduled,
            paused: stats.paused,
            completed: stats.completed,
            failed: stats.failed,
            active_bytes: stats.active_bytes,
            completed_bytes: stats.completed_bytes,
            combined_bytes_per_second: stats.combined_bytes_per_second,
            max_active_downloads: stats.max_active_downloads,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Segment {
    pub start: u64,
    pub end: u64,
    pub received: u64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JobDetails {
    pub job: JobView,
    pub segments: Vec<Segment>,
}

impl From<&model::JobDetails> for JobDetails {
    fn from(details: &model::JobDetails) -> Self {
        Self {
            job: job(&details.job, Timestamp::now()),
            segments: details
                .segments
                .iter()
                .map(|segment| Segment {
                    start: segment.start,
                    end: segment.end,
                    received: segment.received,
                })
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn snapshot(fields: serde_json::Value) -> JobSnapshot {
        let mut base = json!({
            "job_id": "3f1c2a9b-0000-4000-8000-000000000001",
            "kind": "file",
            "state": "queued",
            "job_revision": 4,
            "last_seq": 4,
            "source_display": "https://example.test/a.zip?…",
            "destination": "C:\\Downloads\\a.zip",
            "progress": { "bytes_received": 10, "bytes_total": 40, "rate_bytes_per_second": 5, "eta_seconds": 6 },
            "created_at": "2026-09-25T10:00:00Z",
        });
        for (key, value) in fields.as_object().unwrap() {
            base[key] = value.clone();
        }
        serde_json::from_value(base).unwrap()
    }

    fn view(fields: serde_json::Value, now: &str) -> serde_json::Value {
        serde_json::to_value(job(&snapshot(fields), Timestamp::parse(now).unwrap())).unwrap()
    }

    #[test]
    fn a_snapshot_reads_as_the_interface_expects() {
        let shown = view(json!({ "state": "running" }), "2026-09-25T10:00:01Z");
        assert_eq!(
            shown,
            json!({
                "jobId": "3f1c2a9b-0000-4000-8000-000000000001",
                "source": "https://example.test/a.zip?…",
                "state": "running",
                "bytesReceived": 10,
                "totalBytes": 40,
                "waitingForSpace": false,
                "bytesPerSecond": 5,
                "etaSeconds": 6,
                "attempt": 0,
                "destination": "C:\\Downloads\\a.zip",
                "observedSha256": null,
                "expectedSha256": null,
                "cleanupPending": false,
                "error": null,
                "action": null,
                "retryable": false,
                "createdAtMs": 1_790_330_400_000_u64,
                "notBeforeMs": null,
                "finishedAtMs": null,
                "kind": "file",
                "qualityLabel": null,
                "agent": null,
                "approvalReasons": [],
                "reusedFromCache": false,
                "fromPairedDevice": null,
            })
        );
    }

    #[test]
    fn an_agent_request_names_its_agent_and_reasons() {
        let shown = view(
            json!({
                "state": "awaiting_approval",
                "principal": "agent:claude-code",
                "approval": { "reasons": ["outside_granted_folders", "size_limit"] },
            }),
            "2026-09-25T10:00:01Z",
        );
        assert_eq!(shown["state"], "awaiting_approval");
        assert_eq!(shown["agent"], "claude-code");
        assert_eq!(
            shown["approvalReasons"],
            json!(["outside_granted_folders", "size_limit"])
        );
    }

    #[test]
    fn states_and_actions_map_to_the_interface_names() {
        let now = "2026-09-25T10:00:00Z";
        let later = json!({ "not_before": "2026-09-25T11:00:00Z" });
        assert_eq!(view(later, now)["state"], "scheduled");
        let due = json!({ "not_before": "2026-09-25T09:00:00Z" });
        assert_eq!(view(due, now)["state"], "queued");
        let conflict = view(
            json!({
                "state": "failed",
                "error": {
                    "code": "storage.destination_conflict",
                    "message_key": "storage.destination_conflict",
                    "message": "A file already exists at this destination.",
                    "retryable": false,
                    "action": "choose_new_path",
                    "scope": "job"
                }
            }),
            now,
        );
        assert_eq!(conflict["state"], "failed");
        assert_eq!(conflict["action"], "choose_new_path");
        assert_eq!(
            conflict["error"],
            "A file already exists at this destination."
        );
        let expired = view(
            json!({
                "state": "waiting_for_source",
                "error": {
                    "code": "source.link_expired",
                    "message_key": "source.link_expired",
                    "message": "Paste a refreshed link.",
                    "retryable": false,
                    "action": "edit_link",
                    "scope": "job"
                }
            }),
            now,
        );
        assert_eq!(expired["state"], "needs_source");
        assert_eq!(expired["action"], "edit_link");
        assert_eq!(
            view(json!({ "state": "a_state_from_the_future" }), now)["state"],
            "failed"
        );
    }

    #[test]
    fn an_empty_checksum_field_means_no_checksum() {
        let draft = JobDraft {
            url: " https://example.test/a.zip ".into(),
            destination: "C:\\Downloads\\a.zip".into(),
            not_before_ms: None,
            checksum: Some("  ".into()),
        };
        let JobRequest::File {
            input: JobInput::Url { url },
            expected_sha256,
            ..
        } = draft.request().unwrap()
        else {
            panic!()
        };
        assert_eq!(url.expose(), "https://example.test/a.zip");
        assert_eq!(expected_sha256, None);
    }
}
