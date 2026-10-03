//! Jobs, snapshots and the other values commands and events carry.

use crate::error::ProtocolError;
use crate::ids::{JobId, Timestamp};
use crate::principal::{ApprovalRequest, Principal};
use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::fmt;

/// Longest link accepted, matching the queue's own limit.
pub const MAX_URL_LENGTH: usize = 8_192;

/// A link exactly as the person gave it, which may carry a signed query
/// string or user info.
///
/// It crosses the protocol whole, because the engine needs it to download,
/// but it is never shown: `Debug` and [`SensitiveUrl::display`] redact it. The
/// engine keeps only a fingerprint of it in the command ledger (FP-051) and
/// persists only the redacted form unless the link has no query or fragment.
#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct SensitiveUrl(String);

impl SensitiveUrl {
    /// The full link, for the engine to fetch. Never log it.
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// The link without user info, query or fragment, for display.
    pub fn display(&self) -> String {
        let text = self.0.as_str();
        let (before, hidden) = match text.find(['?', '#']) {
            Some(index) => (&text[..index], true),
            None => (text, false),
        };
        let shown = match before.split_once("://") {
            Some((scheme, rest)) => {
                let authority_end = rest.find('/').unwrap_or(rest.len());
                match rest[..authority_end].rfind('@') {
                    Some(at) => format!("{scheme}://…@{}", &rest[at + 1..]),
                    None => before.to_owned(),
                }
            }
            None => before.to_owned(),
        };
        if hidden {
            format!("{shown}?…")
        } else {
            shown
        }
    }
}

impl TryFrom<String> for SensitiveUrl {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value.is_empty() || value.len() > MAX_URL_LENGTH {
            return Err(format!("a link must be 1 to {MAX_URL_LENGTH} bytes"));
        }
        if value.chars().any(char::is_control) {
            return Err("a link cannot contain control characters".into());
        }
        Ok(Self(value))
    }
}

impl From<SensitiveUrl> for String {
    fn from(value: SensitiveUrl) -> Self {
        value.0
    }
}

impl fmt::Debug for SensitiveUrl {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "SensitiveUrl({:?})", self.display())
    }
}

impl JsonSchema for SensitiveUrl {
    fn schema_name() -> Cow<'static, str> {
        "SensitiveUrl".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "minLength": 1,
            "maxLength": MAX_URL_LENGTH,
            "description": "A link as given, possibly with secrets in it. Never displayed or logged as is."
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum JobKind {
    File,
    Media,
    Torrent,
    #[serde(other)]
    Unknown,
}

/// The contract's job states (§6). A scheduled job is `queued` with a
/// `not_before`, not a separate state.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Queued,
    Probing,
    WaitingForSelection,
    Ready,
    Running,
    Pausing,
    Paused,
    WaitingForSource,
    Verifying,
    Publishing,
    Completed,
    Cancelling,
    Cancelled,
    Failed,
    /// Asked for by an agent outside its policy; waits for the person to
    /// approve or deny it (contract D1).
    AwaitingApproval,
    /// A state this client does not know, from a newer engine.
    #[serde(other)]
    Unknown,
}

impl JobState {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Cancelled)
    }
}

/// Why a non-terminal job is waiting.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WaitingReason {
    /// A private link's query was not kept, so a refreshed link is needed.
    SourceExpired,
    /// A browser capture's protected context is gone.
    BrowserContextLost,
    /// Something already exists at the destination.
    DestinationConflict,
    /// A media format must be chosen.
    MediaSelection,
    /// The media helpers are missing.
    MediaToolsMissing,
    #[serde(other)]
    Unknown,
}

/// What a finished download's bytes are known to be (contract §3).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum IntegrityOutcome {
    /// Matches a trusted expected hash.
    VerifiedExpected,
    /// Consistent with a strong source validator; the publisher was not
    /// independently checked.
    ConsistentSource,
    /// A hash was recorded but there was nothing trusted to compare it with.
    DownloadedObserved,
    /// Did not match the required hash; never published as a success.
    VerificationFailed,
    NotApplicable,
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    Low,
    Medium,
    High,
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Probe,
    Receive,
    Verify,
    Process,
    Publish,
    #[serde(other)]
    Unknown,
}

/// Progress facts (contract §7). An unknown total is `null`, never zero, and
/// there is no remaining time without both a total and a rate.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Progress {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<Phase>,
    pub bytes_received: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes_checkpointed: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes_verified: Option<u64>,
    #[serde(default)]
    pub bytes_total: Option<u64>,
    #[serde(default)]
    pub rate_bytes_per_second: Option<u64>,
    #[serde(default)]
    pub eta_seconds: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<Confidence>,
}

/// One job as a client sees it. Every snapshot carries the revision and the
/// last durable event sequence it reflects, so a client can resume its event
/// stream exactly after it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct JobSnapshot {
    pub job_id: JobId,
    pub kind: JobKind,
    pub state: JobState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waiting_reason: Option<WaitingReason>,
    pub job_revision: u64,
    pub last_seq: u64,
    /// The link without user info, query or fragment.
    pub source_display: String,
    /// The local file path. An agent sees only jobs it created (contract D1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destination: Option<String>,
    pub progress: Progress,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub integrity: Option<IntegrityOutcome>,
    /// The SHA-256 the person supplied, lowercase hex.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_sha256: Option<String>,
    /// The SHA-256 of the bytes received. Records what arrived; it is not
    /// evidence of what the publisher intended.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_sha256: Option<String>,
    /// Staging bytes are still waiting to be removed.
    #[serde(default)]
    pub cleanup_pending: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ProtocolError>,
    /// Automatic retries already spent.
    #[serde(default)]
    pub attempt: u32,
    /// When an automatic retry is due.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_at: Option<Timestamp>,
    pub created_at: Timestamp,
    /// Do not start before this instant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub not_before: Option<Timestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<Timestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quality_label: Option<String>,
    /// Who created the job.
    #[serde(default)]
    pub principal: Principal,
    /// Present while the job is `awaiting_approval`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval: Option<ApprovalRequest>,
    /// Completed by copying verified bytes from this computer's cache rather
    /// than by a transfer, so it has no rate (FP-032).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub reused_from_cache: bool,
    /// Completed from a paired computer on the local network, whose
    /// fingerprint this is (FP-034); checked against the job's checksum.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_paired_device: Option<String>,
}

/// One byte range in flight: received into memory, not yet written.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Segment {
    pub start: u64,
    /// Inclusive.
    pub end: u64,
    pub received: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct JobDetails {
    pub job: JobSnapshot,
    pub segments: Vec<Segment>,
}

/// Queue figures counted from one reconciled moment.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct QueueStats {
    pub running: u64,
    pub queued: u64,
    pub scheduled: u64,
    pub paused: u64,
    pub completed: u64,
    pub failed: u64,
    pub active_bytes: u64,
    pub completed_bytes: u64,
    /// Sum of observed per-download rates, not a link-capacity measurement.
    pub combined_bytes_per_second: u64,
    pub max_active_downloads: u64,
    /// Downloads waiting for the person to approve them (FP-101), so a
    /// management surface such as the tray can say so without listing jobs.
    #[serde(default)]
    pub awaiting_approval: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MediaVariantKind {
    Video,
    Audio,
    #[serde(other)]
    Unknown,
}

/// One format a media page offers. Sizes are the helper's estimates until it
/// resolves them.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MediaVariant {
    pub id: String,
    pub label: String,
    pub kind: MediaVariantKind,
    pub extension: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fps: Option<u32>,
}

/// What a media page offers. `title` comes from the page and is untrusted
/// text.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MediaInspection {
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_seconds: Option<f64>,
    pub variants: Vec<MediaVariant>,
}

/// What a link leads to, as far as the engine can tell before downloading.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LinkKind {
    /// A file to save as it is.
    File,
    /// A page on a video or audio site: inspect it with `InspectMedia` and
    /// choose a quality.
    MediaPage,
    /// A web page (HTML), not a file. It may still hold video or audio.
    WebPage,
    #[serde(other)]
    Unknown,
}

/// A link looked at before downloading. Every field is the server's claim,
/// and `file_name` is untrusted text to be made safe before use.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct LinkInspection {
    pub kind: LinkKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_name: Option<String>,
    /// The media type, such as `application/zip`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
    /// The server accepts ranges, so an interrupted download can resume.
    #[serde(default)]
    pub resumable: bool,
    /// How the person's smart rules decide for this link.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rules: Option<RulesVerdict>,
}

/// A smart rule (FP-064). A link matches when it meets every condition the
/// rule sets; rules are tried in order and the first match decides.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Rule {
    /// Given by the engine; stable while the rule exists.
    pub id: u32,
    #[serde(flatten)]
    pub spec: RuleSpec,
}

/// A rule as the person writes it: at least one condition and one action.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RuleSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default)]
    pub when: RuleConditions,
    #[serde(default)]
    pub then: RuleActions,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RuleConditions {
    /// Host names, lower case; `example.com` also matches its subdomains.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub domains: Vec<String>,
    /// File name extensions, lower case and without the dot.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub file_types: Vec<String>,
    /// Inclusive bounds. A link whose size is not known does not match a
    /// rule with either.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_size_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_size_bytes: Option<u64>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RuleActions {
    /// The folder the download is saved in. For an agent it must still be
    /// inside the agent's grant, or the job waits for approval.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder: Option<String>,
    /// The quality to choose on a video or audio page: `best`, `audio`, or
    /// the tallest height to accept, such as `720p`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_quality: Option<String>,
    /// A file download must be given an expected SHA-256.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub require_checksum: bool,
    /// The most connections one download may open.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_connections: Option<u32>,
}

/// Which rule decides for a link, and why each rule did or did not match.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RulesVerdict {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matched: Option<Rule>,
    /// Every rule tried, in order, up to and including the match.
    pub checks: Vec<RuleCheck>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RuleCheck {
    pub rule_id: u32,
    pub matched: bool,
    /// One line per condition, for a person to read.
    pub reasons: Vec<String>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Theme {
    #[default]
    System,
    Light,
    Dark,
    /// Pure black ground and white lines (FP-095). A build that predates it
    /// reads this as an unknown theme and keeps its current one.
    HighContrast,
    #[serde(other)]
    Unknown,
}

/// How tightly the interface lays out rows and controls (FP-095).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Density {
    #[default]
    Comfortable,
    Compact,
    #[serde(other)]
    Unknown,
}

/// Engine settings. The engine clamps every value into range and reports
/// what it applied.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct EngineSettings {
    pub max_active_downloads: u64,
    #[serde(default)]
    pub default_destination_dir: Option<String>,
    pub auto_retry: bool,
    pub auto_retry_max_attempts: u32,
    pub auto_retry_base_delay_seconds: u64,
    pub close_to_tray: bool,
    pub power_mode: bool,
    #[serde(default)]
    pub media_tools_dir: Option<String>,
    pub confirm_remove_completed: bool,
    pub theme: Theme,
    pub onboarding_completed: bool,
    /// Start the engine when the person signs in to Windows, so schedules
    /// survive a restart. Off unless the person turns it on. Absent from an
    /// update means "leave it as it is", so a client that does not know the
    /// setting cannot turn it off by accident.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_engine_at_sign_in: Option<bool>,
    /// The most the content cache may hold. Absent from an update means
    /// "leave it as it is".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_quota_bytes: Option<u64>,
    /// Row and control density. Absent from an update means "leave it as it
    /// is".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub density: Option<Density>,
    /// The name shown for this engine on every client (contract D6).
    /// Absent from an update means "leave it as it is"; empty means the
    /// computer's name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance_name: Option<String>,
    /// Keep the engine running in the background: no idle stop, started at
    /// sign-in (which it turns on), and the computer kept awake while
    /// downloads run. Absent from an update means "leave it as it is".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hub_mode: Option<bool>,
}

/// A model or dataset repository resolved to one commit (FP-022). Each file
/// becomes an ordinary download pinned to that commit; see
/// [`RepositoryView::requests`].
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RepositoryView {
    /// `huggingface`.
    pub provider: String,
    /// `model`, `dataset` or `space`.
    pub kind: String,
    pub repo: String,
    /// The branch, tag or commit the link named.
    pub revision: String,
    /// The commit every file is pinned to.
    pub commit: String,
    pub files: Vec<RepositoryFile>,
    /// Paths that cannot be saved safely on Windows, left out.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skipped: Vec<String>,
    /// The sum of the sizes the provider states.
    pub total_bytes: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RepositoryFile {
    /// `/`-separated, inside the repository.
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    /// Stated by the provider for large files. The download is published
    /// only if it matches.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    pub url: String,
}

impl RepositoryView {
    /// The folder the files go in: the repository's name inside `folder`.
    pub fn folder_in(&self, folder: &str) -> String {
        let name = self.repo.rsplit('/').next().unwrap_or(&self.repo);
        format!("{}\\{name}", folder.trim_end_matches(['\\', '/']))
    }

    /// One file download per file, into [`Self::folder_in`] with the
    /// repository's own layout, each checked against its stated SHA-256.
    pub fn requests(&self, folder: &str) -> Vec<crate::command::JobRequest> {
        let root = self.folder_in(folder);
        self.files
            .iter()
            .filter_map(|file| {
                let url = SensitiveUrl::try_from(file.url.clone()).ok()?;
                Some(crate::command::JobRequest::File {
                    input: crate::command::JobInput::Url { url },
                    destination: crate::command::DestinationIntent {
                        path: format!("{root}\\{}", file.path.replace('/', "\\")),
                        conflict: crate::command::ConflictPolicy::Ask,
                    },
                    not_before: None,
                    expected_sha256: file.sha256.clone(),
                })
            })
            .collect()
    }
}

/// Paired devices and LAN sharing (FP-033).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct LanView {
    /// Off until the person turns it on.
    pub sharing: bool,
    /// Where paired devices reach this one, while sharing is on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub serving: Option<String>,
    /// Why sharing is on but nothing is being served.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub problem: Option<String>,
    /// This device's fingerprint, grouped for reading aloud.
    pub fingerprint: String,
    pub devices: Vec<PairedDevice>,
    /// The pairing this device is hosting, while there is one to show.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pairing: Option<PairingView>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PairedDevice {
    /// The device's public key, hex; what `Unpair` takes.
    pub key: String,
    pub fingerprint: String,
    pub label: String,
    /// Where it announced itself on the local network lately, if it is
    /// sharing. A hint only: the device is still authenticated by its key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
}

/// A pairing code this device shows, and what became of it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PairingView {
    pub state: PairingState,
    /// Present while the code can still be used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    /// What to type on the other device as its address.
    pub address: String,
    pub expires_at: Timestamp,
    /// The device that paired.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<PairedDevice>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub problem: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PairingState {
    Waiting,
    Paired,
    Failed,
    Expired,
    Cancelled,
    #[serde(other)]
    Unknown,
}

/// The content cache: verified files kept so a download whose checksum
/// matches completes without a transfer (FP-032).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CacheView {
    pub bytes: u64,
    pub entries: u64,
    pub quota_bytes: u64,
    pub min_quota_bytes: u64,
    pub max_quota_bytes: u64,
}

/// Settings as stored plus the limits the engine enforces.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SettingsView {
    pub settings: EngineSettings,
    /// The stored file was unusable and defaults were substituted.
    pub repaired: bool,
    pub max_active_limit: u64,
    pub max_retry_attempts: u32,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct EngineStatus {
    /// The Fetchpath build, such as `0.2.0`.
    pub engine_version: String,
    pub schema_version: u32,
    pub started_at: Timestamp,
    pub connected_clients: u32,
    pub active_jobs: u32,
    /// The latest queue-wide event cursor.
    pub queue_cursor: u64,
    /// Present when the queue was saved by a newer Fetchpath: it is shown
    /// unchanged and every change is refused with this error (FP-070).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue_read_only: Option<crate::error::ProtocolError>,
    /// Which engine this is (contract D6). Absent from an engine without an
    /// identity, such as an in-process test engine.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance: Option<InstanceInfo>,
}

/// An engine's stable identity and the name the person gave it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct InstanceInfo {
    pub id: crate::ids::InstanceId,
    pub name: String,
}
