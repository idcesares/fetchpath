//! Jobs, snapshots and the other values commands and events carry.

use crate::error::ProtocolError;
use crate::ids::{JobId, Timestamp};
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
    /// The local file path. Shown to the person's own clients; policy for
    /// other principals is the engine's (FP-054).
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

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Theme {
    #[default]
    System,
    Light,
    Dark,
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
    /// survive a restart. Off unless the person turns it on.
    #[serde(default)]
    pub start_engine_at_sign_in: bool,
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
}
