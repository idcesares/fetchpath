//! The engine's protocol records in the shapes the interface already reads
//! (`JobSnapshot`, `SettingsView` and the rest in `src/main.ts`), so the
//! interface did not change when the queue moved into the engine (FP-055).

use fetchpath_protocol::command::{ConflictPolicy, DestinationIntent, JobInput, JobRequest};
use fetchpath_protocol::error::Action;
use fetchpath_protocol::model::{
    self, Density, EngineSettings, JobKind, JobState, MediaInspection, MediaVariantKind, Theme,
};
use fetchpath_protocol::principal::{AgentAccess, ApprovalReason};
use fetchpath_protocol::{JobSnapshot, SensitiveUrl, Timestamp};
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

/// One rule, in the words every client uses.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleView {
    pub id: u32,
    pub label: String,
    pub when: String,
    pub then: String,
}

pub fn rules(rules: &[fetchpath_protocol::model::Rule]) -> Vec<RuleView> {
    use fetchpath_protocol::describe;
    rules
        .iter()
        .map(|rule| RuleView {
            id: rule.id,
            label: describe::label(rule),
            when: describe::conditions(&rule.spec.when),
            then: describe::actions(&rule.spec.then),
        })
        .collect()
}

/// How the rules decide for one link: for Add download and for testing a
/// link in Settings.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleAdvice {
    /// "Rule 1 (Disc images): a .iso file", when one matches.
    pub matched: Option<String>,
    pub folder: Option<String>,
    pub needs_checksum: bool,
    /// Every rule tried and why, as `fetchpath rules test` prints it.
    pub lines: Vec<String>,
}

pub fn rule_advice(verdict: Option<&fetchpath_protocol::model::RulesVerdict>) -> RuleAdvice {
    let then = verdict
        .and_then(|verdict| verdict.matched.as_ref())
        .map(|rule| &rule.spec.then);
    RuleAdvice {
        matched: fetchpath_protocol::describe::matched(verdict),
        folder: then.and_then(|then| then.folder.clone()),
        needs_checksum: then.is_some_and(|then| then.require_checksum),
        lines: fetchpath_protocol::describe::verdict(verdict),
    }
}

/// One agent's access, as Settings shows it.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentView {
    pub name: String,
    pub folders: Vec<String>,
    pub max_bytes: u64,
    pub max_new_jobs_per_hour: u32,
    /// Inside its folders, nothing it downloads waits for approval.
    pub automatic: bool,
}

pub fn agents(policies: &[AgentAccess]) -> Vec<AgentView> {
    policies
        .iter()
        .map(|access| AgentView {
            name: access.agent.to_string(),
            folders: access.policy.folders.clone(),
            max_bytes: access.policy.max_bytes,
            max_new_jobs_per_hour: access.policy.max_new_jobs_per_hour,
            automatic: access.policy.automatic,
        })
        .collect()
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

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaDraft {
    pub url: String,
    pub variant_id: String,
    pub quality_label: String,
    pub destination: String,
    pub not_before_ms: Option<u64>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TorrentDraft {
    pub url: String,
    pub destination: String,
    pub not_before_ms: Option<u64>,
    pub discover_peers: bool,
    pub upload: bool,
}

pub fn link(url: &str) -> Result<SensitiveUrl, String> {
    SensitiveUrl::try_from(url.trim().to_owned())
        .map_err(|reason| format!("That address cannot be used: {reason}."))
}

fn at(ms: Option<u64>) -> Option<Timestamp> {
    ms.map(|ms| Timestamp::from_unix_ms(i64::try_from(ms).unwrap_or(i64::MAX)))
}

fn destination(path: &str) -> DestinationIntent {
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

impl MediaDraft {
    pub fn request(&self) -> Result<JobRequest, String> {
        Ok(JobRequest::Media {
            input: JobInput::Url {
                url: link(&self.url)?,
            },
            destination: destination(&self.destination),
            not_before: at(self.not_before_ms),
            variant_id: self.variant_id.clone(),
            quality_label: self.quality_label.clone(),
        })
    }
}

impl TorrentDraft {
    pub fn request(&self) -> Result<JobRequest, String> {
        let source = self.url.trim();
        let input = if source.starts_with("magnet:?") || source.starts_with("https://") {
            JobInput::Url { url: link(source)? }
        } else {
            JobInput::TorrentFile {
                path: source.to_owned(),
            }
        };
        Ok(JobRequest::Torrent {
            input,
            destination: destination(&self.destination),
            not_before: at(self.not_before_ms),
            discover_peers: Some(self.discover_peers),
            upload: self.upload,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub max_active_downloads: u64,
    pub default_destination_dir: Option<String>,
    pub auto_retry: bool,
    pub auto_retry_max_attempts: u32,
    pub auto_retry_base_delay_seconds: u64,
    pub close_to_tray: bool,
    pub power_mode: bool,
    pub media_tools_dir: Option<String>,
    pub confirm_remove_completed: bool,
    pub theme: String,
    pub onboarding_completed: bool,
    #[serde(default)]
    pub cache_quota_bytes: Option<u64>,
    /// `comfortable` or `compact`.
    #[serde(default)]
    pub density: Option<String>,
    /// The name this engine shows on every client (contract D6). Absent
    /// leaves it unchanged; blank returns to the computer's name.
    #[serde(default)]
    pub instance_name: Option<String>,
    /// Keep the engine running in the background. Absent leaves it.
    #[serde(default)]
    pub hub_mode: Option<bool>,
}

impl Settings {
    pub fn from_engine(settings: &EngineSettings) -> Self {
        Self {
            max_active_downloads: settings.max_active_downloads,
            default_destination_dir: settings.default_destination_dir.clone(),
            auto_retry: settings.auto_retry,
            auto_retry_max_attempts: settings.auto_retry_max_attempts,
            auto_retry_base_delay_seconds: settings.auto_retry_base_delay_seconds,
            close_to_tray: settings.close_to_tray,
            power_mode: settings.power_mode,
            media_tools_dir: settings.media_tools_dir.clone(),
            confirm_remove_completed: settings.confirm_remove_completed,
            theme: match settings.theme {
                Theme::Light => "light",
                Theme::Dark => "dark",
                Theme::HighContrast => "high-contrast",
                Theme::System | Theme::Unknown => "system",
            }
            .into(),
            onboarding_completed: settings.onboarding_completed,
            cache_quota_bytes: settings.cache_quota_bytes,
            density: Some(
                match settings.density {
                    Some(Density::Compact) => "compact",
                    _ => "comfortable",
                }
                .into(),
            ),
            instance_name: settings.instance_name.clone(),
            hub_mode: settings.hub_mode,
        }
    }

    /// The settings to send. The interface has no sign-in start switch yet,
    /// so that one is left out, which the engine reads as "unchanged".
    pub fn to_engine(&self) -> EngineSettings {
        EngineSettings {
            max_active_downloads: self.max_active_downloads,
            default_destination_dir: self.default_destination_dir.clone(),
            auto_retry: self.auto_retry,
            auto_retry_max_attempts: self.auto_retry_max_attempts,
            auto_retry_base_delay_seconds: self.auto_retry_base_delay_seconds,
            close_to_tray: self.close_to_tray,
            power_mode: self.power_mode,
            media_tools_dir: self.media_tools_dir.clone(),
            confirm_remove_completed: self.confirm_remove_completed,
            theme: match self.theme.as_str() {
                "light" => Theme::Light,
                "dark" => Theme::Dark,
                "high-contrast" => Theme::HighContrast,
                _ => Theme::System,
            },
            onboarding_completed: self.onboarding_completed,
            start_engine_at_sign_in: None,
            cache_quota_bytes: self.cache_quota_bytes,
            density: match self.density.as_deref() {
                Some("compact") => Some(Density::Compact),
                Some("comfortable") => Some(Density::Comfortable),
                _ => None,
            },
            instance_name: self.instance_name.clone(),
            hub_mode: self.hub_mode,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsView {
    pub settings: Settings,
    pub repaired: bool,
    pub max_active_limit: u64,
    pub max_retry_attempts: u32,
    /// The folder used when no default destination is set.
    pub system_download_dir: Option<String>,
}

impl SettingsView {
    pub fn new(view: &model::SettingsView, system_download_dir: Option<String>) -> Self {
        Self {
            settings: Settings::from_engine(&view.settings),
            repaired: view.repaired,
            max_active_limit: view.max_active_limit,
            max_retry_attempts: view.max_retry_attempts,
            system_download_dir,
        }
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

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaVariant {
    pub id: String,
    pub label: String,
    pub kind: &'static str,
    pub extension: String,
    pub height: Option<u32>,
    pub fps: Option<u32>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Inspection {
    pub title: String,
    pub duration_seconds: Option<f64>,
    pub variants: Vec<MediaVariant>,
}

impl From<&MediaInspection> for Inspection {
    fn from(inspection: &MediaInspection) -> Self {
        Self {
            title: inspection.title.clone(),
            duration_seconds: inspection.duration_seconds,
            variants: inspection
                .variants
                .iter()
                .filter(|variant| variant.kind != MediaVariantKind::Unknown)
                .map(|variant| MediaVariant {
                    id: variant.id.clone(),
                    label: variant.label.clone(),
                    kind: match variant.kind {
                        MediaVariantKind::Audio => "audio",
                        _ => "video",
                    },
                    extension: variant.extension.clone(),
                    height: variant.height,
                    fps: variant.fps,
                })
                .collect(),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CancelResponse {
    pub outcome: &'static str,
    pub job: JobView,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn torrent_draft_preserves_local_file_as_a_torrent_input() {
        let draft = TorrentDraft {
            url: r"C:\Downloads\debian.torrent".into(),
            destination: r"C:\Downloads\debian".into(),
            not_before_ms: None,
            discover_peers: true,
            upload: false,
        };
        assert!(matches!(
            draft.request().unwrap(),
            JobRequest::Torrent {
                input: JobInput::TorrentFile { .. },
                ..
            }
        ));
        let url = TorrentDraft {
            url: "magnet:?xt=urn:btih:abc".into(),
            ..draft
        };
        assert!(matches!(
            url.request().unwrap(),
            JobRequest::Torrent {
                input: JobInput::Url { .. },
                ..
            }
        ));
    }

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
    fn settings_round_trip_and_leave_sign_in_start_alone() {
        let engine = EngineSettings {
            max_active_downloads: 4,
            default_destination_dir: Some("D:\\Downloads".into()),
            auto_retry: false,
            auto_retry_max_attempts: 2,
            auto_retry_base_delay_seconds: 30,
            close_to_tray: true,
            power_mode: true,
            media_tools_dir: None,
            confirm_remove_completed: false,
            theme: Theme::Dark,
            onboarding_completed: true,
            start_engine_at_sign_in: Some(true),
            cache_quota_bytes: Some(1 << 30),
            density: Some(Density::Comfortable),
            instance_name: Some("Studio PC".into()),
            hub_mode: Some(false),
        };
        let shown = Settings::from_engine(&engine);
        assert_eq!(serde_json::to_value(&shown).unwrap()["theme"], "dark");
        let back = shown.to_engine();
        assert_eq!(back.start_engine_at_sign_in, None);
        assert_eq!(
            EngineSettings {
                start_engine_at_sign_in: Some(true),
                ..back
            },
            engine
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
