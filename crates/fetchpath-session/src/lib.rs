//! The download session: the queue, its persistence, scheduling, retry
//! policy, rate estimates, history, settings and media orchestration.
//!
//! Moved out of the desktop host unchanged (FP-049). It holds no UI or
//! transport types; the desktop calls it in-process.

pub use fetchpath_browser_inbox as browser_inbox;
mod durable;
pub mod engine;
pub mod lan;
pub mod policy;
pub mod rules;
pub mod settings;
mod wire;

use browser_inbox::BridgeStore;
use durable::{Durable, DurableEngine, RecordDurable, RemovedJob, Reported};
use fetchpath_core::{CancelResult, FileJob, FileJobState, RequestContext, normalize_sha256};
use fetchpath_media::{MediaInspection, MediaJob, MediaJobState, MediaTools};
use fetchpath_protocol::principal::{AgentName, AgentPolicy, ApprovalReason, Principal};
use fetchpath_torrent::{JobState as TorrentJobState, TorrentJob};
use serde::{Deserialize, Serialize};
use settings::Settings;
use std::collections::{BTreeMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf, Prefix};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;
use std::time::{SystemTime, UNIX_EPOCH};

const QUEUE_SCHEMA_VERSION: u32 = 2;
const ENGINE_SCHEMA_VERSION: u32 = 1;
const MAX_TORRENT_METADATA_BYTES: u64 = 4 * 1024 * 1024;
pub const DEFAULT_MAX_ACTIVE: usize = 3;
/// Upper bound for an inter-process address. Long enough for real signed links,
/// short enough that a malformed renderer message cannot force unbounded work.
const MAX_SOURCE_LENGTH: usize = 8_192;
/// Upper bound for a destination path. Windows long paths stop well below this.
pub const MAX_DESTINATION_LENGTH: usize = 4_096;

pub struct Session {
    inner: Mutex<QueueState>,
    /// Coalesced wakeups for commands and settled transfers. No queue or
    /// durable locks are taken by a worker's completion callback.
    wake: Arc<ReconcileWake>,
    state_path: Option<PathBuf>,
    settings_path: Option<PathBuf>,
    /// Guarded separately from the queue so reading a setting never waits on a
    /// transfer, and so a settings write cannot deadlock against the poll.
    settings: Mutex<Settings>,
    /// True when the stored settings file had to be repaired on load.
    settings_repaired: bool,
    browser_store: Option<BridgeStore>,
    browser_download_dir: Option<PathBuf>,
    media_tools: Mutex<Option<MediaTools>>,
    /// The ledger, the event log and the subscribers (FP-051). Locked after
    /// `inner`, never before it.
    durable: Mutex<Durable>,
    /// Metadata copies retired by a source change or removal. Delete only
    /// after the queue rename commits, including deferred Engine commands.
    retired_torrent_metadata: Mutex<Vec<PathBuf>>,
    /// Set when the engine stops: no job starts any more (FP-053).
    halted: std::sync::atomic::AtomicBool,
    /// Set when the queue was written by a newer Fetchpath (FP-070): the
    /// saved list is shown as it is, nothing starts, and nothing is written
    /// over it. Holds the explanation.
    read_only: Option<String>,
    /// What each configured agent may do without asking (contract D1).
    agents: Mutex<BTreeMap<AgentName, AgentPolicy>>,
    agents_path: Option<PathBuf>,
    /// The content cache, when the host gave one (FP-032). The person's and
    /// the browser's checksum-verified downloads complete from it and fill
    /// it; an agent's never touch it, so an agent cannot learn what the
    /// person has downloaded or obtain it by naming its checksum.
    cache_root: Mutex<Option<PathBuf>>,
    /// Paired devices and sharing, when the host gave a place for them.
    lan: std::sync::OnceLock<lan::Lan>,
    /// Sizes of links looked at lately, so a size rule decides the same way
    /// when the job is created as it did on the card.
    inspected_sizes: Mutex<std::collections::VecDeque<(String, u64)>>,
}

#[derive(Default)]
struct ReconcileWake {
    pending: Mutex<bool>,
    ready: Condvar,
}

impl ReconcileWake {
    fn notify(&self) {
        *self.pending.lock().expect("reconcile wake poisoned") = true;
        self.ready.notify_all();
    }

    fn wait(&self, timeout: Duration) -> bool {
        let pending = self.pending.lock().expect("reconcile wake poisoned");
        let (mut pending, _) = self
            .ready
            .wait_timeout_while(pending, timeout, |pending| !*pending)
            .expect("reconcile wake poisoned");
        std::mem::take(&mut *pending)
    }
}

/// How many inspected sizes are remembered.
const REMEMBERED_SIZES: usize = 64;

#[derive(Default)]
struct QueueState {
    records: Vec<QueueRecord>,
    /// Pages captured from the browser that are media rather than files. They
    /// open in Add download, where the quality is chosen, instead of being
    /// saved as a web page. Their captures stay pending in the browser inbox
    /// until a client takes them (FP-056), so an engine that leaves before
    /// any window opens loses none.
    link_reviews: Vec<LinkReview>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct LinkReview {
    capture_id: String,
    credential_ref: String,
    url: String,
}

#[derive(Clone)]
enum JobHandle {
    File(FileJob),
    Media(MediaJob),
    Torrent(TorrentJob),
}

impl JobHandle {
    fn start(&self, wake: Arc<ReconcileWake>) -> Result<(), &'static str> {
        match self {
            Self::File(job) => job.start_with_completion(move || wake.notify()),
            Self::Media(job) => job.start_with_completion(move || wake.notify()),
            Self::Torrent(job) => job.start_with_completion(move || wake.notify()),
        }
    }

    fn cancel(&self) -> &'static str {
        match self {
            Self::File(job) => match job.cancel() {
                CancelResult::Accepted => "accepted",
                CancelResult::TooLate => "too_late",
                CancelResult::AlreadyTerminal => "already_terminal",
            },
            Self::Media(job) => {
                if matches!(
                    job.snapshot().state,
                    MediaJobState::Completed | MediaJobState::Cancelled | MediaJobState::Failed
                ) {
                    "already_terminal"
                } else {
                    job.cancel();
                    "accepted"
                }
            }
            Self::Torrent(job) => job.cancel(),
        }
    }

    fn join(&self) {
        match self {
            Self::File(job) => job.join(),
            Self::Media(job) => job.join(),
            Self::Torrent(job) => job.join(),
        }
    }

    fn completed(&self) -> bool {
        match self {
            Self::File(job) => job.snapshot().state == FileJobState::Completed,
            Self::Media(job) => job.snapshot().state == MediaJobState::Completed,
            Self::Torrent(job) => job.snapshot().state == TorrentJobState::Completed,
        }
    }
}

/// A smoothed view of how fast one download is actually moving.
///
/// Progress is sampled whenever the interface asks for the queue, which is an
/// irregular interval, so the rate is computed from the elapsed time between
/// samples rather than assuming a fixed cadence. Samples closer together than
/// `MIN_INTERVAL_MS` are ignored: dividing a handful of bytes by a few
/// milliseconds produces a number that swings wildly and reads as noise.
#[derive(Clone, Copy, Default)]
struct RateEstimate {
    last_sample_ms: u64,
    last_bytes: u64,
    started: bool,
    smoothed_bytes_per_second: Option<f64>,
}

impl RateEstimate {
    const MIN_INTERVAL_MS: u64 = 400;
    /// Weight given to the newest sample. Low enough that one slow scheduling
    /// hiccup does not make the displayed rate jump.
    const SMOOTHING: f64 = 0.3;
    /// After this long with no progress the rate is no longer describing
    /// anything that is happening, so it is withdrawn rather than left frozen.
    const STALE_AFTER_MS: u64 = 5_000;
    const FIRST_ESTIMATE_BYTES: u64 = 64 * 1024;

    fn observe(&mut self, bytes: u64, now_ms: u64) {
        if !self.started || bytes < self.last_bytes {
            // First sample, or the transfer restarted from a lower offset.
            // There is no interval to measure yet, so record a baseline only.
            *self = Self {
                last_sample_ms: now_ms,
                last_bytes: bytes,
                started: true,
                smoothed_bytes_per_second: None,
            };
            return;
        }
        let elapsed_ms = now_ms.saturating_sub(self.last_sample_ms);
        if elapsed_ms < Self::MIN_INTERVAL_MS {
            return;
        }
        let delta = bytes - self.last_bytes;
        // The first estimate waits for the transfer proper: a transfer opens
        // with a one-byte range probe, and 1 byte over a second read as
        // "1 B/s, thousands of hours left" (FP-074). The baseline stays put,
        // so a genuinely slow link is averaged from its start.
        if self.smoothed_bytes_per_second.is_none()
            && delta < Self::FIRST_ESTIMATE_BYTES
            && elapsed_ms < Self::STALE_AFTER_MS
        {
            return;
        }
        if delta == 0 && elapsed_ms >= Self::STALE_AFTER_MS {
            self.smoothed_bytes_per_second = None;
            self.last_sample_ms = now_ms;
            return;
        }
        let instant = delta as f64 * 1_000.0 / elapsed_ms as f64;
        self.smoothed_bytes_per_second = Some(match self.smoothed_bytes_per_second {
            Some(previous) => Self::SMOOTHING * instant + (1.0 - Self::SMOOTHING) * previous,
            None => instant,
        });
        self.last_sample_ms = now_ms;
        self.last_bytes = bytes;
    }

    /// Clears the estimate when a download stops, so a paused or finished row
    /// never shows a speed it is not achieving.
    fn clear(&mut self) {
        *self = Self::default();
    }

    fn bytes_per_second(&self) -> Option<u64> {
        self.smoothed_bytes_per_second
            .filter(|rate| *rate >= 1.0)
            .map(|rate| rate as u64)
    }

    /// Remaining time, only when a total and a real rate are both known.
    fn eta_seconds(&self, received: u64, total: Option<u64>) -> Option<u64> {
        let total = total?;
        let rate = self.bytes_per_second()?;
        let remaining = total.checked_sub(received)?;
        (remaining > 0).then(|| remaining.div_ceil(rate))
    }
}

struct QueueRecord {
    id: String,
    live_url: Option<String>,
    live_context: RequestContext,
    credential_ref: Option<String>,
    restart_url: Option<String>,
    display_url: String,
    destination: PathBuf,
    not_before_ms: Option<u64>,
    created_at_ms: u64,
    finished_at_ms: Option<u64>,
    media_variant_id: Option<String>,
    media_quality: Option<String>,
    torrent_policy: Option<TorrentPolicy>,
    torrent_metadata_sha256: Option<String>,
    torrent_metadata_path: Option<PathBuf>,
    /// A torrent whose `destination` is still the root folder: the helper names
    /// the child folder from metadata and `destination` becomes that folder once
    /// it is published.
    torrent_auto: bool,
    job: Option<JobHandle>,
    /// Live only. Deliberately not persisted: a rate measured before a restart
    /// describes a transfer that is no longer running.
    rate: RateEstimate,
    /// Automatic retries already spent on this download.
    attempt: u32,
    /// When an automatic retry is due, for the row to show and reconcile to act on.
    retry_at_ms: Option<u64>,
    /// Revision, event sequence and last durable report (FP-051).
    durable: RecordDurable,
    /// Who created the job (contract D1).
    principal: Principal,
    /// A wait for the person's decision, or its refusal.
    approval: Option<Approval>,
    /// The person approved this job past its agent's size limit.
    size_approved: bool,
    view: JobSnapshot,
}

/// A job an agent asked for outside its policy (contract D1). While it is
/// pending nothing about the job starts; a denial ends it cancelled. A
/// request the agent withdrew keeps its reasons, so retrying it asks the
/// person again instead of slipping past them.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Approval {
    reasons: Vec<ApprovalReason>,
    #[serde(default, skip_serializing_if = "is_false")]
    denied: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    withdrawn: bool,
    /// When the wait began (FP-101). Absent from a request saved before
    /// expiry existed, which then counts from when the download was made.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    requested_at_ms: Option<u64>,
    /// Nobody decided within the expiry period (contract D6).
    #[serde(default, skip_serializing_if = "is_false")]
    expired: bool,
}

/// How long a request waits for the person before it expires (contract D6,
/// decision O3).
pub const APPROVAL_EXPIRY_MS: u64 = 7 * 24 * 60 * 60 * 1000;

impl QueueRecord {
    /// Records who asked for the job and holds it for approval when needed.
    fn apply(&mut self, origin: &Origin) {
        self.principal = origin.principal.clone();
        if !origin.approval.is_empty() {
            self.hold(origin.approval.clone());
        }
    }

    /// Puts the job in `awaiting_approval`. Whatever was prepared for it
    /// stays unstarted until the person decides.
    fn hold(&mut self, reasons: Vec<ApprovalReason>) {
        self.approval = Some(Approval {
            reasons,
            denied: false,
            withdrawn: false,
            requested_at_ms: Some(now_ms()),
            expired: false,
        });
        self.view.state = "awaiting_approval".into();
        self.view.error = None;
        self.view.action = None;
        self.view.retryable = false;
        self.finished_at_ms = None;
        self.view.finished_at_ms = None;
        sample_rate(self);
    }

    /// Waiting for the person to approve or deny it.
    fn awaiting_approval(&self) -> bool {
        self.approval
            .as_ref()
            .is_some_and(|approval| !approval.denied && !approval.withdrawn && !approval.expired)
    }

    /// Waiting for the person for longer than the expiry period at `now`.
    fn approval_expired_at(&self, now: u64) -> bool {
        self.awaiting_approval()
            && self.approval.as_ref().is_some_and(|approval| {
                let since = approval.requested_at_ms.unwrap_or(self.created_at_ms);
                now.saturating_sub(since) >= APPROVAL_EXPIRY_MS
            })
    }
}

fn is_false(value: &bool) -> bool {
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
}

/// Aggregate queue figures, for the statistics panel.
///
/// Every field is counted from the same reconciled snapshot, so the totals
/// agree with the rows on screen rather than describing a slightly different
/// moment.
#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueStats {
    running: usize,
    queued: usize,
    scheduled: usize,
    paused: usize,
    completed: usize,
    failed: usize,
    /// Bytes received by downloads that are running right now.
    active_bytes: u64,
    /// Bytes received by downloads that finished and are still in the list.
    completed_bytes: u64,
    /// Sum of the per-download rates. This is throughput actually observed, not
    /// a link-capacity measurement.
    combined_bytes_per_second: u64,
    max_active_downloads: usize,
    awaiting_approval: usize,
}

/// Where the content cache lives, and the bounds of its quota (FP-032).
pub const MIN_CACHE_QUOTA_BYTES: u64 = settings::MIN_CACHE_QUOTA_BYTES;
pub const MAX_CACHE_QUOTA_BYTES: u64 = settings::MAX_CACHE_QUOTA_BYTES;

/// One byte range in flight: received into memory, not yet written.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SegmentView {
    start: u64,
    /// Inclusive.
    end: u64,
    received: u64,
}

/// What the details window shows: the row itself plus its ranges in flight.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JobDetails {
    job: JobSnapshot,
    segments: Vec<SegmentView>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CancelResponse {
    outcome: &'static str,
    job: JobSnapshot,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PersistedQueue {
    schema_version: u32,
    records: Vec<PersistedRecord>,
    /// The engine file generation and event cursor this queue committed
    /// (FP-051). Absent from a 0.1.0 file, which therefore reads and writes
    /// back unchanged.
    #[serde(default, skip_serializing_if = "is_zero")]
    engine_generation: u64,
    #[serde(default, skip_serializing_if = "is_zero")]
    engine_cursor: u64,
}

fn is_zero(value: &u64) -> bool {
    *value == 0
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PersistedRecord {
    id: String,
    restart_url: Option<String>,
    #[serde(default)]
    credential_ref: Option<String>,
    display_url: String,
    destination: String,
    not_before_ms: Option<u64>,
    created_at_ms: u64,
    finished_at_ms: Option<u64>,
    #[serde(default)]
    media_variant_id: Option<String>,
    #[serde(default)]
    media_quality: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    torrent_policy: Option<TorrentPolicy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    torrent_metadata_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    torrent_auto: bool,
    #[serde(flatten, default)]
    durable: RecordDurable,
    /// Absent for the person's own jobs, so a 0.1.0 file writes back
    /// unchanged.
    #[serde(default, skip_serializing_if = "Principal::is_user")]
    principal: Principal,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    approval: Option<Approval>,
    #[serde(default, skip_serializing_if = "is_false")]
    size_approved: bool,
    view: JobSnapshot,
}

impl Session {
    #[cfg(test)]
    fn load(state_path: PathBuf, max_active: usize) -> io::Result<Self> {
        Self::load_with_browser(state_path, max_active, None)
    }

    pub fn load_with_browser(
        state_path: PathBuf,
        max_active: usize,
        browser_download_dir: Option<PathBuf>,
    ) -> io::Result<Self> {
        let (persisted, newer) = match load_persisted(&state_path)? {
            Loaded::Current(queue) => (Some(queue), None),
            Loaded::Newer { version, queue } => (None, Some((version, queue))),
            Loaded::Missing => (None, None),
        };
        let (generation, cursor) = persisted.as_ref().map_or((0, 0), |queue| {
            (queue.engine_generation, queue.engine_cursor)
        });
        let engine_path = state_path.with_file_name(ENGINE_FILE);
        let (engine, discarded, seen) = load_engine(&engine_path).committed(generation, cursor);
        let now = now_ms();
        let browser_store = state_path
            .parent()
            .map(|parent| BridgeStore::new(parent.to_path_buf()));
        let settings_path = state_path.with_file_name("settings-v1.json");
        let agents_path = state_path.with_file_name(policy::AGENTS_FILE);
        let loaded = settings::load(&settings_path);
        let mut stored = loaded.settings;
        // A caller-supplied concurrency (the tests, and the launch default) only
        // applies when the user has never chosen one themselves.
        if !settings_path.exists() {
            stored.max_active_downloads = max_active;
            stored.clamp();
        }
        let media_tools = discover_media_tools(stored.media_tools_dir.as_deref());
        let records: Vec<QueueRecord> = persisted
            .map(|queue| {
                queue
                    .records
                    .into_iter()
                    .map(|saved| {
                        QueueRecord::restore(
                            saved,
                            now,
                            browser_store.as_ref(),
                            media_tools.as_ref(),
                            &state_path,
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        let mut records = records;
        let read_only = newer.map(|(version, queue)| {
            // Shown as the newer build saved them: no download is prepared
            // and no browser secret is opened for them.
            records = queue
                .map(|queue| queue.records.into_iter().map(QueueRecord::shown).collect())
                .unwrap_or_default();
            newer_queue_explanation(version)
        });
        for record in &mut records {
            seen.advance(record);
        }
        Ok(Self {
            wake: Arc::default(),
            inner: Mutex::new(QueueState {
                records,
                ..QueueState::default()
            }),
            state_path: Some(state_path),
            settings_path: Some(settings_path),
            settings: Mutex::new(stored),
            settings_repaired: loaded.repaired,
            browser_store,
            browser_download_dir,
            media_tools: Mutex::new(media_tools),
            durable: Mutex::new(Durable {
                engine,
                // Rewrite the engine file once without what never committed.
                engine_changed: discarded,
                committed: generation,
                ..Durable::default()
            }),
            retired_torrent_metadata: Mutex::new(Vec::new()),
            halted: std::sync::atomic::AtomicBool::new(read_only.is_some()),
            read_only,
            agents: Mutex::new(policy::AgentsFile::load(&agents_path)),
            agents_path: Some(agents_path),
            cache_root: Mutex::default(),
            lan: std::sync::OnceLock::new(),
            inspected_sizes: Mutex::default(),
        })
    }

    #[cfg(test)]
    fn in_memory(max_active: usize) -> Self {
        let mut stored = Settings {
            max_active_downloads: max_active,
            ..Settings::default()
        };
        stored.clamp();
        Self {
            wake: Arc::default(),
            inner: Mutex::new(QueueState::default()),
            state_path: None,
            settings_path: None,
            settings: Mutex::new(stored),
            settings_repaired: false,
            browser_store: None,
            browser_download_dir: None,
            media_tools: Mutex::new(None),
            durable: Mutex::new(Durable::default()),
            retired_torrent_metadata: Mutex::new(Vec::new()),
            halted: std::sync::atomic::AtomicBool::new(false),
            read_only: None,
            agents: Mutex::new(BTreeMap::new()),
            agents_path: None,
            cache_root: Mutex::default(),
            lan: std::sync::OnceLock::new(),
            inspected_sizes: Mutex::default(),
        }
    }

    #[cfg(test)]
    fn in_memory_with_media(max_active: usize, media_tools: MediaTools) -> Self {
        let jobs = Self::in_memory(max_active);
        *jobs.media_tools.lock().expect("media tools poisoned") = Some(media_tools);
        jobs
    }

    pub fn settings(&self) -> Settings {
        self.settings.lock().expect("settings poisoned").clone()
    }

    /// Whether the engine is kept running in the background (FP-101).
    pub fn hub_mode(&self) -> bool {
        self.settings.lock().expect("settings poisoned").hub_mode
    }

    /// Downloads transferring now, as opposed to queued or scheduled.
    pub fn running_count(&self) -> usize {
        if self.read_only.is_some() {
            return 0;
        }
        // Read as it stands: reconciling belongs to the engine's own pass.
        let state = self.inner.lock().expect("desktop jobs poisoned");
        state
            .records
            .iter()
            .filter(|record| record.view.state == "running")
            .count()
    }

    /// The name clients show for this engine (contract D6).
    pub fn instance_name(&self) -> String {
        self.settings
            .lock()
            .expect("settings poisoned")
            .display_instance_name()
    }

    /// True when the stored settings file was unusable on load and defaults
    /// were substituted.
    pub fn settings_repaired(&self) -> bool {
        self.settings_repaired
    }

    /// Why the queue cannot be changed, when it was written by a newer
    /// Fetchpath (FP-070).
    pub fn read_only(&self) -> Option<&str> {
        self.read_only.as_deref()
    }

    /// Media pages sent from the browser, each handed out once. Only now is
    /// the capture marked done and its protected context, which a media page
    /// does not use, deleted. Both happen under the queue lock, so the next
    /// intake cannot offer the page again.
    pub fn take_link_reviews(&self) -> Vec<String> {
        let mut state = self.inner.lock().expect("desktop jobs poisoned");
        let reviews = std::mem::take(&mut state.link_reviews);
        let mut urls = Vec::with_capacity(reviews.len());
        for review in reviews {
            if let Some(store) = self.browser_store.as_ref() {
                // A receipt that cannot be written offers the page again
                // after a restart, which is the safe way round.
                let _ = store.mark_processed(&review.capture_id, "link-review");
                let _ = store.remove_secret(&review.credential_ref);
            }
            if !urls.contains(&review.url) {
                urls.push(review.url);
            }
        }
        urls
    }

    fn max_active(&self) -> usize {
        self.settings().max_active_downloads
    }

    fn media_tools(&self) -> Option<MediaTools> {
        self.media_tools
            .lock()
            .expect("media tools poisoned")
            .clone()
    }

    /// Keeps the content cache at `root`. Without this call nothing is cached.
    pub fn use_cache(&self, root: PathBuf) {
        *self.cache_root.lock().expect("cache poisoned") = Some(root);
    }

    /// Resolves a model or dataset repository link to one commit and its
    /// files (FP-022). Nothing is queued.
    pub fn inspect_repository(
        &self,
        link: &str,
    ) -> Result<fetchpath_protocol::model::RepositoryView, String> {
        let reference = fetchpath_providers::parse(link).ok_or_else(|| {
            "That is not a Hugging Face repository link, such as hf://owner/name or https://huggingface.co/owner/name.".to_string()
        })?;
        let listing =
            fetchpath_providers::resolve(&reference).map_err(|error| error.to_string())?;
        Ok(fetchpath_protocol::model::RepositoryView {
            provider: "huggingface".into(),
            kind: listing.kind.name().into(),
            total_bytes: listing.files.iter().filter_map(|file| file.size).sum(),
            files: listing
                .files
                .into_iter()
                .map(|file| fetchpath_protocol::model::RepositoryFile {
                    path: file.path,
                    size: file.size,
                    sha256: file.sha256,
                    url: file.url,
                })
                .collect(),
            repo: listing.repo,
            revision: listing.revision,
            commit: listing.commit,
            skipped: listing.skipped,
        })
    }

    /// Keeps paired devices and the sharing switch in `dir`, sharing from
    /// the cache at `cache_root`, and resumes sharing if it was left on.
    pub fn use_lan(&self, dir: PathBuf, cache_root: PathBuf) {
        if self.lan.set(lan::Lan::new(dir, cache_root)).is_ok() {
            self.lan().expect("just set").resume();
        }
    }

    pub fn lan(&self) -> Result<&lan::Lan, String> {
        self.lan
            .get()
            .ok_or_else(|| "Paired devices are not available in this Fetchpath.".to_string())
    }

    fn cache_config(settings: &Settings) -> fetchpath_core::fetchpath_cache::CacheConfig {
        let quota = settings.cache_quota_bytes;
        fetchpath_core::fetchpath_cache::CacheConfig::new(quota, quota)
    }

    fn open_cache(&self) -> Result<fetchpath_core::fetchpath_cache::ContentCache, String> {
        let root = self
            .cache_root
            .lock()
            .expect("cache poisoned")
            .clone()
            .ok_or_else(|| "This Fetchpath keeps no cache.".to_string())?;
        fetchpath_core::fetchpath_cache::ContentCache::open(
            &root,
            Self::cache_config(&self.settings()),
        )
        .map_err(|error| format!("The cache cannot be read: {error}"))
    }

    /// How much the cache holds, and its bounds.
    pub fn cache_status(&self) -> Result<fetchpath_protocol::model::CacheView, String> {
        let cache = self.open_cache()?;
        Ok(fetchpath_protocol::model::CacheView {
            bytes: cache.total_bytes(),
            entries: cache.entries().len() as u64,
            quota_bytes: cache.config().quota_bytes,
            min_quota_bytes: MIN_CACHE_QUOTA_BYTES,
            max_quota_bytes: MAX_CACHE_QUOTA_BYTES,
        })
    }

    /// Empties the cache. Saved downloads are separate files and stay.
    pub fn clear_cache(&self) -> Result<fetchpath_protocol::model::CacheView, String> {
        self.open_cache()?
            .clear()
            .map_err(|error| format!("The cache could not be cleared: {error}"))?;
        self.cache_status()
    }

    /// Applies a settings change, clamping it first and re-resolving anything
    /// the change affects.
    pub fn update_settings(&self, mut next: Settings) -> Result<Settings, String> {
        next.clamp();
        let tools_dir_changed = {
            let current = self.settings.lock().expect("settings poisoned");
            current.media_tools_dir != next.media_tools_dir
        };
        if tools_dir_changed {
            *self.media_tools.lock().expect("media tools poisoned") =
                discover_media_tools(next.media_tools_dir.as_deref());
        }
        let quota_lowered = next.cache_quota_bytes < self.settings().cache_quota_bytes;
        *self.settings.lock().expect("settings poisoned") = next.clone();
        if quota_lowered && let Ok(mut cache) = self.open_cache() {
            let _ = cache.trim();
        }
        if let Some(path) = self.settings_path.as_ref() {
            settings::save(path, &next).map_err(|error| {
                format!("Could not save settings at {}: {error}", path.display())
            })?;
        }
        // A higher concurrency takes effect immediately rather than at the next
        // poll, so raising the limit visibly starts the next waiting download.
        let mut state = self.inner.lock().expect("desktop jobs poisoned");
        self.reconcile_locked(&mut state);
        let _ = self.save_locked(&mut state);
        Ok(next)
    }

    pub fn enqueue(&self, drafts: Vec<JobDraft>) -> Result<Vec<JobSnapshot>, String> {
        self.enqueue_for(drafts, &Origin::default())
    }

    pub(crate) fn enqueue_for(
        &self,
        drafts: Vec<JobDraft>,
        origin: &Origin,
    ) -> Result<Vec<JobSnapshot>, String> {
        if drafts.is_empty() {
            return Err("Add at least one download address.".into());
        }
        if drafts.len() > 100 {
            return Err("A batch can contain at most 100 downloads.".into());
        }
        // A checksum describes exactly one file.
        if drafts.len() > 1
            && drafts
                .iter()
                .any(|draft| has_checksum(draft.checksum.as_deref()))
        {
            return Err(
                "A checksum describes one file. Add links with a checksum one at a time.".into(),
            );
        }
        // Every link is checked before any is queued, so a batch that fails
        // on one link queues none of them (finding F4).
        let mut destinations = HashSet::new();
        let mut batch_directory: Option<PathBuf> = None;
        let mut accepted = Vec::with_capacity(drafts.len());
        for draft in drafts {
            let url = validated_source(&draft.url)?;
            let destination = validated_destination(&draft.destination)?;
            let directory = destination
                .parent()
                .ok_or_else(|| "Choose a destination folder for this download.".to_string())?
                .to_path_buf();
            match batch_directory.as_ref() {
                None => batch_directory = Some(directory),
                Some(expected) if expected == &directory => {}
                Some(_) => {
                    return Err(
                        "Every download in one batch has to be saved in the same folder.".into(),
                    );
                }
            }
            if !destinations.insert(destination.clone()) {
                return Err(format!(
                    "The batch contains the destination {} more than once.",
                    destination.display()
                ));
            }
            let checksum = match draft.checksum.as_deref() {
                Some(text) => checked_checksum(text)?,
                None => None,
            };
            accepted.push((url, destination, draft.not_before_ms, checksum));
        }

        let mut state = self.inner.lock().expect("desktop jobs poisoned");
        self.reconcile_locked(&mut state);
        let mut created_ids = Vec::with_capacity(accepted.len());
        for (url, destination, not_before_ms, checksum) in accepted {
            let mut record = QueueRecord::new_checked(url, destination, not_before_ms, checksum);
            record.apply(origin);
            created_ids.push(record.id.clone());
            state.records.push(record);
        }
        self.reconcile_locked(&mut state);
        self.save_locked(&mut state)?;
        Ok(state
            .records
            .iter()
            .filter(|record| created_ids.contains(&record.id))
            .map(|record| record.view.clone())
            .collect())
    }

    pub fn inspect_media(&self, source: &str) -> Result<MediaInspection, String> {
        let tools = self
            .media_tools()
            .ok_or_else(|| TOOLS_MISSING.to_string())?;
        let source = validated_source(source)?;
        tools.inspect(&source).map_err(|error| error.to_string())
    }

    /// Whether a link is a file, a media page or a web page, and what its
    /// server says about it. Known media sites are recognized without a
    /// request; anything else is asked for its headers.
    pub fn inspect_link(
        &self,
        source: &str,
    ) -> Result<fetchpath_protocol::model::LinkInspection, String> {
        use fetchpath_protocol::model::{LinkInspection, LinkKind};
        let source = validated_source(source)?;
        if is_media_page(&source) {
            return Ok(LinkInspection {
                kind: LinkKind::MediaPage,
                file_name: None,
                content_type: None,
                size_bytes: None,
                resumable: false,
                rules: Some(self.decide_rules(&source, None, None)),
            });
        }
        let facts = fetchpath_core::inspect_link(&source)?;
        if let Some(size) = facts.size {
            let mut sizes = self.inspected_sizes.lock().expect("sizes poisoned");
            sizes.retain(|(url, _)| *url != source);
            if sizes.len() == REMEMBERED_SIZES {
                sizes.pop_front();
            }
            sizes.push_back((source.clone(), size));
        }
        let rules = self.decide_rules(&source, facts.file_name.as_deref(), facts.size);
        Ok(LinkInspection {
            kind: if facts.is_web_page() {
                LinkKind::WebPage
            } else {
                LinkKind::File
            },
            file_name: facts.file_name,
            content_type: facts.content_type,
            size_bytes: facts.size,
            resumable: facts.resumable,
            rules: Some(rules),
        })
    }

    /// Which rule decides for a link, and why.
    pub fn decide_rules(
        &self,
        url: &str,
        file_name: Option<&str>,
        size: Option<u64>,
    ) -> fetchpath_protocol::model::RulesVerdict {
        let rules = self
            .settings
            .lock()
            .expect("settings poisoned")
            .rules
            .clone();
        rules::decide(&rules, &rules::Facts::new(url, file_name, size))
    }

    /// The size an inspection found for this exact link, if one did lately.
    fn inspected_size(&self, url: &str) -> Option<u64> {
        self.inspected_sizes
            .lock()
            .expect("sizes poisoned")
            .iter()
            .find(|(seen, _)| seen == url)
            .map(|(_, size)| *size)
    }

    /// Applies the rules to a new job before it is queued, for every
    /// principal: a `destination` that is only a file name goes into the
    /// matching rule's folder, or the default folder; a rule may require a
    /// checksum of a file download. Returns the full destination.
    pub fn apply_rules(
        &self,
        url: &str,
        destination: &str,
        checksum: Option<&str>,
        media: bool,
    ) -> Result<String, String> {
        let path = Path::new(destination.trim());
        let bare = path.components().count() == 1
            && matches!(path.components().next(), Some(Component::Normal(_)));
        let name = path.file_name().and_then(|name| name.to_str());
        let verdict = self.decide_rules(url, name, self.inspected_size(url));
        let matched = verdict.matched.as_ref();
        if let Some(rule) = matched
            && rule.spec.then.require_checksum
            && !media
            && !has_checksum(checksum)
        {
            return Err(format!(
                "integrity.checksum_required: {} requires a SHA-256 for this download. \
                 Add the checksum and try again.",
                rules::label(rule)
            ));
        }
        if !bare {
            return Ok(destination.to_owned());
        }
        let folder = matched
            .and_then(|rule| rule.spec.then.folder.clone())
            .map(PathBuf::from)
            .or_else(|| self.default_folder())
            .ok_or_else(|| "Choose a full destination path, including its drive.".to_owned())?;
        Ok(folder.join(path).display().to_string())
    }

    /// The destination of a new torrent after the person's rules, for every
    /// principal. A full path stays as it is. A bare folder name goes into the
    /// matching rule's folder or the default folder, like a file name does. An
    /// empty one means the engine names the folder from the torrent: the
    /// returned path is then the root and the flag is set. A checksum rule does
    /// not apply, since a torrent verifies itself against its metadata.
    pub fn resolve_torrent_destination(
        &self,
        source: Option<&str>,
        destination: &str,
    ) -> Result<(String, bool), String> {
        let trimmed = destination.trim();
        let path = Path::new(trimmed);
        let bare = path.components().count() == 1
            && matches!(path.components().next(), Some(Component::Normal(_)));
        if !trimmed.is_empty() && !bare {
            return Ok((destination.to_owned(), false));
        }
        let verdict = self.decide_rules(source.unwrap_or(""), None, None);
        let root = verdict
            .matched
            .and_then(|rule| rule.spec.then.folder)
            .map(PathBuf::from)
            .or_else(|| self.default_folder())
            .ok_or_else(|| "Choose a folder for this torrent.".to_owned())?;
        if trimmed.is_empty() {
            Ok((root.display().to_string(), true))
        } else {
            Ok((root.join(path).display().to_string(), false))
        }
    }

    /// Where a download goes when nothing else says: the setting, or the
    /// Downloads folder this engine was started with.
    pub fn default_folder(&self) -> Option<PathBuf> {
        self.settings
            .lock()
            .expect("settings poisoned")
            .default_destination_dir
            .clone()
            .map(PathBuf::from)
            .or_else(|| self.browser_download_dir.clone())
    }

    pub fn rules(&self) -> Vec<fetchpath_protocol::model::Rule> {
        self.settings
            .lock()
            .expect("settings poisoned")
            .rules
            .clone()
    }

    /// Adds a rule last, or at `position` from 1, and saves it.
    pub fn add_rule(
        &self,
        spec: fetchpath_protocol::model::RuleSpec,
        position: Option<u32>,
    ) -> Result<Vec<fetchpath_protocol::model::Rule>, String> {
        let spec = rules::validate(spec)?;
        let mut next = self.settings();
        if next.rules.len() >= rules::MAX_RULES {
            return Err(format!("There can be at most {} rules.", rules::MAX_RULES));
        }
        let id = next.rules.iter().map(|rule| rule.id).max().unwrap_or(0) + 1;
        let at = position
            .map(|position| (position.max(1) as usize - 1).min(next.rules.len()))
            .unwrap_or(next.rules.len());
        next.rules
            .insert(at, fetchpath_protocol::model::Rule { id, spec });
        Ok(self.update_settings(next)?.rules)
    }

    pub fn remove_rule(&self, id: u32) -> Result<Vec<fetchpath_protocol::model::Rule>, String> {
        let mut next = self.settings();
        let before = next.rules.len();
        next.rules.retain(|rule| rule.id != id);
        if next.rules.len() == before {
            return Err(format!("There is no rule {id}."));
        }
        Ok(self.update_settings(next)?.rules)
    }

    pub fn enqueue_media(&self, draft: MediaDraft) -> Result<JobSnapshot, String> {
        self.enqueue_media_for(draft, &Origin::default())
    }

    pub(crate) fn enqueue_torrent_for(
        &self,
        draft: TorrentDraft,
        origin: &Origin,
    ) -> Result<JobSnapshot, String> {
        let source = validated_torrent_source(&draft.source)?;
        let destination = validated_torrent_destination(&draft.destination, draft.auto)?;
        if !draft.policy.discover_peers {
            return Err(
                "policy.discovery_off: a torrent cannot find peers without discovery.".into(),
            );
        }
        let mut record = QueueRecord::new_torrent(
            source,
            destination,
            draft.auto,
            draft.not_before_ms,
            draft.policy,
        );
        record.apply(origin);
        let id = record.id.clone();
        let mut state = self.inner.lock().expect("desktop jobs poisoned");
        self.reconcile_locked(&mut state);
        state.records.push(record);
        self.reconcile_locked(&mut state);
        self.save_locked(&mut state)?;
        Ok(find_record(&state, &id)?.view.clone())
    }

    pub(crate) fn enqueue_torrent_file_for(
        &self,
        source: &Path,
        destination: &str,
        auto: bool,
        not_before_ms: Option<u64>,
        policy: TorrentPolicy,
        origin: &Origin,
    ) -> Result<JobSnapshot, String> {
        if !policy.discover_peers {
            return Err(
                "policy.discovery_off: a torrent cannot find peers without discovery.".into(),
            );
        }
        if !source.is_absolute() {
            return Err("Choose a .torrent file by its full path.".into());
        }
        let state_path = self
            .state_path
            .as_ref()
            .ok_or("Local torrents need a saved engine queue.")?;
        let attributes =
            fs::symlink_metadata(source).map_err(|_| "That .torrent file could not be read.")?;
        if !attributes.file_type().is_file()
            || attributes.len() > MAX_TORRENT_METADATA_BYTES
            || !source
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.to_ascii_lowercase().ends_with(".torrent"))
        {
            return Err("Choose a regular .torrent file of at most 4 MiB.".into());
        }
        let bytes = fs::read(source).map_err(|_| "That .torrent file could not be read.")?;
        if bytes.len() as u64 > MAX_TORRENT_METADATA_BYTES {
            return Err("Choose a .torrent file of at most 4 MiB.".into());
        }
        use sha2::{Digest, Sha256};
        let hash = format!("{:x}", Sha256::digest(&bytes));
        let destination = validated_torrent_destination(destination, auto)?;
        let source_name = source.file_name().unwrap().to_string_lossy().into_owned();
        let synthetic_source = format!("local-torrent:{hash}");
        let mut record = QueueRecord::new_torrent(
            synthetic_source.clone(),
            destination,
            auto,
            not_before_ms,
            policy,
        );
        record.display_url = source_name.clone();
        record.view.source = source_name;
        record.restart_url = Some(synthetic_source.clone());
        let snapshot = torrent_metadata_path(state_path, &record.id)
            .ok_or("The torrent metadata path could not be made.")?;
        let mut state = self.inner.lock().expect("desktop jobs poisoned");
        self.reconcile_locked(&mut state);
        fs::create_dir_all(snapshot.parent().unwrap()).map_err(|error| error.to_string())?;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&snapshot)
            .map_err(|error| format!("Could not save torrent metadata: {error}"))?;
        if let Err(error) = file.write_all(&bytes).and_then(|_| file.sync_all()) {
            drop(file);
            let _ = fs::remove_file(&snapshot);
            return Err(format!("Could not save torrent metadata: {error}"));
        }
        drop(file);
        record.torrent_metadata_sha256 = Some(hash);
        record.torrent_metadata_path = Some(snapshot);
        if !record.destination_taken() {
            record.job = Some(torrent_job(&record, synthetic_source, policy));
        }
        record.apply(origin);
        let id = record.id.clone();
        state.records.push(record);
        self.reconcile_locked(&mut state);
        self.save_locked(&mut state)?;
        Ok(find_record(&state, &id)?.view.clone())
    }

    pub(crate) fn enqueue_media_for(
        &self,
        draft: MediaDraft,
        origin: &Origin,
    ) -> Result<JobSnapshot, String> {
        let tools = self
            .media_tools()
            .ok_or_else(|| TOOLS_MISSING.to_string())?;
        let url = validated_source(&draft.url)?;
        let destination = validated_destination(&draft.destination)?;
        if draft.variant_id.len() > 512 || draft.quality_label.len() > 512 {
            return Err(
                "That quality selection could not be read. Inspect the source again.".into(),
            );
        }
        let inspection = tools.inspect(&url).map_err(|error| error.to_string())?;
        let selected = inspection
            .variants
            .iter()
            .find(|variant| variant.id == draft.variant_id)
            .ok_or_else(|| {
                "That quality is no longer available. Inspect the source again.".to_string()
            })?;
        if selected.label != draft.quality_label {
            return Err("That quality changed. Inspect the source again.".into());
        }
        let mut record = QueueRecord::new_media(
            url,
            destination,
            draft.not_before_ms,
            draft.variant_id,
            draft.quality_label,
            tools,
        );
        record.apply(origin);
        let id = record.id.clone();
        let mut state = self.inner.lock().expect("desktop jobs poisoned");
        self.reconcile_locked(&mut state);
        state.records.push(record);
        self.reconcile_locked(&mut state);
        self.save_locked(&mut state)?;
        Ok(find_record(&state, &id)?.view.clone())
    }

    pub fn list(&self) -> Result<Vec<JobSnapshot>, String> {
        let mut state = self.inner.lock().expect("desktop jobs poisoned");
        let processed = self.ingest_browser_locked(&mut state)?;
        self.reconcile_locked(&mut state);
        self.save_locked(&mut state)?;
        let snapshots = state
            .records
            .iter()
            .rev()
            .map(|record| record.view.clone())
            .collect();
        drop(state);
        if let Some(store) = self.browser_store.as_ref() {
            for (capture_id, job_id) in processed {
                store
                    .mark_processed(&capture_id, &job_id)
                    .map_err(|error| format!("Browser capture was queued but its receipt could not be updated: {error}"))?;
            }
        }
        Ok(snapshots)
    }

    pub fn snapshot(&self, job_id: &str) -> Result<JobSnapshot, String> {
        self.list()?
            .into_iter()
            .find(|job| job.job_id == job_id)
            .ok_or_else(|| "This download is no longer available.".to_string())
    }

    pub fn details(&self, job_id: &str) -> Result<JobDetails, String> {
        let mut state = self.inner.lock().expect("desktop jobs poisoned");
        self.reconcile_locked(&mut state);
        let record = find_record(&state, job_id)?;
        // Only a running file download has ranges in flight; media reports none.
        let segments = match (&record.job, record.view.state.as_str()) {
            (Some(JobHandle::File(job)), "running") => job
                .segments()
                .into_iter()
                .map(|segment| SegmentView {
                    start: segment.start,
                    end: segment.end,
                    received: segment.received,
                })
                .collect(),
            _ => Vec::new(),
        };
        Ok(JobDetails {
            job: record.view.clone(),
            segments,
        })
    }

    pub fn cancel(&self, job_id: &str) -> Result<CancelResponse, String> {
        let mut state = self.inner.lock().expect("desktop jobs poisoned");
        self.reconcile_locked(&mut state);
        let record = find_record_mut(&mut state, job_id)?;
        // Cancelling a job that waits for approval ends it; the person no
        // longer has anything to decide.
        let awaiting = record.awaiting_approval();
        if awaiting {
            if let Some(approval) = record.approval.as_mut() {
                approval.withdrawn = true;
            }
            if record.job.is_none() {
                record.view.state = "cancelled".into();
                record.finished_at_ms = Some(now_ms());
                record.view.finished_at_ms = record.finished_at_ms;
            }
        }
        let outcome = if let Some(job) = record.job.as_ref() {
            match job.cancel() {
                // A size stop already cancelled it; this cancel is the one
                // that counts.
                "already_terminal" if awaiting => "accepted",
                outcome => outcome,
            }
        } else if awaiting {
            "accepted"
        } else {
            "already_terminal"
        };
        refresh_record(record);
        self.save_locked(&mut state)?;
        Ok(CancelResponse {
            outcome,
            job: find_record(&state, job_id)?.view.clone(),
        })
    }

    pub fn start_now(&self, job_id: &str) -> Result<JobSnapshot, String> {
        let mut state = self.inner.lock().expect("desktop jobs poisoned");
        let record = find_record_mut(&mut state, job_id)?;
        if record.view.state != "scheduled" && record.view.state != "queued" {
            return Err("Only queued or scheduled downloads can start now.".into());
        }
        record.not_before_ms = None;
        record.view.not_before_ms = None;
        record.view.state = "queued".into();
        self.reconcile_locked(&mut state);
        self.save_locked(&mut state)?;
        Ok(find_record(&state, job_id)?.view.clone())
    }

    /// Moves a queued or scheduled download to a new start time.
    pub fn reschedule(&self, job_id: &str, not_before_ms: u64) -> Result<JobSnapshot, String> {
        let mut state = self.inner.lock().expect("desktop jobs poisoned");
        let record = find_record_mut(&mut state, job_id)?;
        if record.view.state != "scheduled" && record.view.state != "queued" {
            return Err("Only queued or scheduled downloads can be rescheduled.".into());
        }
        record.not_before_ms = Some(not_before_ms);
        record.view.not_before_ms = Some(not_before_ms);
        record.view.state = if not_before_ms > now_ms() {
            "scheduled".into()
        } else {
            "queued".into()
        };
        self.reconcile_locked(&mut state);
        self.save_locked(&mut state)?;
        Ok(find_record(&state, job_id)?.view.clone())
    }

    pub fn retry(
        &self,
        job_id: &str,
        url: Option<String>,
        destination: Option<String>,
        checksum: Option<String>,
    ) -> Result<JobSnapshot, String> {
        self.retry_as(
            job_id,
            url,
            destination,
            checksum,
            &Principal::User,
            Vec::new(),
        )
    }

    /// Retries on behalf of `by`. Only the person may retry a download they
    /// declined; `hold` puts the retried job back in front of them.
    pub(crate) fn retry_as(
        &self,
        job_id: &str,
        url: Option<String>,
        destination: Option<String>,
        checksum: Option<String>,
        by: &Principal,
        hold: Vec<ApprovalReason>,
    ) -> Result<JobSnapshot, String> {
        let checksum = checksum.map(|text| checked_checksum(&text)).transpose()?;
        let mut state = self.inner.lock().expect("desktop jobs poisoned");
        let record = find_record_mut(&mut state, job_id)?;
        if record.awaiting_approval() {
            return Err("This download is waiting for the person to approve it.".into());
        }
        // Nothing about the approval changes until every check below has
        // passed, so a refused retry cannot shed a hold.
        let mut hold = hold;
        if let Some(approval) = record.approval.as_ref() {
            if approval.denied && !by.is_user() {
                return Err("The person declined this download.".into());
            }
            // An agent retrying what it withdrew, or what nobody decided on
            // in time, asks the person again.
            if (approval.withdrawn || approval.expired) && !by.is_user() {
                for reason in &approval.reasons {
                    if !hold.contains(reason) {
                        hold.push(*reason);
                    }
                }
            }
        }
        let url = url
            .map(|url| {
                if record.torrent_policy.is_some() {
                    validated_torrent_source(&url)
                } else {
                    validated_source(&url)
                }
            })
            .transpose()?;
        if url.is_some()
            && !by.is_user()
            && let Some(policy) = record.torrent_policy
        {
            if policy.discover_peers && !hold.contains(&ApprovalReason::PeerDiscovery) {
                hold.push(ApprovalReason::PeerDiscovery);
            }
            if policy.upload && !hold.contains(&ApprovalReason::PeerUpload) {
                hold.push(ApprovalReason::PeerUpload);
            }
        }
        let destination = destination
            .map(|destination| validated_destination(&destination))
            .transpose()?;
        if url.is_some() && !by.is_user() {
            // A size approval was for the link the person saw. Dropped even
            // if the retry fails later, which only tightens the limit.
            record.size_approved = false;
        }
        if !matches!(
            record.view.state.as_str(),
            "failed" | "cancelled" | "needs_source"
        ) {
            return Err(
                "Only failed, cancelled, or source-expired downloads can be retried.".into(),
            );
        }
        if let Some(job) = record.job.take() {
            job.cancel();
            job.join();
        }
        let mut retired_metadata = None;
        if let Some(url) = url {
            if record.torrent_metadata_sha256.take().is_some() {
                retired_metadata = record.torrent_metadata_path.take().or_else(|| {
                    self.state_path
                        .as_ref()
                        .and_then(|path| torrent_metadata_path(path, &record.id))
                });
            }
            record.display_url = display_url(&url);
            record.view.source = record.display_url.clone();
            record.restart_url = restartable_url(&url);
            record.live_url = Some(url);
            record.live_context = RequestContext::default();
            if let Some(credential_ref) = record.credential_ref.take()
                && let Some(store) = self.browser_store.as_ref()
            {
                let _ = store.remove_secret(&credential_ref);
            }
        }
        // An unchanged destination keeps an automatic torrent automatic.
        if let Some(destination) = destination
            && destination != record.destination
        {
            record.destination = destination;
            record.torrent_auto = false;
        }
        // An empty field clears the checksum; anything else replaces it.
        if let Some(checksum) = checksum {
            record.view.expected_sha256 = checksum;
        }
        let live_url = record.live_url.clone().ok_or_else(|| {
            "This source included private query values. Paste a refreshed link to continue."
                .to_string()
        })?;
        if record.destination_taken() {
            record.view.state = "failed".into();
            record.view.error = Some("A file already exists at this destination.".into());
            record.view.action = Some("choose_new_path".into());
            record.view.retryable = true;
        } else {
            record.job = Some(if let Some(policy) = record.torrent_policy {
                torrent_job(record, live_url, policy)
            } else if let Some(variant_id) = record.media_variant_id.clone() {
                let tools = self
                    .media_tools()
                    .ok_or_else(|| TOOLS_MISSING.to_string())?;
                JobHandle::Media(MediaJob::create(
                    live_url,
                    variant_id,
                    record.destination.clone(),
                    tools,
                ))
            } else {
                JobHandle::File(file_job(
                    live_url,
                    record.destination.clone(),
                    record.live_context.clone(),
                    record.view.expected_sha256.as_deref(),
                )?)
            });
            record.not_before_ms = None;
            record.finished_at_ms = None;
            record.view.state = "queued".into();
            record.view.bytes_received = 0;
            record.view.destination = Some(record.destination.display().to_string());
            record.view.observed_sha256 = None;
            record.view.cleanup_pending = false;
            record.view.error = None;
            record.view.action = None;
            record.view.retryable = false;
            record.view.not_before_ms = None;
            record.view.finished_at_ms = None;
        }
        // The retry went through: settle the approval.
        record.approval = None;
        if !hold.is_empty() {
            record.hold(hold);
        }
        if let Some(path) = retired_metadata {
            self.retired_torrent_metadata
                .lock()
                .expect("torrent cleanup poisoned")
                .push(path);
        }
        self.reconcile_locked(&mut state);
        self.save_locked(&mut state)?;
        Ok(find_record(&state, job_id)?.view.clone())
    }

    /// Stops a running download at its last checkpoint so it can continue
    /// later from that offset.
    ///
    /// Pausing races publication. Cancellation is refused once the engine has
    /// committed to publishing, and when that happens the download really did
    /// finish: this reports the completion instead of claiming a paused state
    /// for a file that is already on disk.
    pub fn pause(&self, job_id: &str) -> Result<JobSnapshot, String> {
        let handle = {
            let mut state = self.inner.lock().expect("desktop jobs poisoned");
            self.reconcile_locked(&mut state);
            let record = find_record_mut(&mut state, job_id)?;
            if record.view.kind != "file" {
                return Err(
                    "Only file downloads can be paused. Video and audio downloads have to be cancelled and started again."
                        .into(),
                );
            }
            match record.view.state.as_str() {
                // Not started yet: hold it out of the queue without disturbing
                // the prepared job, so resuming costs nothing.
                "queued" | "scheduled" => {
                    record.view.state = "paused".into();
                    record.view.error = None;
                    record.view.action = None;
                    record.view.retryable = false;
                    sample_rate(record);
                    let view = record.view.clone();
                    self.save_locked(&mut state)?;
                    return Ok(view);
                }
                "running" => record.job.clone(),
                "paused" => return Ok(record.view.clone()),
                other => {
                    return Err(format!("A download that is {other} cannot be paused."));
                }
            }
        };

        // Joining the worker must happen with the queue lock released; it waits
        // for a network read to unwind and would otherwise freeze every other
        // command, including the poll that draws the interface.
        let Some(handle) = handle else {
            return Err("This download is no longer running.".into());
        };
        handle.cancel();
        handle.join();

        let mut state = self.inner.lock().expect("desktop jobs poisoned");
        let record = find_record_mut(&mut state, job_id)?;
        refresh_record(record);
        if record.view.state == "cancelled" {
            // The checkpoint was retained, so the bytes already verified stay
            // on disk and the next start resumes from that offset.
            record.job = None;
            record.view.state = "paused".into();
            record.view.error = None;
            record.view.action = None;
            record.view.retryable = false;
            record.finished_at_ms = None;
            record.view.finished_at_ms = None;
            sample_rate(record);
        }
        let view = record.view.clone();
        self.save_locked(&mut state)?;
        Ok(view)
    }

    /// Returns a paused download to the queue, continuing from its checkpoint.
    pub fn resume(&self, job_id: &str) -> Result<JobSnapshot, String> {
        let mut state = self.inner.lock().expect("desktop jobs poisoned");
        let record = find_record_mut(&mut state, job_id)?;
        if record.view.state != "paused" {
            return Err("Only a paused download can be resumed.".into());
        }
        if record.job.is_none() {
            let url = record.live_url.clone().ok_or_else(|| {
                "This source included private query values. Paste a refreshed link to continue."
                    .to_string()
            })?;
            // A recoverable job validates the retained checkpoint against the
            // source before reusing a single byte of it, so a source that
            // changed while paused restarts instead of splicing.
            record.job = Some(JobHandle::File(file_job(
                url,
                record.destination.clone(),
                record.live_context.clone(),
                record.view.expected_sha256.as_deref(),
            )?));
        }
        record.view.state = "queued".into();
        record.view.error = None;
        record.view.action = None;
        record.view.retryable = false;
        record.retry_at_ms = None;
        self.reconcile_locked(&mut state);
        self.save_locked(&mut state)?;
        Ok(find_record(&state, job_id)?.view.clone())
    }

    pub fn remove(&self, job_id: &str) -> Result<(), String> {
        let mut state = self.inner.lock().expect("desktop jobs poisoned");
        let index = state
            .records
            .iter()
            .position(|record| record.id == job_id)
            .ok_or_else(|| "This download is no longer available.".to_string())?;
        let record = state.records.remove(index);
        if record.durable.reported.is_some() {
            self.durable
                .lock()
                .expect("engine state poisoned")
                .removed
                .push(RemovedJob {
                    job_id: record.id.clone(),
                    job_revision: record.durable.job_revision,
                    last_seq: record.durable.last_seq,
                });
        }
        if let Some(credential_ref) = record.credential_ref.as_deref()
            && let Some(store) = self.browser_store.as_ref()
        {
            let _ = store.remove_secret(credential_ref);
        }
        if let Some(job) = record.job {
            job.cancel();
            job.join();
        }
        if record.torrent_metadata_sha256.is_some()
            && let Some(path) = self
                .state_path
                .as_ref()
                .and_then(|state_path| torrent_metadata_path(state_path, &record.id))
        {
            self.retired_torrent_metadata
                .lock()
                .expect("torrent cleanup poisoned")
                .push(path);
        }
        self.save_locked(&mut state)?;
        Ok(())
    }

    /// Lets a job that waits for approval run (contract D1). A size stop is
    /// joined first, so its checkpoint is quiet before anything reuses it,
    /// and the job then continues from that checkpoint.
    pub fn approve(&self, job_id: &str) -> Result<JobSnapshot, String> {
        let stopped = {
            let mut state = self.inner.lock().expect("desktop jobs poisoned");
            let record = find_record_mut(&mut state, job_id)?;
            if !record.awaiting_approval() {
                return Err("This download is not waiting for approval.".into());
            }
            record.job.take()
        };
        // Joining waits for a network read to unwind; never under the lock.
        let finished = self.join_stopped(stopped);

        let mut state = self.inner.lock().expect("desktop jobs poisoned");
        let media_tools = self.media_tools();
        let record = find_record_mut(&mut state, job_id)?;
        if !record.awaiting_approval() {
            return Err("This download is not waiting for approval.".into());
        }
        if let Some(job) = finished {
            // The size stop came after publication: the download finished.
            record.job = Some(job);
            refresh_record(record);
            let view = record.view.clone();
            self.save_locked(&mut state)?;
            return Ok(view);
        }
        let reasons = record
            .approval
            .take()
            .map(|approval| approval.reasons)
            .unwrap_or_default();
        if reasons.contains(&ApprovalReason::SizeLimit) {
            record.size_approved = true;
        }
        record.finished_at_ms = None;
        record.view.finished_at_ms = None;
        let prepared = match record.live_url.clone() {
            None => Err((
                "needs_source",
                "Paste a refreshed link because private query values were not saved.",
                "edit_link",
            )),
            Some(_) if record.destination_taken() => Err((
                "failed",
                "A file already exists at this destination.",
                "choose_new_path",
            )),
            Some(url) if record.torrent_policy.is_some() => Ok(torrent_job(
                record,
                url,
                record.torrent_policy.expect("checked above"),
            )),
            Some(url) => match record.media_variant_id.clone() {
                Some(variant_id) => media_tools
                    .map(|tools| {
                        JobHandle::Media(MediaJob::create(
                            url,
                            variant_id,
                            record.destination.clone(),
                            tools,
                        ))
                    })
                    .ok_or((
                        "failed",
                        "Media tools are unavailable. Configure them to retry this download.",
                        "configure_media_tools",
                    )),
                // A recoverable job validates any retained checkpoint against
                // the source before reusing a byte of it.
                None => file_job(
                    url,
                    record.destination.clone(),
                    record.live_context.clone(),
                    record.view.expected_sha256.as_deref(),
                )
                .map(JobHandle::File)
                .map_err(|_| ("failed", UNREADABLE_CHECKSUM, "check_checksum")),
            },
        };
        match prepared {
            Ok(job) => {
                record.job = Some(job);
                record.view.state = if record.not_before_ms.is_some_and(|due| due > now_ms()) {
                    "scheduled".into()
                } else {
                    "queued".into()
                };
                record.view.error = None;
                record.view.action = None;
                record.view.retryable = false;
            }
            Err((state_name, error, action)) => {
                record.view.state = state_name.into();
                record.view.error = Some(error.into());
                record.view.action = Some(action.into());
                record.view.retryable = true;
                if state_name == "failed" {
                    record.finished_at_ms = Some(now_ms());
                    record.view.finished_at_ms = record.finished_at_ms;
                }
            }
        }
        self.reconcile_locked(&mut state);
        self.save_locked(&mut state)?;
        Ok(find_record(&state, job_id)?.view.clone())
    }

    /// Refuses a job that waits for approval. It ends cancelled, and its
    /// agent is told the person declined it.
    pub fn deny(&self, job_id: &str) -> Result<JobSnapshot, String> {
        self.decline(job_id, false)
    }

    /// Ends every request nobody decided on within the expiry period
    /// (contract D6), as a denial does. Run by the engine's own pass.
    pub fn expire_approvals(&self) {
        if self.read_only.is_some() {
            return;
        }
        let now = now_ms();
        let expired: Vec<String> = {
            let state = self.inner.lock().expect("desktop jobs poisoned");
            state
                .records
                .iter()
                .filter(|record| record.approval_expired_at(now))
                .map(|record| record.id.clone())
                .collect()
        };
        for job_id in expired {
            let _ = self.decline(&job_id, true);
        }
    }

    /// Ends a request: the person declined it, or nobody decided in time.
    fn decline(&self, job_id: &str, expired: bool) -> Result<JobSnapshot, String> {
        let stopped = {
            let mut state = self.inner.lock().expect("desktop jobs poisoned");
            let record = find_record_mut(&mut state, job_id)?;
            if !record.awaiting_approval() {
                return Err("This download is not waiting for approval.".into());
            }
            record.job.take()
        };
        let finished = self.join_stopped(stopped);
        let mut state = self.inner.lock().expect("desktop jobs poisoned");
        let record = find_record_mut(&mut state, job_id)?;
        if !record.awaiting_approval() {
            return Err("This download is not waiting for approval.".into());
        }
        if let Some(job) = finished {
            // Too late to decline: the size stop came after publication.
            record.job = Some(job);
            refresh_record(record);
            let view = record.view.clone();
            self.save_locked(&mut state)?;
            return Ok(view);
        }
        {
            if let Some(approval) = record.approval.as_mut() {
                if expired {
                    approval.expired = true;
                } else {
                    approval.denied = true;
                }
            }
            let now = now_ms();
            record.view.state = "cancelled".into();
            record.view.error = Some(if expired {
                "Nobody approved this download within 7 days.".into()
            } else {
                "The person declined this download.".into()
            });
            record.view.action = None;
            record.view.retryable = false;
            record.finished_at_ms = Some(now);
            record.view.finished_at_ms = Some(now);
            sample_rate(record);
        }
        let view = record.view.clone();
        self.save_locked(&mut state)?;
        Ok(view)
    }

    /// Cancels and joins a job taken from a record, returning it only when
    /// it had already published, so the caller reports that completion.
    fn join_stopped(&self, stopped: Option<JobHandle>) -> Option<JobHandle> {
        let job = stopped?;
        job.cancel();
        job.join();
        job.completed().then_some(job)
    }

    /// Where a job saves, for re-checking an agent's grants.
    pub(crate) fn destination_of(&self, job_id: &str) -> Option<PathBuf> {
        let state = self.inner.lock().expect("desktop jobs poisoned");
        find_record(&state, job_id)
            .ok()
            .map(QueueRecord::grant_path)
    }

    /// Stops a running agent download whose stated or received size passed
    /// its agent's limit and holds it for approval. The stop is requested
    /// here and joined on approval, so the queue lock never waits on it.
    fn stop_oversize_agent_jobs(&self, state: &mut QueueState) {
        let agents = self.agents.lock().expect("agents poisoned").clone();
        for record in &mut state.records {
            let Principal::Agent(agent) = &record.principal else {
                continue;
            };
            // A media download stopped by its byte cap has failed in the
            // adapter; for the person it is a size stop like any other.
            if record.approval.is_none()
                && record.view.state == "failed"
                && record
                    .view
                    .error
                    .as_deref()
                    .is_some_and(|error| error.starts_with("size_limit"))
            {
                record.retry_at_ms = None;
                record.hold(vec![ApprovalReason::SizeLimit]);
                continue;
            }
            if record.size_approved || record.approval.is_some() || record.view.state != "running" {
                continue;
            }
            let limit = agents
                .get(agent)
                .map_or(AgentPolicy::DEFAULT_MAX_BYTES, |policy| policy.max_bytes);
            let over = record.view.total_bytes.is_some_and(|total| total > limit)
                || record.view.bytes_received > limit;
            if !over {
                continue;
            }
            if let Some(job) = record.job.as_ref() {
                job.cancel();
            }
            record.hold(vec![ApprovalReason::SizeLimit]);
        }
    }

    /// Jobs of `principal` waiting for the person.
    pub(crate) fn pending_approvals(&self, principal: &Principal) -> usize {
        let state = self.inner.lock().expect("desktop jobs poisoned");
        state
            .records
            .iter()
            .filter(|record| &record.principal == principal && record.awaiting_approval())
            .count()
    }

    /// Who created a job, if it exists.
    pub(crate) fn principal_of(&self, job_id: &str) -> Option<Principal> {
        let state = self.inner.lock().expect("desktop jobs poisoned");
        find_record(&state, job_id)
            .ok()
            .map(|record| record.principal.clone())
    }

    /// An agent's policy, or the default for an agent the person has not
    /// configured.
    pub fn agent_policy(&self, agent: &AgentName) -> AgentPolicy {
        self.agents
            .lock()
            .expect("agents poisoned")
            .get(agent)
            .cloned()
            .unwrap_or_default()
    }

    pub fn agent_policies(&self) -> Vec<fetchpath_protocol::principal::AgentAccess> {
        self.agents
            .lock()
            .expect("agents poisoned")
            .iter()
            .map(
                |(agent, policy)| fetchpath_protocol::principal::AgentAccess {
                    agent: agent.clone(),
                    policy: policy.clone(),
                },
            )
            .collect()
    }

    /// Sets or removes one agent's access and saves every agent's. Its
    /// unfinished downloads that now fall outside its folders wait for the
    /// person again, a running one stopped at its checkpoint; so revoking an
    /// agent stops everything it started from continuing unapproved.
    pub fn set_agent_policy(
        &self,
        agent: AgentName,
        next: Option<AgentPolicy>,
    ) -> Result<(), String> {
        let next = next.map(policy::validated).transpose()?;
        let folders = next
            .as_ref()
            .map(|policy| policy.folders.clone())
            .unwrap_or_default();
        {
            let mut agents = self.agents.lock().expect("agents poisoned");
            let mut updated = agents.clone();
            match next {
                Some(policy) => updated.insert(agent.clone(), policy),
                None => updated.remove(&agent),
            };
            if let Some(path) = self.agents_path.as_ref() {
                write_json_atomically(path, &policy::AgentsFile::new(updated.clone())).map_err(
                    |error| format!("Could not save agent access at {}: {error}", path.display()),
                )?;
            }
            *agents = updated;
        }
        self.hold_outside_grants(&agent, &folders)
    }

    /// Holds `agent`'s unfinished downloads whose destination is outside
    /// `folders`. The stop of a running one is requested here and joined on
    /// approval, as a size stop is, so the queue lock never waits on it; a
    /// download that published first reports its completion then.
    fn hold_outside_grants(&self, agent: &AgentName, folders: &[String]) -> Result<(), String> {
        let principal = Principal::Agent(agent.clone());
        let mut state = self.inner.lock().expect("desktop jobs poisoned");
        let mut held = false;
        for record in &mut state.records {
            // A failed one counts while an automatic retry would start it.
            let unfinished = match record.view.state.as_str() {
                "queued" | "scheduled" | "running" | "paused" | "needs_source" => true,
                "failed" => record.retry_at_ms.is_some(),
                _ => false,
            };
            if record.principal != principal
                || record.approval.is_some()
                || !unfinished
                || policy::inside_grants(&record.grant_path(), folders)
            {
                continue;
            }
            if let Some(job) = record.job.as_ref() {
                job.cancel();
            }
            record.retry_at_ms = None;
            record.hold(vec![ApprovalReason::OutsideGrantedFolders]);
            held = true;
        }
        if held {
            self.save_locked(&mut state)?;
        }
        Ok(())
    }

    pub fn cancel_all_and_join(&self) {
        let jobs: Vec<(String, JobHandle)> = {
            let state = self.inner.lock().expect("desktop jobs poisoned");
            state
                .records
                .iter()
                .filter(|record| {
                    matches!(record.view.state.as_str(), "running" | "cancelling")
                        // A size stop may still be writing its checkpoint.
                        || (record.awaiting_approval() && record.job.is_some())
                })
                .filter_map(|record| {
                    record
                        .job
                        .as_ref()
                        .cloned()
                        .map(|job| (record.id.clone(), job))
                })
                .collect()
        };
        for (_, job) in &jobs {
            job.cancel();
        }
        for (_, job) in &jobs {
            job.join();
        }

        let mut state = self.inner.lock().expect("desktop jobs poisoned");
        for (id, _) in jobs {
            let Ok(record) = find_record_mut(&mut state, &id) else {
                continue;
            };
            if record.awaiting_approval() {
                // Quiet now; approval prepares a fresh job from the checkpoint.
                if !record.job.as_ref().is_some_and(JobHandle::completed) {
                    record.job = None;
                    continue;
                }
            }
            refresh_record(record);
            if record.view.state == "cancelled"
                && let Some(url) = record.live_url.clone()
            {
                record.job = if let Some(policy) = record.torrent_policy {
                    Some(torrent_job(record, url, policy))
                } else if let Some(variant_id) = record.media_variant_id.clone() {
                    self.media_tools().map(|tools| {
                        JobHandle::Media(MediaJob::create(
                            url,
                            variant_id,
                            record.destination.clone(),
                            tools,
                        ))
                    })
                } else {
                    file_job(
                        url,
                        record.destination.clone(),
                        record.live_context.clone(),
                        record.view.expected_sha256.as_deref(),
                    )
                    .ok()
                    .map(JobHandle::File)
                };
                record.view.state = if record.not_before_ms.is_some_and(|due| due > now_ms()) {
                    "scheduled".into()
                } else {
                    "queued".into()
                };
                record.view.error = Some("Ready to recover after Fetchpath restarts.".into());
                record.view.action = None;
                record.view.retryable = false;
                record.finished_at_ms = None;
                record.view.finished_at_ms = None;
            }
        }
        let _ = self.save_locked(&mut state);
    }

    fn reconcile_locked(&self, state: &mut QueueState) {
        if self.read_only.is_some() {
            // Nothing runs, is retried, or has its secrets removed.
            return;
        }
        let settings = self.settings();
        let max_active = settings.max_active_downloads;
        for record in &mut state.records {
            refresh_record(record);
            if record.view.state == "completed"
                && let Some(credential_ref) = record.credential_ref.take()
                && let Some(store) = self.browser_store.as_ref()
            {
                let _ = store.remove_secret(&credential_ref);
            }
        }
        self.stop_oversize_agent_jobs(state);
        if settings.auto_retry {
            self.schedule_automatic_retries(state, &settings);
        }
        let mut active = state
            .records
            .iter()
            .filter(|record| matches!(record.view.state.as_str(), "running" | "cancelling"))
            .count();
        let now = now_ms();
        let halted = self.halted.load(std::sync::atomic::Ordering::SeqCst);
        for record in &mut state.records {
            if active >= max_active || halted {
                break;
            }
            if record.view.state == "scheduled" && record.not_before_ms.is_none_or(|due| due <= now)
            {
                record.view.state = "queued".into();
            }
            if record.view.state != "queued" || record.not_before_ms.is_some_and(|due| due > now) {
                continue;
            }
            let Some(job) = record.job.as_ref() else {
                continue;
            };
            // An agent's video or audio download is capped at its size limit
            // inside the adapter, since a helper can write faster than the
            // engine samples (FP-067). The person's approval lifts it.
            if let (JobHandle::Media(media), Principal::Agent(agent)) = (job, &record.principal)
                && !record.size_approved
            {
                let limit = self
                    .agents
                    .lock()
                    .expect("agents poisoned")
                    .get(agent)
                    .map_or(AgentPolicy::DEFAULT_MAX_BYTES, |policy| policy.max_bytes);
                media.limit_bytes(limit);
            }
            if let (JobHandle::Torrent(torrent), Principal::Agent(agent)) = (job, &record.principal)
                && !record.size_approved
            {
                let limit = self
                    .agents
                    .lock()
                    .expect("agents poisoned")
                    .get(agent)
                    .map_or(AgentPolicy::DEFAULT_MAX_BYTES, |policy| policy.max_bytes);
                torrent.limit_bytes(limit);
            }
            if let JobHandle::File(file) = job {
                // A rule may cap the connections, decided as the job starts.
                let url = record.live_url.as_deref().unwrap_or(&record.display_url);
                let name = record
                    .destination
                    .file_name()
                    .and_then(|name| name.to_str());
                let size = record.view.total_bytes.or_else(|| self.inspected_size(url));
                let verdict = rules::decide(&settings.rules, &rules::Facts::new(url, name, size));
                if let Some(connections) = verdict
                    .matched
                    .and_then(|rule| rule.spec.then.max_connections)
                {
                    let _ = file.limit_connections(connections as usize);
                }
                // A download into a folder that does not exist yet, such as
                // a repository's subfolder, gets it as it starts: after any
                // approval, never while it waits for one.
                if let Some(parent) = record.destination.parent()
                    && !parent.as_os_str().is_empty()
                {
                    let _ = fs::create_dir_all(parent);
                }
                if !matches!(record.principal, Principal::Agent(_))
                    && let Some(root) = self.cache_root.lock().expect("cache poisoned").clone()
                {
                    let _ = file.use_cache(root, Self::cache_config(&settings));
                }
                // Paired devices are asked for the person's and the browser's
                // checksum-verified downloads only, like the cache.
                if !matches!(record.principal, Principal::Agent(_))
                    && let Some(lan) = self.lan.get()
                {
                    let peers = lan.peer_sources();
                    if !peers.is_empty() {
                        let _ = file.use_peers(peers);
                    }
                }
            }
            if let Err(code) = job.start(Arc::clone(&self.wake)) {
                record.view.state = "failed".into();
                record.view.error = Some(format!("Could not start this download ({code})."));
                record.view.action = Some("retry".into());
                record.view.retryable = true;
                record.finished_at_ms = Some(now);
                record.view.finished_at_ms = record.finished_at_ms;
                continue;
            }
            refresh_record(record);
            active += 1;
        }
        for record in &mut state.records {
            record.view.error_code = failure_code(&record.view);
        }
    }

    /// Re-queues transport failures on a widening backoff.
    ///
    /// Only failures the engine classified as plain transport trouble qualify.
    /// A destination conflict, an invalid link, an expired private source or a
    /// missing helper all need a person to decide something, and retrying them
    /// on a timer would bury that decision under repeated identical failures.
    fn schedule_automatic_retries(&self, state: &mut QueueState, settings: &Settings) {
        let now = now_ms();
        for record in &mut state.records {
            let due = record.retry_at_ms.is_some_and(|due| due <= now);
            if due && record.view.state == "scheduled" {
                record.retry_at_ms = None;
                continue;
            }
            if record.view.state != "failed"
                || record.attempt >= settings.auto_retry_max_attempts
                || record.retry_at_ms.is_some()
            {
                continue;
            }
            // `recovery_action` returns "retry" only for failures with no
            // specific user action attached. Of those, only recognized codes
            // are retried by the queue itself (finding F2).
            if record.view.action.as_deref() != Some("retry")
                || !failure_code(&record.view).is_some_and(|code| retried_automatically(&code))
            {
                continue;
            }
            let Some(url) = record.live_url.clone() else {
                continue;
            };
            if record.destination_taken() {
                continue;
            }
            record.attempt += 1;
            let delay = settings.retry_delay_seconds(record.attempt);
            let due_at = now.saturating_add(delay.saturating_mul(1_000));
            record.job = if let Some(policy) = record.torrent_policy {
                Some(torrent_job(record, url, policy))
            } else if let Some(variant_id) = record.media_variant_id.clone() {
                let Some(tools) = self.media_tools() else {
                    record.attempt -= 1;
                    continue;
                };
                Some(JobHandle::Media(MediaJob::create(
                    url,
                    variant_id,
                    record.destination.clone(),
                    tools,
                )))
            } else {
                file_job(
                    url,
                    record.destination.clone(),
                    record.live_context.clone(),
                    record.view.expected_sha256.as_deref(),
                )
                .ok()
                .map(JobHandle::File)
            };
            record.not_before_ms = Some(due_at);
            record.retry_at_ms = Some(due_at);
            record.finished_at_ms = None;
            record.view.state = "scheduled".into();
            record.view.not_before_ms = Some(due_at);
            record.view.finished_at_ms = None;
            record.view.attempt = record.attempt;
            record.view.action = None;
            record.view.retryable = false;
            record.view.error = Some(format!(
                "Retrying automatically (attempt {} of {}).",
                record.attempt, settings.auto_retry_max_attempts
            ));
        }
    }

    /// Stops starting jobs, for an engine that is shutting down. Running
    /// ones are then cancelled to their checkpoints by `cancel_all_and_join`
    /// and nothing takes their place.
    pub fn halt(&self) {
        self.halted.store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(lan) = self.lan.get() {
            lan.cancel_pairing();
            lan.stop_serving();
            lan.stop_listening();
        }
    }

    /// True while a job is queued, scheduled (including an automatic retry)
    /// or running: work that happens without a person. The engine stays up
    /// for it (FP-053).
    pub fn has_own_work(&self) -> bool {
        // A newer build's queue is only shown; nothing in it is waiting on us.
        if self.read_only.is_some() {
            return false;
        }
        let stats = self.stats();
        stats.running + stats.queued + stats.scheduled > 0
            || self.lan.get().is_some_and(lan::Lan::busy)
    }

    /// Aggregate figures for the statistics panel.
    pub fn stats(&self) -> QueueStats {
        let mut state = self.inner.lock().expect("desktop jobs poisoned");
        self.reconcile_locked(&mut state);
        let mut stats = QueueStats::default();
        for record in &state.records {
            let view = &record.view;
            match view.state.as_str() {
                "running" => stats.running += 1,
                "queued" => stats.queued += 1,
                "scheduled" => stats.scheduled += 1,
                "paused" => stats.paused += 1,
                "completed" => {
                    stats.completed += 1;
                    stats.completed_bytes =
                        stats.completed_bytes.saturating_add(view.bytes_received);
                }
                "failed" | "needs_source" => stats.failed += 1,
                "awaiting_approval" => stats.awaiting_approval += 1,
                _ => {}
            }
            if view.state == "running" {
                stats.active_bytes = stats.active_bytes.saturating_add(view.bytes_received);
                if let Some(rate) = view.bytes_per_second {
                    stats.combined_bytes_per_second =
                        stats.combined_bytes_per_second.saturating_add(rate);
                }
            }
        }
        stats.max_active_downloads = self.max_active();
        stats
    }

    /// Commits the queue: derives the durable events of every change since
    /// the last commit and writes records, events and ledger in one save.
    /// While a ledgered command runs it only marks the state dirty, and the
    /// command's own commit writes everything (see [`engine`]).
    fn save_locked(&self, state: &mut QueueState) -> Result<(), String> {
        if self.read_only.is_some() {
            // Nothing changed that is ours to save.
            return Ok(());
        }
        let mut durable = self.durable.lock().expect("engine state poisoned");
        if durable.defer {
            durable.dirty = true;
            return Ok(());
        }
        durable.derive_events(&mut state.records, now_ms());
        durable.prune_ledger(now_ms());
        self.write_locked(state, &mut durable)
    }

    /// Writes the queue and engine state, then hands the committed events to
    /// subscribers. Events are never shown before they are on disk.
    ///
    /// Ordering: the engine file (ledger and events, tagged with the new
    /// generation) is written first and the queue file, which names that
    /// generation, second. The queue file's rename is the commit point: until
    /// it happens, a load discards the new generation's entries.
    fn write_locked(&self, state: &QueueState, durable: &mut Durable) -> Result<(), String> {
        // The last line of defense: a newer build's queue is never written
        // over, whatever path got here.
        if let Some(reason) = &self.read_only {
            return Err(reason.clone());
        }
        if let Some(path) = self.state_path.as_ref() {
            if durable.engine_changed {
                let mut engine = durable.engine.clone();
                engine.schema_version = ENGINE_SCHEMA_VERSION;
                engine.generation = durable.pending_generation();
                let engine_path = path.with_file_name(ENGINE_FILE);
                write_json_atomically(&engine_path, &engine).map_err(|error| {
                    format!(
                        "Could not save the engine journal at {}: {error}",
                        engine_path.display()
                    )
                })?;
                durable.engine.generation = engine.generation;
                durable.engine_changed = false;
            }
            write_queue(
                path,
                state,
                durable.engine.generation,
                durable.engine.cursor,
            )
            .map_err(|error| {
                format!(
                    "Could not save download history at {}: {error}",
                    path.display()
                )
            })?;
            let mut retired = self
                .retired_torrent_metadata
                .lock()
                .expect("torrent cleanup poisoned");
            if !retired.is_empty() {
                // The previous queue remains as a recovery backup. Do not
                // remove metadata it still names; the next successful save
                // rotates that reference away.
                let backup_refs = match fs::read(path.with_extension("json.bak")) {
                    Ok(bytes) => {
                        serde_json::from_slice::<PersistedQueue>(&bytes)
                            .ok()
                            .map(|queue| {
                                queue
                                    .records
                                    .into_iter()
                                    .filter(|record| record.torrent_metadata_sha256.is_some())
                                    .map(|record| record.id)
                                    .collect::<HashSet<_>>()
                            })
                    }
                    Err(error) if error.kind() == io::ErrorKind::NotFound => Some(HashSet::new()),
                    Err(_) => None,
                };
                if let Some(backup_refs) = backup_refs {
                    retired.retain(|snapshot| {
                        let id = snapshot
                            .file_stem()
                            .and_then(|stem| stem.to_str())
                            .unwrap_or_default();
                        if backup_refs.contains(id)
                            || state.records.iter().any(|record| {
                                record.id == id && record.torrent_metadata_sha256.is_some()
                            })
                        {
                            return true;
                        }
                        match fs::remove_file(snapshot) {
                            Ok(()) => false,
                            Err(error) if error.kind() == io::ErrorKind::NotFound => false,
                            Err(_) => true,
                        }
                    });
                }
            }
        } else if durable.engine_changed {
            // Nothing to write to: everything in memory counts as committed.
            durable.engine.generation = durable.pending_generation();
            durable.engine_changed = false;
        }
        durable.committed = durable.engine.generation;
        durable.broadcast();
        Ok(())
    }

    fn ingest_browser_locked(
        &self,
        state: &mut QueueState,
    ) -> Result<Vec<(String, String)>, String> {
        let (Some(store), Some(download_dir), None) = (
            self.browser_store.as_ref(),
            self.browser_download_dir.as_ref(),
            self.read_only.as_ref(),
        ) else {
            // Read-only: captures stay in the inbox for the newer build.
            return Ok(Vec::new());
        };
        let pending = store
            .pending()
            .map_err(|error| format!("Could not read browser captures: {error}"))?;
        let mut processed = Vec::with_capacity(pending.len());
        for capture in pending {
            if let Some(existing) = state
                .records
                .iter()
                .find(|record| record.credential_ref.as_deref() == Some(&capture.credential_ref))
            {
                processed.push((capture.capture_id, existing.id.clone()));
                continue;
            }
            let secret = store
                .load_secret(&capture.credential_ref)
                .map_err(|error| format!("Protected browser capture is unavailable: {error}"))?;
            // The person's rules decide as for any new job (FP-075).
            let verdict = self.decide_rules(&secret.url, Some(&capture.suggested_filename), None);
            let rule = verdict.matched.as_ref();
            // A page on a media site saved as a file would be its HTML, and a
            // file a rule requires a checksum for cannot start without one.
            // Both go to Add download instead, where the video is found or
            // the checksum is asked for. Their cookies are not carried over.
            if is_media_page(&secret.url)
                || rule.is_some_and(|rule| rule.spec.then.require_checksum)
            {
                if !state
                    .link_reviews
                    .iter()
                    .any(|review| review.capture_id == capture.capture_id)
                {
                    state.link_reviews.push(LinkReview {
                        capture_id: capture.capture_id,
                        credential_ref: capture.credential_ref,
                        url: secret.url,
                    });
                }
                continue;
            }
            let context = RequestContext::new(secret.cookie_lines, secret.referer)
                .map_err(|code| format!("Browser request context was rejected ({code})."))?;
            let folder = rule
                .and_then(|rule| rule.spec.then.folder.clone())
                .map(PathBuf::from)
                .or_else(|| self.default_folder())
                .unwrap_or_else(|| download_dir.clone());
            let destination =
                unique_browser_destination(&folder, &capture.suggested_filename, &state.records);
            let record = QueueRecord::new_with_context(
                secret.url,
                destination,
                None,
                Some(capture.credential_ref),
                context,
                None,
            );
            let job_id = record.id.clone();
            state.records.push(record);
            processed.push((capture.capture_id, job_id));
        }
        Ok(processed)
    }
}

/// Why a video or audio request cannot run yet. The code lets every client
/// offer its own way to set the helpers up.
const TOOLS_MISSING: &str = "media.helper_unavailable: Video and audio need yt-dlp and ffmpeg, \
     which are not set up. Set them up in Settings, Video and audio, or run `fetchpath tools install`.";

/// Sites whose pages are video or audio rather than files. Kept in step with
/// `MEDIA_HOSTS` in the desktop interface, which uses it for pasted links.
const MEDIA_HOSTS: &[&str] = &[
    "youtube.com",
    "youtu.be",
    "vimeo.com",
    "dailymotion.com",
    "dai.ly",
    "twitch.tv",
    "tiktok.com",
    "instagram.com",
    "facebook.com",
    "fb.watch",
    "x.com",
    "twitter.com",
    "soundcloud.com",
    "bandcamp.com",
    "bilibili.com",
    "rumble.com",
    "odysee.com",
    "streamable.com",
    "ted.com",
    "reddit.com",
    "v.redd.it",
    "mixcloud.com",
    "nicovideo.jp",
    "archive.org",
];

/// A page on a known media site, not a direct file on it (an `.mp4` on
/// archive.org is still a file).
fn is_media_page(raw: &str) -> bool {
    let Ok(url) = url::Url::parse(raw) else {
        return false;
    };
    let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
    let host = ["www.", "m.", "music."]
        .iter()
        .find_map(|prefix| host.strip_prefix(prefix))
        .unwrap_or(&host)
        .to_owned();
    let on_media_site = MEDIA_HOSTS
        .iter()
        .any(|media| host == *media || host.ends_with(&format!(".{media}")));
    let last = url
        .path_segments()
        .and_then(|mut s| s.next_back())
        .unwrap_or("");
    let has_extension = last
        .rsplit_once('.')
        .is_some_and(|(stem, ext)| !stem.is_empty() && (2..=5).contains(&ext.len()));
    on_media_site && !has_extension
}

fn unique_browser_destination(
    directory: &Path,
    filename: &str,
    records: &[QueueRecord],
) -> PathBuf {
    let candidate = directory.join(filename);
    if !candidate.exists() && !records.iter().any(|record| record.destination == candidate) {
        return candidate;
    }
    let path = Path::new(filename);
    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("download");
    let extension = path.extension().and_then(|value| value.to_str());
    for index in 2..10_000 {
        let name = match extension {
            Some(extension) => format!("{stem} ({index}).{extension}"),
            None => format!("{stem} ({index})"),
        };
        let candidate = directory.join(name);
        if !candidate.exists() && !records.iter().any(|record| record.destination == candidate) {
            return candidate;
        }
    }
    directory.join(format!("{}-{}", uuid::Uuid::new_v4(), filename))
}

impl QueueRecord {
    #[cfg(test)]
    fn new(url: String, destination: PathBuf, not_before_ms: Option<u64>) -> Self {
        Self::new_checked(url, destination, not_before_ms, None)
    }

    /// A record for a pasted link. `expected_sha256` must already have been
    /// normalized by [`checked_checksum`].
    fn new_checked(
        url: String,
        destination: PathBuf,
        not_before_ms: Option<u64>,
        expected_sha256: Option<String>,
    ) -> Self {
        Self::new_with_context(
            url,
            destination,
            not_before_ms,
            None,
            RequestContext::default(),
            expected_sha256,
        )
    }

    fn new_with_context(
        url: String,
        destination: PathBuf,
        not_before_ms: Option<u64>,
        credential_ref: Option<String>,
        live_context: RequestContext,
        expected_sha256: Option<String>,
    ) -> Self {
        let now = now_ms();
        let id = uuid::Uuid::new_v4().to_string();
        let display = display_url(&url);
        let restart_url = restartable_url(&url);
        let conflict = destination.exists();
        let scheduled = not_before_ms.is_some_and(|due| due > now);
        // The checksum was normalized before it reached here, so this only
        // fails if that contract is broken; the record then has no job rather
        // than one that downloads unchecked.
        let job = (!conflict)
            .then(|| {
                file_job(
                    url.clone(),
                    destination.clone(),
                    live_context.clone(),
                    expected_sha256.as_deref(),
                )
                .ok()
                .map(JobHandle::File)
            })
            .flatten();
        let state = if conflict {
            "failed"
        } else if scheduled {
            "scheduled"
        } else {
            "queued"
        };
        Self {
            id: id.clone(),
            live_url: Some(url),
            live_context,
            credential_ref,
            restart_url,
            display_url: display.clone(),
            destination: destination.clone(),
            not_before_ms,
            created_at_ms: now,
            finished_at_ms: conflict.then_some(now),
            media_variant_id: None,
            media_quality: None,
            torrent_policy: None,
            torrent_metadata_sha256: None,
            torrent_metadata_path: None,
            torrent_auto: false,
            job,
            rate: RateEstimate::default(),
            attempt: 0,
            retry_at_ms: None,
            durable: RecordDurable::default(),
            principal: Principal::User,
            approval: None,
            size_approved: false,
            view: JobSnapshot {
                job_id: id,
                source: display,
                state: state.into(),
                bytes_received: 0,
                total_bytes: None,
                bytes_per_second: None,
                eta_seconds: None,
                attempt: 0,
                destination: Some(destination.display().to_string()),
                observed_sha256: None,
                expected_sha256,
                cleanup_pending: false,
                error: conflict.then(|| "A file already exists at this destination.".into()),
                error_code: None,
                action: conflict.then(|| "choose_new_path".into()),
                retryable: conflict,
                created_at_ms: now,
                not_before_ms,
                finished_at_ms: conflict.then_some(now),
                kind: "file".into(),
                quality_label: None,
                reused_from_cache: false,
                from_paired_device: None,
            },
        }
    }

    fn new_media(
        url: String,
        destination: PathBuf,
        not_before_ms: Option<u64>,
        variant_id: String,
        quality_label: String,
        tools: MediaTools,
    ) -> Self {
        let now = now_ms();
        let id = uuid::Uuid::new_v4().to_string();
        let display = display_url(&url);
        let restart_url = restartable_url(&url);
        let conflict = destination.exists();
        let scheduled = not_before_ms.is_some_and(|due| due > now);
        let job = (!conflict).then(|| {
            JobHandle::Media(MediaJob::create(
                url.clone(),
                variant_id.clone(),
                destination.clone(),
                tools,
            ))
        });
        let state = if conflict {
            "failed"
        } else if scheduled {
            "scheduled"
        } else {
            "queued"
        };
        Self {
            id: id.clone(),
            live_url: Some(url),
            live_context: RequestContext::default(),
            credential_ref: None,
            restart_url,
            display_url: display.clone(),
            destination: destination.clone(),
            not_before_ms,
            created_at_ms: now,
            finished_at_ms: conflict.then_some(now),
            media_variant_id: Some(variant_id),
            media_quality: Some(quality_label.clone()),
            torrent_policy: None,
            torrent_metadata_sha256: None,
            torrent_metadata_path: None,
            torrent_auto: false,
            job,
            rate: RateEstimate::default(),
            attempt: 0,
            retry_at_ms: None,
            durable: RecordDurable::default(),
            principal: Principal::User,
            approval: None,
            size_approved: false,
            view: JobSnapshot {
                job_id: id,
                source: display,
                state: state.into(),
                bytes_received: 0,
                total_bytes: None,
                bytes_per_second: None,
                eta_seconds: None,
                attempt: 0,
                destination: Some(destination.display().to_string()),
                observed_sha256: None,
                expected_sha256: None,
                cleanup_pending: false,
                error: conflict.then(|| "A file already exists at this destination.".into()),
                error_code: None,
                action: conflict.then(|| "choose_new_path".into()),
                retryable: conflict,
                created_at_ms: now,
                not_before_ms,
                finished_at_ms: conflict.then_some(now),
                kind: "media".into(),
                quality_label: Some(quality_label),
                reused_from_cache: false,
                from_paired_device: None,
            },
        }
    }

    fn new_torrent(
        source: String,
        destination: PathBuf,
        auto: bool,
        not_before_ms: Option<u64>,
        policy: TorrentPolicy,
    ) -> Self {
        let mut record =
            Self::new_checked(source.clone(), destination.clone(), not_before_ms, None);
        record.torrent_policy = Some(policy);
        record.torrent_auto = auto;
        record.view.kind = "torrent".into();
        if auto {
            // `new_checked` took the existing root for a conflict.
            let scheduled = not_before_ms.is_some_and(|due| due > now_ms());
            record.view.state = if scheduled { "scheduled" } else { "queued" }.into();
            record.view.error = None;
            record.view.action = None;
            record.view.retryable = false;
            record.view.finished_at_ms = None;
            record.finished_at_ms = None;
        }
        record.job = (!record.destination_taken()).then(|| torrent_job(&record, source, policy));
        record
    }

    /// The path an agent's folder grants are checked against: the destination,
    /// or for an automatic torrent a folder the engine will name inside the
    /// root, since a grant covers what is saved within it.
    fn grant_path(&self) -> PathBuf {
        if self.torrent_auto {
            self.destination.join("torrent")
        } else {
            self.destination.clone()
        }
    }

    /// An existing destination blocks a new download, except the root of an
    /// automatic torrent, which is meant to exist.
    fn destination_taken(&self) -> bool {
        !self.torrent_auto && self.destination.exists()
    }

    fn restore(
        saved: PersistedRecord,
        now: u64,
        browser_store: Option<&BridgeStore>,
        media_tools: Option<&MediaTools>,
        state_path: &Path,
    ) -> Self {
        let metadata = saved.torrent_metadata_sha256.as_ref().map(|hash| {
            let path = torrent_metadata_path(state_path, &saved.id)
                .ok_or("The saved torrent metadata could not be located.")?;
            verify_torrent_metadata(&path, hash)
                .map_err(|_| "The saved torrent metadata is missing or changed.")?;
            Ok::<PathBuf, &'static str>(path)
        });
        let metadata_error = metadata
            .as_ref()
            .and_then(|result| result.as_ref().err().copied());
        let metadata_path = metadata.and_then(Result::ok);
        let terminal = is_terminal(&saved.view.state);
        let browser_secret = saved
            .credential_ref
            .as_deref()
            .and_then(|credential_ref| browser_store?.load_secret(credential_ref).ok())
            .and_then(|secret| {
                let context = RequestContext::new(secret.cookie_lines, secret.referer).ok()?;
                Some((secret.url, context))
            });
        let live_url = saved
            .restart_url
            .clone()
            .or_else(|| browser_secret.as_ref().map(|(url, _)| url.clone()));
        let live_context = browser_secret
            .as_ref()
            .map(|(_, context)| context.clone())
            .unwrap_or_default();
        let private_source_needs_refresh =
            live_url.is_none() && matches!(saved.view.state.as_str(), "failed" | "cancelled");
        let missing_browser_secret = saved.credential_ref.is_some() && live_url.is_none();
        // A download the user paused stays paused. Restoring it as queued would
        // silently start a transfer the user deliberately stopped, which is the
        // one thing a pause has to be able to promise across a restart.
        let paused = saved.view.state == "paused";
        // A job waiting for the person's decision keeps waiting. Nothing is
        // prepared for it until it is approved (contract D1).
        // A request the agent withdrew, or that expired, ended: it is not
        // brought back for the person to approve.
        let awaiting_approval = saved
            .approval
            .as_ref()
            .is_some_and(|approval| !approval.denied && !approval.withdrawn && !approval.expired);
        let (job, state, error, action, retryable) = if let Some(problem) = metadata_error {
            (None, "failed".into(), Some(problem.into()), None, false)
        } else if awaiting_approval {
            (None, "awaiting_approval".into(), None, None, false)
        } else if paused && live_url.is_some() {
            (None, "paused".into(), None, None, false)
        } else if private_source_needs_refresh {
            (
                None,
                "needs_source".into(),
                Some(if missing_browser_secret {
                    "Send this download from the browser again because its protected context is unavailable.".into()
                } else {
                    "Paste a refreshed link because private query values were not saved.".into()
                }),
                Some(if missing_browser_secret {
                    "recapture".into()
                } else {
                    "edit_link".into()
                }),
                true,
            )
        } else if terminal {
            (
                None,
                saved.view.state.clone(),
                saved.view.error.clone(),
                saved.view.action.clone(),
                saved.view.retryable,
            )
        } else if let Some(url) = live_url.clone() {
            let state = if saved.not_before_ms.is_some_and(|due| due > now) {
                "scheduled"
            } else {
                "queued"
            };
            let job = if let Some(policy) = saved.torrent_policy {
                let mut request = match (&metadata_path, &saved.torrent_metadata_sha256) {
                    (Some(path), Some(hash)) => fetchpath_torrent::request_local(
                        url.clone(),
                        path.clone(),
                        hash.clone(),
                        PathBuf::from(&saved.destination),
                        saved.id.clone(),
                        policy.discover_peers,
                        policy.upload,
                    ),
                    _ => fetchpath_torrent::request(
                        url.clone(),
                        PathBuf::from(&saved.destination),
                        saved.id.clone(),
                        policy.discover_peers,
                        policy.upload,
                    ),
                };
                request.auto_name = saved.torrent_auto;
                Some(JobHandle::Torrent(TorrentJob::create(request)))
            } else if let Some(variant_id) = saved.media_variant_id.clone() {
                media_tools.map(|tools| {
                    JobHandle::Media(MediaJob::create(
                        url.clone(),
                        variant_id,
                        PathBuf::from(&saved.destination),
                        tools.clone(),
                    ))
                })
            } else {
                file_job(
                    url.clone(),
                    PathBuf::from(&saved.destination),
                    live_context.clone(),
                    saved.view.expected_sha256.as_deref(),
                )
                .ok()
                .map(JobHandle::File)
            };
            if saved.media_variant_id.is_none() && saved.torrent_policy.is_none() && job.is_none() {
                // Only an unreadable saved checksum gets here. Fail closed.
                (
                    None,
                    "failed".into(),
                    Some(UNREADABLE_CHECKSUM.into()),
                    Some("check_checksum".into()),
                    true,
                )
            } else if saved.media_variant_id.is_some() && job.is_none() {
                (
                    None,
                    "failed".into(),
                    Some(
                        "Media tools are unavailable. Configure them to retry this download."
                            .into(),
                    ),
                    Some("configure_media_tools".into()),
                    true,
                )
            } else {
                (
                    job,
                    state.into(),
                    Some("Recovered after Fetchpath restarted.".into()),
                    None,
                    false,
                )
            }
        } else {
            (
                None,
                "needs_source".into(),
                Some(if missing_browser_secret {
                    "Send this download from the browser again because its protected context is unavailable.".into()
                } else {
                    "Paste a refreshed link because private query values were not saved.".into()
                }),
                Some(if missing_browser_secret {
                    "recapture".into()
                } else {
                    "edit_link".into()
                }),
                true,
            )
        };
        let mut view = saved.view;
        view.state = state;
        view.error = error;
        view.action = action;
        view.retryable = retryable;
        view.error_code = failure_code(&view);
        let mut durable = saved.durable;
        if durable.reported.is_none() {
            // A record from before FP-051 has no events; it starts from what
            // it is now rather than reporting itself as newly created.
            durable.reported = Some(Reported {
                state: view.state.clone(),
                not_before_ms: saved.not_before_ms,
            });
        }
        Self {
            id: saved.id,
            live_url,
            live_context,
            credential_ref: saved.credential_ref,
            restart_url: saved.restart_url,
            display_url: saved.display_url,
            destination: PathBuf::from(saved.destination),
            not_before_ms: saved.not_before_ms,
            created_at_ms: saved.created_at_ms,
            finished_at_ms: saved.finished_at_ms,
            media_variant_id: saved.media_variant_id,
            media_quality: saved.media_quality,
            torrent_policy: saved.torrent_policy,
            torrent_metadata_sha256: saved.torrent_metadata_sha256,
            torrent_metadata_path: metadata_path,
            torrent_auto: saved.torrent_auto,
            job,
            rate: RateEstimate::default(),
            attempt: view.attempt,
            retry_at_ms: None,
            durable,
            principal: saved.principal,
            approval: saved.approval,
            size_approved: saved.size_approved,
            view,
        }
    }

    /// A record from a newer build's queue, only to be shown (FP-070): no
    /// download is prepared, no browser secret opened, and its saved state
    /// is kept as that build wrote it.
    fn shown(saved: PersistedRecord) -> Self {
        let mut view = saved.view;
        view.bytes_per_second = None;
        view.eta_seconds = None;
        view.error_code = failure_code(&view);
        let mut durable = saved.durable;
        if durable.reported.is_none() {
            durable.reported = Some(Reported {
                state: view.state.clone(),
                not_before_ms: saved.not_before_ms,
            });
        }
        Self {
            id: saved.id,
            live_url: None,
            live_context: RequestContext::default(),
            credential_ref: saved.credential_ref,
            restart_url: saved.restart_url,
            display_url: saved.display_url,
            destination: PathBuf::from(saved.destination),
            not_before_ms: saved.not_before_ms,
            created_at_ms: saved.created_at_ms,
            finished_at_ms: saved.finished_at_ms,
            media_variant_id: saved.media_variant_id,
            media_quality: saved.media_quality,
            torrent_policy: saved.torrent_policy,
            torrent_metadata_sha256: saved.torrent_metadata_sha256,
            torrent_metadata_path: None,
            torrent_auto: saved.torrent_auto,
            job: None,
            rate: RateEstimate::default(),
            attempt: view.attempt,
            retry_at_ms: None,
            durable,
            principal: saved.principal,
            approval: saved.approval,
            size_approved: saved.size_approved,
            view,
        }
    }
}

fn refresh_record(record: &mut QueueRecord) {
    // A job waiting for approval never takes its state from a prepared job,
    // which would report itself queued and be started. The one exception is
    // a size stop that lost the race with publication: that download really
    // finished, and saying otherwise would hide a file already on disk.
    if record.awaiting_approval() {
        if !record.job.as_ref().is_some_and(JobHandle::completed) {
            record.view.state = "awaiting_approval".into();
            sample_rate(record);
            return;
        }
        record.approval = None;
    }
    // A paused record may still hold a prepared job that never started. That
    // job reports itself as queued, and copying its state over the view would
    // put the record straight back in the queue for reconcile to start, which
    // is exactly the transfer the user stopped.
    if record.view.state == "paused" {
        sample_rate(record);
        return;
    }
    let Some(job) = record.job.as_ref() else {
        return;
    };
    match job {
        JobHandle::File(job) => {
            let snapshot = job.snapshot();
            if snapshot.state == FileJobState::Queued
                && record.not_before_ms.is_some_and(|due| due > now_ms())
            {
                record.view.state = "scheduled".into();
                return;
            }
            record.view.state = state_name(snapshot.state).into();
            record.view.bytes_received = snapshot.bytes_received;
            // The engine only reports a total once a source states one, and a
            // total it has already reported is kept rather than flickering off
            // between samples.
            record.view.total_bytes = snapshot.total_bytes.or(record.view.total_bytes);
            record.view.destination = snapshot
                .destination
                .as_ref()
                .map(|path| path.display().to_string());
            record.view.observed_sha256 = snapshot.observed_sha256;
            record.view.cleanup_pending = snapshot.staging_cleanup_pending.is_some();
            record.view.error = snapshot.error;
            record.view.reused_from_cache = snapshot.reused_from_cache;
            record.view.from_paired_device = snapshot
                .from_peer
                .map(|fingerprint| fetchpath_lan::Fingerprint(fingerprint).to_string());
        }
        JobHandle::Media(job) => {
            let snapshot = job.snapshot();
            if snapshot.state == MediaJobState::Queued
                && record.not_before_ms.is_some_and(|due| due > now_ms())
            {
                record.view.state = "scheduled".into();
                return;
            }
            record.view.state = media_state_name(snapshot.state).into();
            record.view.bytes_received = snapshot.bytes_received;
            record.view.destination = snapshot
                .destination
                .as_ref()
                .map(|path| path.display().to_string());
            record.view.observed_sha256 = snapshot.observed_sha256;
            record.view.cleanup_pending = false;
            record.view.error = snapshot.error;
            if snapshot.action.is_some() {
                record.view.action = snapshot.action;
            }
        }
        JobHandle::Torrent(job) => {
            let snapshot = job.snapshot();
            if snapshot.state == TorrentJobState::Queued
                && record.not_before_ms.is_some_and(|due| due > now_ms())
            {
                record.view.state = "scheduled".into();
                return;
            }
            record.view.state = match snapshot.state {
                TorrentJobState::Queued => "queued",
                TorrentJobState::Running => "running",
                TorrentJobState::Cancelling => "cancelling",
                TorrentJobState::Cancelled => "cancelled",
                TorrentJobState::Completed => "completed",
                TorrentJobState::Failed => "failed",
            }
            .into();
            record.view.bytes_received = snapshot.received;
            record.view.total_bytes = snapshot.total;
            record.view.error = snapshot.error;
            record.view.cleanup_pending = false;
            // The helper named and published the folder: from here on the
            // job's destination is that folder, not the root it went into.
            if let Some(published) = snapshot.published
                && record.torrent_auto
            {
                if published_inside(&record.destination, Path::new(&published)) {
                    record.destination = PathBuf::from(&published);
                    record.view.destination = Some(published);
                    record.torrent_auto = false;
                } else {
                    record.view.state = "failed".into();
                    record.view.error = Some("torrent.transfer_failed".into());
                }
            }
        }
    }
    let (action, retryable) = recovery_action(&record.view);
    if record.view.action.is_none() {
        record.view.action = action;
    }
    record.view.retryable = retryable;
    if is_terminal(&record.view.state) && record.finished_at_ms.is_none() {
        record.finished_at_ms = Some(now_ms());
        record.view.finished_at_ms = record.finished_at_ms;
    }
    sample_rate(record);
}

/// Updates the transfer rate and remaining time for one record.
///
/// Rate and remaining time are only meaningful while bytes are moving. Every
/// other state clears them, so a queued, paused or finished row never displays
/// a speed that nothing is producing.
fn sample_rate(record: &mut QueueRecord) {
    if record.view.state == "running" {
        record.rate.observe(record.view.bytes_received, now_ms());
        record.view.bytes_per_second = record.rate.bytes_per_second();
        record.view.eta_seconds = record
            .rate
            .eta_seconds(record.view.bytes_received, record.view.total_bytes);
    } else {
        record.rate.clear();
        record.view.bytes_per_second = None;
        record.view.eta_seconds = None;
    }
}

fn has_checksum(text: Option<&str>) -> bool {
    text.is_some_and(|text| !text.trim().is_empty())
}

/// Normalizes a pasted checksum. Blank means none; anything else must be a
/// SHA-256, and a person gets a plain explanation when it is not.
fn checked_checksum(text: &str) -> Result<Option<String>, String> {
    if text.trim().is_empty() {
        return Ok(None);
    }
    normalize_sha256(text).map(Some).ok_or_else(|| {
        "That checksum isn't a SHA-256. It should be 64 characters, each 0-9 or a-f.".to_string()
    })
}

const UNREADABLE_CHECKSUM: &str = "integrity.checksum_unreadable: the checksum saved with this download cannot be read, so it will not be downloaded unchecked. Remove it and add it again.";

/// Creates a file job that enforces the record's checksum when it has one.
///
/// Fails closed: a checksum that cannot be read never produces a job that
/// downloads without it.
fn file_job(
    url: String,
    destination: PathBuf,
    context: RequestContext,
    expected_sha256: Option<&str>,
) -> Result<FileJob, String> {
    let job = FileJob::create_recoverable_with_context(url, destination, context);
    match expected_sha256 {
        None => Ok(job),
        Some(expected) => job
            .with_expected_sha256(expected)
            .map_err(|_| UNREADABLE_CHECKSUM.to_string()),
    }
}

fn torrent_job(record: &QueueRecord, source: String, policy: TorrentPolicy) -> JobHandle {
    let mut request = match (
        &record.torrent_metadata_path,
        &record.torrent_metadata_sha256,
    ) {
        (Some(path), Some(hash)) => fetchpath_torrent::request_local(
            source,
            path.clone(),
            hash.clone(),
            record.destination.clone(),
            record.id.clone(),
            policy.discover_peers,
            policy.upload,
        ),
        _ => fetchpath_torrent::request(
            source,
            record.destination.clone(),
            record.id.clone(),
            policy.discover_peers,
            policy.upload,
        ),
    };
    request.auto_name = record.torrent_auto;
    JobHandle::Torrent(TorrentJob::create(request))
}

fn recovery_action(snapshot: &JobSnapshot) -> (Option<String>, bool) {
    if snapshot.state != "failed" {
        return (None, false);
    }
    let error = snapshot.error.as_deref().unwrap_or_default();
    (Some(action_for_error(error).into()), true)
}

/// The next step a failure calls for, from its code (finding F3: never from
/// words elsewhere in the message). Only `retry` can lead to an automatic
/// retry, and only for the codes [`retried_automatically`] accepts.
fn action_for_error(error: &str) -> &'static str {
    action_for_code(&error_code(error), error)
}

fn action_for_code(code: &str, error: &str) -> &'static str {
    match code {
        // A checksum failure needs a person: the source or the checksum is
        // wrong. It is deliberately not "retry", so it is never repeated.
        "integrity.checksum_mismatch" | "integrity.checksum_unreadable" => "check_checksum",
        "media.source_expired" | "media.unknown_variant" => "refresh_source",
        "media.helper_unavailable" => "configure_media_tools",
        "storage.destination_conflict" | "media.destination_conflict" | "destination.conflict" => {
            "choose_new_path"
        }
        "torrent.path_invalid" | "torrent.metadata_invalid" | "source.unsupported" => "edit_link",
        "media.invalid_source" => "edit_link",
        "source.transfer_failed" if refused_by_server(error) => "edit_link",
        code if code.starts_with("input.") => "edit_link",
        // Anything else, recognized or not, offers a person a retry; whether
        // the queue retries it by itself is decided by the code alone.
        _ => "retry",
    }
}

/// The stable code at the start of an error message: the leading token when
/// it has the shape of one, such as `input.invalid_url` from the core, or a
/// bare name from the media adapter, read as `media.<name>`. A message with
/// no code is `internal.unknown`.
pub(crate) fn error_code(error: &str) -> String {
    let token = error
        .trim_start()
        .split(|character: char| character == ':' || character.is_whitespace())
        .next()
        .unwrap_or_default();
    let word = |part: &str| {
        !part.is_empty()
            && part.starts_with(|c: char| c.is_ascii_lowercase())
            && part
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
    };
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() >= 2 && parts.iter().all(|part| word(part)) {
        token.to_owned()
    } else if parts.len() == 1 && word(token) && token.contains('_') {
        format!("media.{token}")
    } else {
        "internal.unknown".to_owned()
    }
}

/// The code of a failed download, for snapshots. A message the session wrote
/// itself has no code in its text, so the code follows from the action it
/// attached.
fn failure_code(view: &JobSnapshot) -> Option<String> {
    if view.state != "failed" {
        return None;
    }
    let code = error_code(view.error.as_deref().unwrap_or_default());
    if code != "internal.unknown" {
        return Some(code);
    }
    Some(
        match view.action.as_deref() {
            Some("choose_new_path") => "storage.destination_conflict",
            Some("check_checksum") => "integrity.checksum_unreadable",
            Some("configure_media_tools") => "media.helper_unavailable",
            Some("refresh_source") => "media.source_expired",
            _ => "internal.unknown",
        }
        .to_owned(),
    )
}

/// Whether the queue may retry a failure whose action is `retry` by itself
/// (finding F2). A recognized code may be; `internal.*`, which includes a
/// message with no code at all, waits for a person, as the job contract (§9)
/// requires.
fn retried_automatically(code: &str) -> bool {
    !code.starts_with("internal.")
}

/// True for an HTTP client error that repeating will not change, such as 404
/// or 403. The engine reports every HTTP error as a transfer failure, so the
/// status is read from its message here. 408 and 429 are the server asking
/// for time, and stay retryable along with every 5xx.
fn refused_by_server(error: &str) -> bool {
    error
        .split("HTTP status ")
        .nth(1)
        .and_then(|rest| rest.get(..3))
        .and_then(|code| code.parse::<u16>().ok())
        .is_some_and(|code| (400..500).contains(&code) && code != 408 && code != 429)
}

fn find_record<'a>(state: &'a QueueState, job_id: &str) -> Result<&'a QueueRecord, String> {
    state
        .records
        .iter()
        .find(|record| record.id == job_id)
        .ok_or_else(|| "This download is no longer available.".to_string())
}

fn find_record_mut<'a>(
    state: &'a mut QueueState,
    job_id: &str,
) -> Result<&'a mut QueueRecord, String> {
    state
        .records
        .iter_mut()
        .find(|record| record.id == job_id)
        .ok_or_else(|| "This download is no longer available.".to_string())
}

fn state_name(state: FileJobState) -> &'static str {
    match state {
        FileJobState::Queued => "queued",
        FileJobState::Running => "running",
        FileJobState::Cancelling => "cancelling",
        FileJobState::Completed => "completed",
        FileJobState::Cancelled => "cancelled",
        FileJobState::Failed => "failed",
    }
}

fn media_state_name(state: MediaJobState) -> &'static str {
    match state {
        MediaJobState::Queued => "queued",
        MediaJobState::Running => "running",
        MediaJobState::Cancelling => "cancelling",
        MediaJobState::Completed => "completed",
        MediaJobState::Cancelled => "cancelled",
        MediaJobState::Failed => "failed",
    }
}

fn default_job_kind() -> String {
    "file".into()
}

fn is_terminal(state: &str) -> bool {
    matches!(state, "completed" | "cancelled" | "failed")
}

/// Resolves the media helpers, preferring a directory the user chose.
///
/// A configured directory that no longer contains the helpers falls through to
/// the ambient discovery rather than failing outright, so a moved folder
/// degrades to "not set up" instead of hiding tools that are still findable.
fn discover_media_tools(configured: Option<&str>) -> Option<MediaTools> {
    configured
        .map(PathBuf::from)
        .and_then(|root| MediaTools::discover_in(&root).ok())
        .or_else(|| MediaTools::discover().ok())
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn display_url(url: &str) -> String {
    let base = url.split(['?', '#']).next().unwrap_or(url);
    let redacted = redact_userinfo(base);
    if redacted == url {
        redacted
    } else {
        format!("{redacted}?…")
    }
}

/// Replaces any `user:password@` section of an address with a fixed marker so a
/// credential can never reach queue JSON, an event payload, or an error string.
fn redact_userinfo(base: &str) -> String {
    let Some(scheme_end) = base.find("://") else {
        return base.to_owned();
    };
    let authority_start = scheme_end + 3;
    let authority_end = base[authority_start..]
        .find('/')
        .map_or(base.len(), |offset| authority_start + offset);
    let authority = &base[authority_start..authority_end];
    match authority.rfind('@') {
        None => base.to_owned(),
        Some(at) => format!(
            "{}…@{}{}",
            &base[..authority_start],
            &authority[at + 1..],
            &base[authority_end..]
        ),
    }
}

/// Validates an address arriving over IPC. Every rejection is a user-facing
/// sentence and never echoes the raw value, which may carry a signed query.
fn validated_source(raw: &str) -> Result<String, String> {
    let url = raw.trim();
    if url.is_empty() {
        return Err("Add at least one download address.".into());
    }
    if url.len() > MAX_SOURCE_LENGTH {
        return Err("That download address is too long to queue safely.".into());
    }
    if url.chars().any(char::is_control) {
        return Err("That download address contains characters Fetchpath cannot use.".into());
    }
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err(format!(
            "{} is not an HTTP or HTTPS address.",
            display_url(url)
        ));
    }
    let parsed = url::Url::parse(url)
        .map_err(|_| "That download address could not be read as a web address.".to_string())?;
    if parsed.host_str().is_none() {
        return Err("That download address is missing a host name.".into());
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(
            "Addresses that embed a user name or password are not accepted. Remove the credentials from the link."
                .into(),
        );
    }
    Ok(url.to_owned())
}

fn validated_torrent_source(raw: &str) -> Result<String, String> {
    let source = raw.trim();
    if source.is_empty() || source.len() > MAX_SOURCE_LENGTH || source.chars().any(char::is_control)
    {
        return Err("That torrent or magnet link cannot be queued safely.".into());
    }
    if source.starts_with("magnet:?") {
        let parsed = url::Url::parse(source)
            .map_err(|_| "That magnet link could not be read.".to_string())?;
        let has_hash = parsed.query_pairs().any(|(key, value)| {
            key == "xt" && (value.starts_with("urn:btih:") || value.starts_with("urn:btmh:"))
        });
        if !has_hash {
            return Err("That magnet link has no supported torrent hash.".into());
        }
        return Ok(source.to_owned());
    }
    if source.starts_with("https://") {
        let parsed = url::Url::parse(source)
            .map_err(|_| "That torrent link could not be read.".to_string())?;
        if parsed.host_str().is_none()
            || !parsed.username().is_empty()
            || parsed.password().is_some()
        {
            return Err("That torrent link has an invalid host or embedded credentials.".into());
        }
        return Ok(source.to_owned());
    }
    Err("Use a magnet link or HTTPS torrent link.".into())
}

/// Validates a destination arriving over IPC. The path must be absolute and free
/// of traversal, so a renderer cannot steer a write outside the folder the user
/// actually chose, and the leaf must be a name Windows can really create.
/// Whether the helper's reported folder is a single new name directly in the root.
fn published_inside(root: &Path, published: &Path) -> bool {
    published.parent() == Some(root)
        && published
            .components()
            .next_back()
            .is_some_and(|last| matches!(last, Component::Normal(_)))
}

/// A torrent's destination: a new folder, or for an automatic torrent the
/// existing root folder it is published into, which may be a drive root.
fn validated_torrent_destination(raw: &str, auto: bool) -> Result<PathBuf, String> {
    if !auto {
        return validated_destination(raw);
    }
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.len() > MAX_DESTINATION_LENGTH {
        return Err("Choose a folder for this torrent.".into());
    }
    if trimmed.chars().any(char::is_control) {
        return Err("That destination path contains characters Windows cannot use.".into());
    }
    let path = PathBuf::from(trimmed);
    plain_path(&path)?;
    Ok(path)
}

fn validated_destination(raw: &str) -> Result<PathBuf, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("Every queued download needs a destination filename.".into());
    }
    if trimmed.len() > MAX_DESTINATION_LENGTH {
        return Err("That destination path is too long.".into());
    }
    if trimmed.chars().any(char::is_control) {
        return Err("That destination path contains characters Windows cannot use.".into());
    }
    let path = PathBuf::from(trimmed);
    if path.file_name().and_then(|name| name.to_str()).is_none() {
        return Err("Every queued download needs a destination filename.".into());
    }
    plain_path(&path)?;
    Ok(path)
}

/// Whether `path` is an ordinary full path Windows applies its name rules
/// to, with every folder and name a plain one. Shared by destinations, rule
/// folders and the default folder, so a folder accepted there is never one a
/// download into it would be refused for (FP-067).
pub(crate) fn plain_path(path: &Path) -> Result<(), String> {
    if path
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err("A destination path cannot contain \"..\".".into());
    }
    // A drive path or a network share. Device (`\\.\`) and verbatim
    // (`\\?\`) paths skip the rules Windows applies to ordinary names, and a
    // folder part such as `CON` or `a:stream` would then reach a device or a
    // hidden stream (FP-067).
    let ordinary = match path.components().next() {
        Some(Component::Prefix(prefix)) => {
            matches!(prefix.kind(), Prefix::Disk(_) | Prefix::UNC(..))
        }
        _ => false,
    };
    if !path.is_absolute() || !ordinary {
        return Err("Choose a full destination path, including its drive.".into());
    }
    // Every folder and the file name must be a plain name. A colon names an
    // NTFS alternate data stream, so `notes.txt:hidden` would attach bytes to
    // a file the person never chose while still satisfying the create-only
    // publication fence. The drive letter's colon lives in the prefix.
    for component in path.components() {
        let Component::Normal(part) = component else {
            continue;
        };
        let Some(part) = part.to_str() else {
            return Err("That destination path contains characters Windows cannot use.".into());
        };
        if part != part.trim_end_matches(['.', ' ']) {
            return Err(format!(
                "{part:?}: a destination filename or folder cannot end with a dot or a space."
            ));
        }
        if let Some(offending) = part.chars().find(|character| {
            matches!(
                character,
                '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*'
            )
        }) {
            return Err(format!(
                "A destination filename or folder cannot contain {offending}."
            ));
        }
        // A name must read as what it is on the person's approval card:
        // embeddings, overrides and isolates reorder what follows (a name
        // ending `\u{202E}txt.exe` looks like a text file), and line or
        // paragraph separators break the line. Joiners and marks that do
        // neither stay allowed: Persian, Indic and emoji names need them
        // (FP-067).
        if part.chars().any(|character| {
            matches!(
                character,
                '\u{2028}'..='\u{202E}' | '\u{2066}'..='\u{2069}'
            )
        }) {
            return Err(
                "A destination name cannot contain characters that reorder or break its text."
                    .into(),
            );
        }
        if is_reserved_device_name(part) {
            return Err(format!(
                "{part:?} is reserved by Windows; choose another name."
            ));
        }
    }
    Ok(())
}

/// Names Windows keeps for devices, with or without an extension: `CON`,
/// `PRN`, `AUX`, `NUL`, `CONIN$`, `CONOUT$`, and `COM` or `LPT` followed by
/// 1 to 9 or a superscript 1 to 3.
fn is_reserved_device_name(name: &str) -> bool {
    let stem = name
        .split('.')
        .next()
        .unwrap_or(name)
        .trim_end_matches(' ')
        .to_uppercase();
    if matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) {
        return true;
    }
    let mut chars = stem.chars();
    let head: String = chars.by_ref().take(3).collect();
    let rest: Vec<char> = chars.collect();
    (head == "COM" || head == "LPT")
        && matches!(
            rest.as_slice(),
            ['1'..='9'] | ['\u{b9}' | '\u{b2}' | '\u{b3}']
        )
}

fn restartable_url(url: &str) -> Option<String> {
    (!url.contains(['?', '#'])).then(|| url.to_owned())
}

fn torrent_metadata_path(state_path: &Path, id: &str) -> Option<PathBuf> {
    uuid::Uuid::parse_str(id).ok()?;
    Some(
        state_path
            .parent()?
            .join("torrent-metadata")
            .join(format!("{id}.torrent")),
    )
}

fn verify_torrent_metadata(path: &Path, expected: &str) -> io::Result<()> {
    use sha2::{Digest, Sha256};
    if expected.len() != 64 || !expected.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid metadata hash",
        ));
    }
    let attributes = fs::symlink_metadata(path)?;
    if !attributes.file_type().is_file() || attributes.len() > MAX_TORRENT_METADATA_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid metadata file",
        ));
    }
    let bytes = fs::read(path)?;
    if bytes.len() as u64 > MAX_TORRENT_METADATA_BYTES
        || format!("{:x}", Sha256::digest(bytes)) != expected.to_ascii_lowercase()
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "metadata hash changed",
        ));
    }
    Ok(())
}

#[cfg(test)]
fn save_persisted(path: &Path, state: &QueueState) -> io::Result<()> {
    write_queue(path, state, 0, 0)
}

/// The ledger and event log's file, beside the queue.
const ENGINE_FILE: &str = "engine-v1.json";

/// Reads the engine file. A missing or unreadable one starts empty: it only
/// holds replayable history and the recent command ledger, never a job.
fn load_engine(path: &Path) -> DurableEngine {
    File::open(path)
        .ok()
        .and_then(|file| serde_json::from_reader::<_, DurableEngine>(io::BufReader::new(file)).ok())
        .filter(|engine| engine.schema_version == ENGINE_SCHEMA_VERSION)
        .unwrap_or_default()
}

/// Writes JSON through a synced temporary file and a rename.
fn write_json_atomically(path: &Path, value: &impl Serialize) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension("json.new");
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&temporary)?;
    // Serialized first and written once, so a failed write is an error
    // rather than a short file that is synced and renamed into place.
    file.write_all(&serde_json::to_vec(value)?)?;
    file.sync_all()?;
    drop(file);
    fs::rename(&temporary, path)
}

fn write_queue(
    path: &Path,
    state: &QueueState,
    engine_generation: u64,
    engine_cursor: u64,
) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let queue = PersistedQueue {
        schema_version: QUEUE_SCHEMA_VERSION,
        records: state
            .records
            .iter()
            .map(|record| PersistedRecord {
                id: record.id.clone(),
                restart_url: record.restart_url.clone(),
                credential_ref: record.credential_ref.clone(),
                display_url: record.display_url.clone(),
                destination: record.destination.display().to_string(),
                not_before_ms: record.not_before_ms,
                created_at_ms: record.created_at_ms,
                finished_at_ms: record.finished_at_ms,
                media_variant_id: record.media_variant_id.clone(),
                media_quality: record.media_quality.clone(),
                torrent_policy: record.torrent_policy,
                torrent_metadata_sha256: record.torrent_metadata_sha256.clone(),
                torrent_auto: record.torrent_auto,
                durable: record.durable.clone(),
                principal: record.principal.clone(),
                approval: record.approval.clone(),
                size_approved: record.size_approved,
                view: record.view.clone(),
            })
            .collect(),
        engine_generation,
        engine_cursor,
    };
    let temporary = path.with_extension("json.new");
    let backup = path.with_extension("json.bak");
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&temporary)?;
    // One write: straight into the file, serde's small writes cost a system
    // call each, about a millisecond per job on a long queue.
    let mut bytes = serde_json::to_vec_pretty(&queue)?;
    bytes.push(b'\n');
    file.write_all(&bytes)?;
    file.sync_all()?;
    drop(file);

    let _ = fs::remove_file(&backup);
    if path.exists() {
        fs::rename(path, &backup)?;
    }
    if let Err(error) = fs::rename(&temporary, path) {
        let _ = fs::rename(&backup, path);
        return Err(error);
    }
    Ok(())
}

/// What the queue file holds.
enum Loaded {
    Current(PersistedQueue),
    /// Written by a newer Fetchpath. Its records, when they can still be
    /// read, are only shown.
    Newer {
        version: Option<u64>,
        queue: Option<PersistedQueue>,
    },
    Missing,
}

#[cfg(test)]
impl Loaded {
    fn current(self) -> Option<PersistedQueue> {
        match self {
            Self::Current(queue) => Some(queue),
            _ => None,
        }
    }
}

/// What a queue file says about its format.
enum Version {
    Current,
    /// Newer than this build; the number when it is one this build can read.
    Newer(Option<u64>),
    /// Not a queue this build or a later one wrote: missing, corrupt, or 0.
    Unreadable,
}

fn version_of(text: &[u8]) -> Version {
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(text) else {
        return Version::Unreadable;
    };
    match value.get("schemaVersion") {
        None | Some(serde_json::Value::Null) => Version::Unreadable,
        Some(version) => match version.as_u64() {
            Some(number) if (1..=u64::from(QUEUE_SCHEMA_VERSION)).contains(&number) => {
                Version::Current
            }
            Some(number) if number > u64::from(QUEUE_SCHEMA_VERSION) => {
                Version::Newer(Some(number))
            }
            Some(_) => Version::Unreadable,
            // A version written in a form this build does not use ("2", 2.5,
            // or beyond u64) can only come from a later build.
            None => Version::Newer(None),
        },
    }
}

/// Reads the queue, falling back to the backup when the queue is missing or
/// unreadable. A file from a newer build, in either place, decides (finding
/// F1): the session is then read-only, so neither file is ever replaced,
/// including a newer backup behind a queue an older build wrote over it.
fn load_persisted(path: &Path) -> io::Result<Loaded> {
    let read = |candidate: &Path| match fs::read(candidate) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    };
    let files = [read(path)?, read(&path.with_extension("json.bak"))?];
    for text in files.iter().flatten() {
        if let Version::Newer(version) = version_of(text) {
            return Ok(Loaded::Newer {
                version,
                queue: serde_json::from_slice::<PersistedQueue>(text).ok(),
            });
        }
    }
    for text in files.iter().flatten() {
        if let Version::Current = version_of(text)
            && let Ok(queue) = serde_json::from_slice::<PersistedQueue>(text)
        {
            return Ok(Loaded::Current(queue));
        }
    }
    Ok(Loaded::Missing)
}

fn newer_queue_explanation(version: Option<u64>) -> String {
    let format = version.map_or(String::new(), |version| format!(" (format {version})"));
    format!(
        "Your download list was saved by a newer version of Fetchpath{format}. \
         This version shows it without changing it and starts nothing. \
         Install the newer version to keep using it; the list is kept as it is."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_opening_probe_byte_is_never_reported_as_the_rate() {
        const MIB: u64 = 1024 * 1024;
        let mut rate = RateEstimate::default();
        rate.observe(0, 0);
        rate.observe(1, 1_000); // the one-byte range probe
        assert_eq!(rate.bytes_per_second(), None);
        assert_eq!(rate.eta_seconds(1, Some(24 * MIB)), None);
        rate.observe(4 * MIB, 1_500); // the ranges arrive
        assert!(rate.bytes_per_second().unwrap() > MIB);

        // A link that really is slow still gets a rate, averaged from its start.
        let mut slow = RateEstimate::default();
        slow.observe(0, 0);
        slow.observe(5_000, 5_000);
        assert_eq!(slow.bytes_per_second(), Some(1_000));
    }

    /// The blank line that ends an HTTP request head.
    const HEAD_TERMINATOR: &[u8] = b"\r\n\r\n";
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;
    use std::time::Duration;

    /// Settings promises that only connection problems are retried
    /// automatically. A link the server refuses is not one.
    #[test]
    fn a_link_the_server_refuses_waits_for_a_person_instead_of_retrying() {
        for refused in [400, 401, 403, 404, 410, 451] {
            let error = format!("source.transfer_failed: HTTP status {refused}");
            assert_eq!(action_for_error(&error), "edit_link", "HTTP {refused}");
        }
        for transient in [408, 429, 500, 502, 503] {
            let error = format!("source.transfer_failed: HTTP status {transient}");
            assert_eq!(action_for_error(&error), "retry", "HTTP {transient}");
        }
        assert_eq!(
            action_for_error("source.transfer_failed: [7] Couldn't connect to server"),
            "retry"
        );
    }

    fn fixture(body: Vec<u8>, slow: bool) -> (String, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request);
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    )
                    .as_bytes(),
                )
                .unwrap();
            for chunk in body.chunks(16 * 1024) {
                if stream.write_all(chunk).is_err() {
                    break;
                }
                if slow {
                    thread::sleep(Duration::from_millis(5));
                }
            }
        });
        (url, handle)
    }

    /// A fixture that honours `Range` and `If-Range`, so a resumed transfer can
    /// be observed continuing from an offset rather than starting over.
    ///
    /// It serves connections in a loop and counts the requests that carried an
    /// `If-Range` header. That header is sent only by the resume path, so the
    /// count is direct evidence that a resume used the checkpoint instead of
    /// quietly re-downloading the whole file.
    fn resumable_fixture(
        body: Vec<u8>,
        chunk_delay: Duration,
    ) -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let if_range_requests = Arc::new(AtomicUsize::new(0));
        let counter = if_range_requests.clone();

        thread::spawn(move || {
            let total = body.len();
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let mut head = Vec::new();
                let mut byte = [0_u8; 1];
                while head.len() < 8192 {
                    match stream.read(&mut byte) {
                        Ok(1) => head.push(byte[0]),
                        _ => break,
                    }
                    if head.ends_with(HEAD_TERMINATOR) {
                        break;
                    }
                }
                let request = String::from_utf8_lossy(&head).to_string();
                if request.to_ascii_lowercase().contains("if-range:") {
                    counter.fetch_add(1, Ordering::SeqCst);
                }
                let start = request
                    .lines()
                    .find_map(|line| {
                        let rest = line.strip_prefix("Range: bytes=")?;
                        rest.split('-').next()?.parse::<usize>().ok()
                    })
                    .unwrap_or(0)
                    .min(total);

                let header = if start > 0 {
                    format!(
                        concat!(
                            "HTTP/1.1 206 Partial Content\r\n",
                            "Content-Length: {}\r\n",
                            "Content-Range: bytes {}-{}/{}\r\n",
                            "ETag: \"fixture-v1\"\r\n",
                            "Accept-Ranges: bytes\r\n",
                            "Connection: close\r\n\r\n",
                        ),
                        total - start,
                        start,
                        total - 1,
                        total
                    )
                } else {
                    format!(
                        concat!(
                            "HTTP/1.1 200 OK\r\n",
                            "Content-Length: {}\r\n",
                            "ETag: \"fixture-v1\"\r\n",
                            "Accept-Ranges: bytes\r\n",
                            "Connection: close\r\n\r\n",
                        ),
                        total
                    )
                };
                if stream.write_all(header.as_bytes()).is_err() {
                    continue;
                }
                for chunk in body[start..].chunks(32 * 1024) {
                    if stream.write_all(chunk).is_err() {
                        break;
                    }
                    if !chunk_delay.is_zero() {
                        thread::sleep(chunk_delay);
                    }
                }
            }
        });
        (url, if_range_requests)
    }

    /// Waits for a record to reach one of `states`, returning the snapshot.
    fn wait_for_state(jobs: &Session, job_id: &str, states: &[&str]) -> JobSnapshot {
        for _ in 0..600 {
            let snapshot = jobs.snapshot(job_id).unwrap();
            if states.contains(&snapshot.state.as_str()) {
                return snapshot;
            }
            thread::sleep(Duration::from_millis(5));
        }
        panic!(
            "desktop job never reached {states:?}; last state was {:?}",
            jobs.snapshot(job_id).unwrap().state
        );
    }

    fn wait_for_terminal(jobs: &Session, job_id: &str) -> JobSnapshot {
        for _ in 0..400 {
            let snapshot = jobs.snapshot(job_id).unwrap();
            if is_terminal(&snapshot.state) {
                return snapshot;
            }
            thread::sleep(Duration::from_millis(5));
        }
        panic!("desktop job did not finish");
    }

    fn sha256_hex(bytes: &[u8]) -> String {
        use sha2::{Digest, Sha256};
        format!("{:x}", Sha256::digest(bytes))
    }

    fn checked_draft(url: String, destination: &Path, checksum: &str) -> JobDraft {
        JobDraft {
            checksum: Some(checksum.into()),
            url,
            destination: destination.display().to_string(),
            not_before_ms: None,
        }
    }

    #[test]
    fn a_download_matching_its_pasted_checksum_completes() {
        let dir = tempfile::tempdir().unwrap();
        let body = vec![0x41; 64 * 1024];
        let digest = sha256_hex(&body);
        let (url, server) = fixture(body.clone(), false);
        let jobs = Session::in_memory(3);
        let destination = dir.path().join("checked.bin");
        // Pasted the way checksum files often print it.
        let pasted = format!("  SHA256:{}  ", digest.to_uppercase());
        let started = jobs
            .enqueue(vec![checked_draft(url, &destination, &pasted)])
            .unwrap()
            .remove(0);
        assert_eq!(started.expected_sha256.as_deref(), Some(digest.as_str()));

        let completed = wait_for_terminal(&jobs, &started.job_id);
        server.join().unwrap();
        assert_eq!(completed.state, "completed");
        assert_eq!(completed.observed_sha256.as_deref(), Some(digest.as_str()));
        assert_eq!(completed.expected_sha256.as_deref(), Some(digest.as_str()));
        assert_eq!(fs::read(&destination).unwrap(), body);
    }

    #[test]
    fn a_checksum_mismatch_saves_nothing_and_is_never_retried_automatically() {
        let dir = tempfile::tempdir().unwrap();
        let body = vec![0x42; 64 * 1024];
        let (url, server) = fixture(body, false);
        let jobs = Session::in_memory(3);
        let destination = dir.path().join("mismatch.bin");
        let wrong = sha256_hex(b"a different file");
        let started = jobs
            .enqueue(vec![checked_draft(url, &destination, &wrong)])
            .unwrap()
            .remove(0);

        let failed = wait_for_terminal(&jobs, &started.job_id);
        server.join().unwrap();
        assert_eq!(failed.state, "failed");
        assert_eq!(failed.action.as_deref(), Some("check_checksum"));
        assert!(failed.retryable, "a person can still retry by hand");
        let error = failed.error.unwrap();
        assert!(error.contains("checksum_mismatch"), "{error}");
        assert!(
            error.contains(&wrong),
            "the expected value is shown: {error}"
        );
        assert!(!destination.exists(), "nothing was saved");

        // Automatic retry is on by default and only repeats plain transport
        // failures. A checksum mismatch needs a person, so it is left alone.
        let mut state = jobs.inner.lock().unwrap();
        jobs.schedule_automatic_retries(&mut state, &jobs.settings());
        let record = find_record(&state, &started.job_id).unwrap();
        assert_eq!(record.retry_at_ms, None);
        assert_eq!(record.view.state, "failed");
    }

    #[test]
    fn a_malformed_checksum_is_refused_with_a_plain_explanation() {
        let dir = tempfile::tempdir().unwrap();
        let jobs = Session::in_memory(3);
        let error = jobs
            .enqueue(vec![checked_draft(
                "http://127.0.0.1:9/file.bin".into(),
                &dir.path().join("x.bin"),
                "d41d8cd98f00b204e9800998ecf8427e",
            )])
            .unwrap_err();
        assert!(error.contains("isn't a SHA-256"), "{error}");
        assert!(jobs.list().unwrap().is_empty(), "nothing was queued");
    }

    #[test]
    fn a_blank_checksum_means_none() {
        let dir = tempfile::tempdir().unwrap();
        let jobs = Session::in_memory(0);
        let queued = jobs
            .enqueue(vec![checked_draft(
                "http://127.0.0.1:9/file.bin".into(),
                &dir.path().join("x.bin"),
                "   ",
            )])
            .unwrap()
            .remove(0);
        assert_eq!(queued.expected_sha256, None);
    }

    #[test]
    fn a_checksum_is_refused_on_a_batch_because_it_describes_one_file() {
        let dir = tempfile::tempdir().unwrap();
        let jobs = Session::in_memory(3);
        let error = jobs
            .enqueue(vec![
                checked_draft(
                    "http://127.0.0.1:9/a.bin".into(),
                    &dir.path().join("a.bin"),
                    &"ab".repeat(32),
                ),
                draft("http://127.0.0.1:9/b.bin", &dir.path().join("b.bin")),
            ])
            .unwrap_err();
        assert!(error.contains("one file"), "{error}");
    }

    #[test]
    fn retrying_with_a_corrected_checksum_completes() {
        let dir = tempfile::tempdir().unwrap();
        let body = vec![0x43; 32 * 1024];
        let digest = sha256_hex(&body);
        let (url, server) = fixture(body.clone(), false);
        let jobs = Session::in_memory(3);
        let destination = dir.path().join("corrected.bin");
        let started = jobs
            .enqueue(vec![checked_draft(url, &destination, &"00".repeat(32))])
            .unwrap()
            .remove(0);
        assert_eq!(wait_for_terminal(&jobs, &started.job_id).state, "failed");
        server.join().unwrap();

        let (url, server) = fixture(body.clone(), false);
        jobs.retry(&started.job_id, Some(url), None, Some(digest.clone()))
            .unwrap();
        let completed = wait_for_terminal(&jobs, &started.job_id);
        server.join().unwrap();
        assert_eq!(completed.state, "completed");
        assert_eq!(completed.expected_sha256.as_deref(), Some(digest.as_str()));
        assert_eq!(fs::read(&destination).unwrap(), body);
    }

    #[test]
    fn an_unreadable_saved_checksum_fails_closed_after_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let record = QueueRecord::new_checked(
            "http://127.0.0.1:9/file.bin".into(),
            dir.path().join("file.bin"),
            None,
            Some("ab".repeat(32)),
        );
        let mut view = record.view.clone();
        view.state = "queued".into();
        view.expected_sha256 = Some("not a checksum".into());
        let saved = PersistedRecord {
            id: record.id.clone(),
            restart_url: record.restart_url.clone(),
            credential_ref: None,
            display_url: record.display_url.clone(),
            destination: record.destination.display().to_string(),
            not_before_ms: None,
            created_at_ms: record.created_at_ms,
            finished_at_ms: None,
            media_variant_id: None,
            media_quality: None,
            torrent_policy: None,
            torrent_metadata_sha256: None,
            torrent_auto: false,
            durable: RecordDurable::default(),
            principal: Principal::User,
            approval: None,
            size_approved: false,
            view,
        };
        let restored =
            QueueRecord::restore(saved, now_ms(), None, None, Path::new("queue-v1.json"));
        assert!(
            restored.job.is_none(),
            "never a job that downloads unchecked"
        );
        assert_eq!(restored.view.state, "failed");
        assert_eq!(restored.view.action.as_deref(), Some("check_checksum"));
    }

    #[test]
    fn local_torrent_snapshot_survives_original_deletion_and_rejects_tampering() {
        let dir = tempfile::tempdir().unwrap();
        let queue = dir.path().join("queue-v1.json");
        let original = dir.path().join("debian.torrent");
        fs::write(&original, b"d4:infod4:name6:debian6:lengthi0eee").unwrap();
        let destination = dir.path().join("debian");
        let policy = TorrentPolicy {
            discover_peers: true,
            upload: false,
        };
        let id = {
            let session = Session::load(queue.clone(), 1).unwrap();
            session
                .enqueue_torrent_file_for(
                    &original,
                    &destination.display().to_string(),
                    false,
                    Some(now_ms() + 3_600_000),
                    policy,
                    &Origin::default(),
                )
                .unwrap()
                .job_id
        };
        fs::remove_file(&original).unwrap();
        let restored = Session::load(queue.clone(), 1).unwrap();
        assert_eq!(restored.snapshot(&id).unwrap().state, "scheduled");
        assert_eq!(restored.snapshot(&id).unwrap().source, "debian.torrent");
        drop(restored);
        fs::write(
            dir.path()
                .join("torrent-metadata")
                .join(format!("{id}.torrent")),
            b"tampered",
        )
        .unwrap();
        let tampered = Session::load(queue, 1).unwrap();
        assert_eq!(tampered.snapshot(&id).unwrap().state, "failed");
    }

    #[test]
    fn a_default_and_an_approval_hold_survive_a_restart_through_the_engine() {
        use crate::engine::{Engine, InProcessClient};
        use fetchpath_protocol::command::{
            Command, ConflictPolicy, DestinationIntent, JobInput, JobRequest,
        };
        use fetchpath_protocol::principal::{AgentName, AgentPolicy};
        use fetchpath_protocol::{ClientId, EngineClient, Timestamp};
        let dir = tempfile::tempdir().unwrap();
        let queue = dir.path().join("queue-v1.json");
        let granted = dir.path().join("granted");
        fs::create_dir_all(&granted).unwrap();
        let original = dir.path().join("debian.torrent");
        fs::write(&original, b"d4:infod4:name6:debian6:lengthi0eee").unwrap();
        let torrent = |name: &str, discover_peers, input| Command::CreateJob {
            request: JobRequest::Torrent {
                input,
                destination: DestinationIntent {
                    path: granted.join(name).display().to_string(),
                    conflict: ConflictPolicy::Ask,
                },
                not_before: Some(Timestamp::from_unix_ms(now_ms() as i64 + 3_600_000)),
                discover_peers,
                upload: false,
            },
        };
        let name = AgentName::try_from("helper").unwrap();
        let (person, agent) = {
            let engine = Engine::new(Arc::new(Session::load(queue.clone(), 1).unwrap()));
            let user = InProcessClient::manual(Arc::clone(&engine));
            let helper = InProcessClient::manual(Arc::clone(&engine))
                .with_principal(Principal::Agent(name.clone()));
            user.send(
                &ClientId::random(),
                Command::SetAgentPolicy {
                    agent: name,
                    policy: Some(AgentPolicy {
                        folders: vec![granted.display().to_string()],
                        ..AgentPolicy::default()
                    }),
                },
            )
            .unwrap();
            let id = |client: &InProcessClient, command| match client
                .send(&ClientId::random(), command)
                .unwrap()
            {
                fetchpath_protocol::message::CommandResult::Job { job } => job.job_id.to_string(),
                other => panic!("{other:?}"),
            };
            (
                id(
                    &user,
                    torrent(
                        "person",
                        None,
                        JobInput::TorrentFile {
                            path: original.display().to_string(),
                        },
                    ),
                ),
                id(
                    &helper,
                    torrent(
                        "agent",
                        Some(true),
                        JobInput::Url {
                            url: fetchpath_protocol::SensitiveUrl::try_from(
                                "magnet:?xt=urn:btih:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                                    .to_owned(),
                            )
                            .unwrap(),
                        },
                    ),
                ),
            )
        };
        let restored = Session::load(queue, 1).unwrap();
        let state = restored.inner.lock().unwrap();
        let find = |id: &str| state.records.iter().find(|record| record.id == id).unwrap();
        assert!(find(&person).torrent_policy.unwrap().discover_peers);
        let held = find(&agent);
        assert!(held.torrent_policy.unwrap().discover_peers);
        assert!(
            held.approval
                .as_ref()
                .unwrap()
                .reasons
                .contains(&ApprovalReason::PeerDiscovery)
        );
    }

    #[test]
    fn refreshing_a_local_torrent_retires_its_metadata_before_using_the_new_link() {
        let dir = tempfile::tempdir().unwrap();
        let queue = dir.path().join("queue-v1.json");
        let original = dir.path().join("first.torrent");
        fs::write(&original, b"d4:infod4:name5:first6:lengthi0eee").unwrap();
        let destination = dir.path().join("output");
        let session = Session::load(queue.clone(), 1).unwrap();
        let id = session
            .enqueue_torrent_file_for(
                &original,
                &destination.display().to_string(),
                false,
                Some(now_ms() + 3_600_000),
                TorrentPolicy {
                    discover_peers: true,
                    upload: false,
                },
                &Origin::default(),
            )
            .unwrap()
            .job_id;
        let snapshot = torrent_metadata_path(&queue, &id).unwrap();
        assert!(snapshot.exists());
        session.cancel(&id).unwrap();
        fs::create_dir(&destination).unwrap();
        session.durable.lock().unwrap().defer = true;
        session
            .retry(
                &id,
                Some("https://example.test/second.torrent".into()),
                None,
                None,
            )
            .unwrap();
        assert!(
            snapshot.exists(),
            "deferred save must keep metadata referenced by the old queue"
        );
        let persisted = load_persisted(&queue).unwrap().current().unwrap();
        assert!(persisted.records[0].torrent_metadata_sha256.is_some());
        let state = session.inner.lock().unwrap();
        let record = find_record(&state, &id).unwrap();
        assert_eq!(
            record.live_url.as_deref(),
            Some("https://example.test/second.torrent")
        );
        assert!(record.torrent_metadata_sha256.is_none());
        assert!(record.torrent_metadata_path.is_none());
        let mut durable = session.durable.lock().unwrap();
        durable.defer = false;
        session.write_locked(&state, &mut durable).unwrap();
        assert!(
            snapshot.exists(),
            "the backup queue still names the old metadata"
        );
        let backup: PersistedQueue =
            serde_json::from_slice(&fs::read(queue.with_extension("json.bak")).unwrap()).unwrap();
        assert!(backup.records[0].torrent_metadata_sha256.is_some());
        session.write_locked(&state, &mut durable).unwrap();
        assert!(!snapshot.exists());
    }

    #[test]
    fn an_automatic_torrent_keeps_its_root_across_restart_and_follows_the_published_folder() {
        let dir = tempfile::tempdir().unwrap();
        let queue = dir.path().join("queue-v1.json");
        let root = dir.path().join("downloads");
        fs::create_dir(&root).unwrap();
        let policy = TorrentPolicy {
            discover_peers: true,
            upload: false,
        };
        let id = {
            let session = Session::load(queue.clone(), 1).unwrap();
            session
                .enqueue_torrent_for(
                    TorrentDraft {
                        source: "https://example.test/a.torrent".into(),
                        destination: root.display().to_string(),
                        not_before_ms: Some(now_ms() + 3_600_000),
                        policy,
                        auto: true,
                    },
                    &Origin::default(),
                )
                .unwrap()
                .job_id
        };
        let persisted = load_persisted(&queue).unwrap().current().unwrap();
        assert!(
            persisted.records[0].torrent_auto,
            "{:?}",
            persisted.records[0].view
        );
        assert_eq!(persisted.records[0].destination, root.display().to_string());
        let restored = Session::load(queue, 1).unwrap();
        let state = restored.inner.lock().unwrap();
        let record = find_record(&state, &id).unwrap();
        assert!(record.torrent_auto);
        assert_eq!(
            record.view.state, "scheduled",
            "an existing root is no conflict"
        );
        let Some(JobHandle::Torrent(job)) = record.job.as_ref() else {
            panic!("the restored torrent has a job");
        };
        assert!(job.request().auto_name);
    }

    #[test]
    fn a_published_folder_must_be_one_name_directly_inside_the_root() {
        let root = Path::new(r"C:\downloads");
        assert!(published_inside(root, Path::new(r"C:\downloads\Album")));
        assert!(!published_inside(root, Path::new(r"C:\downloads")));
        assert!(!published_inside(root, Path::new(r"C:\downloads\a\b")));
        assert!(!published_inside(root, Path::new(r"C:\elsewhere\Album")));
        assert!(!published_inside(root, Path::new(r"C:\downloads\..")));
    }

    #[test]
    fn an_agent_refreshing_a_torrent_waits_for_peer_approval_again() {
        let output = tempfile::tempdir().unwrap();
        let agent = AgentName::try_from("helper").unwrap();
        let principal = Principal::Agent(agent);
        let jobs = Session::in_memory(1);
        let mut record = QueueRecord::new_torrent(
            "magnet:?xt=urn:btih:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
            output.path().join("torrent"),
            false,
            None,
            TorrentPolicy {
                discover_peers: true,
                upload: true,
            },
        );
        record.principal = principal.clone();
        record.view.state = "failed".into();
        let job_id = record.id.clone();
        jobs.inner
            .lock()
            .expect("desktop jobs poisoned")
            .records
            .push(record);

        let refreshed = jobs
            .retry_as(
                &job_id,
                Some("magnet:?xt=urn:btih:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into()),
                None,
                None,
                &principal,
                Vec::new(),
            )
            .unwrap();
        assert_eq!(refreshed.state, "awaiting_approval");
        let state = jobs.inner.lock().expect("desktop jobs poisoned");
        let reasons = &find_record(&state, &job_id)
            .unwrap()
            .approval
            .as_ref()
            .unwrap()
            .reasons;
        assert!(reasons.contains(&ApprovalReason::PeerDiscovery));
        assert!(reasons.contains(&ApprovalReason::PeerUpload));
    }

    #[test]
    fn a_stated_length_reaches_the_queue_view_as_a_total() {
        let dir = tempfile::tempdir().unwrap();
        let body = vec![0x31; 256 * 1024];
        let expected = body.len() as u64;
        let (url, server) = fixture(body, false);
        let jobs = Session::in_memory(3);
        let started = jobs
            .enqueue(vec![JobDraft {
                checksum: None,
                url,
                destination: dir.path().join("sized.bin").display().to_string(),
                not_before_ms: None,
            }])
            .unwrap()
            .remove(0);
        // A queued download has not spoken to the source yet, so it has no total.
        assert_eq!(started.total_bytes, None);

        let completed = wait_for_terminal(&jobs, &started.job_id);
        server.join().unwrap();
        assert_eq!(completed.state, "completed");
        assert_eq!(completed.total_bytes, Some(expected));
        // Nothing is running, so no rate or remaining time may be reported.
        assert_eq!(completed.bytes_per_second, None);
        assert_eq!(completed.eta_seconds, None);
    }

    #[test]
    fn pausing_a_running_download_resumes_from_its_checkpoint() {
        use std::sync::atomic::Ordering;

        let dir = tempfile::tempdir().unwrap();
        let destination = dir.path().join("resumable.bin");
        let body: Vec<u8> = (0..768 * 1024).map(|index| (index % 251) as u8).collect();
        // Slow enough that the test can pause partway through rather than
        // racing a transfer that has already finished.
        let (url, if_range_requests) = resumable_fixture(body.clone(), Duration::from_millis(12));

        let jobs = Session::in_memory(3);
        let started = jobs
            .enqueue(vec![JobDraft {
                checksum: None,
                url,
                destination: destination.display().to_string(),
                not_before_ms: None,
            }])
            .unwrap()
            .remove(0);

        // Wait for real bytes before pausing; pausing at zero would prove nothing.
        let mut running = wait_for_state(&jobs, &started.job_id, &["running", "completed"]);
        for _ in 0..600 {
            if running.state != "running" || running.bytes_received > 64 * 1024 {
                break;
            }
            thread::sleep(Duration::from_millis(5));
            running = jobs.snapshot(&started.job_id).unwrap();
        }
        assert_eq!(
            running.state, "running",
            "fixture finished before the pause"
        );
        let paused = jobs.pause(&started.job_id).unwrap();
        assert_eq!(paused.state, "paused");
        assert!(
            paused.bytes_received > 0,
            "a pause has to keep the bytes already verified"
        );
        assert_eq!(
            paused.bytes_per_second, None,
            "a paused row reports no speed"
        );
        assert!(!destination.exists(), "nothing is published while paused");
        let paused_at = paused.bytes_received;

        // Pausing is durable: another poll must not restart it.
        thread::sleep(Duration::from_millis(60));
        assert_eq!(jobs.snapshot(&started.job_id).unwrap().state, "paused");

        let resumed = jobs.resume(&started.job_id).unwrap();
        assert!(matches!(resumed.state.as_str(), "queued" | "running"));

        let completed = wait_for_terminal(&jobs, &started.job_id);
        assert_eq!(completed.state, "completed", "error: {:?}", completed.error);
        assert_eq!(fs::read(&destination).unwrap(), body);
        assert_eq!(
            if_range_requests.load(Ordering::SeqCst),
            1,
            "the resume must continue from the checkpoint at {paused_at} bytes, \
             which is the only thing that sends If-Range"
        );
    }

    #[test]
    fn a_queued_download_pauses_without_ever_contacting_the_source() {
        let dir = tempfile::tempdir().unwrap();
        let body = vec![9; 512 * 1024];
        let (slow_url, slow_server) = fixture(body.clone(), true);
        let (waiting_url, waiting_server) = fixture(body, false);

        // One slot, so the second download is still queued when it is paused.
        let jobs = Session::in_memory(1);
        let created = jobs
            .enqueue(vec![
                JobDraft {
                    checksum: None,
                    url: slow_url,
                    destination: dir.path().join("slow.bin").display().to_string(),
                    not_before_ms: None,
                },
                JobDraft {
                    checksum: None,
                    url: waiting_url,
                    destination: dir.path().join("waiting.bin").display().to_string(),
                    not_before_ms: None,
                },
            ])
            .unwrap();

        let waiting_id = created[1].job_id.clone();
        let paused = jobs.pause(&waiting_id).unwrap();
        assert_eq!(paused.state, "paused");
        assert_eq!(paused.bytes_received, 0);

        // The first download finishing must not promote the paused one.
        wait_for_terminal(&jobs, &created[0].job_id);
        thread::sleep(Duration::from_millis(60));
        assert_eq!(jobs.snapshot(&waiting_id).unwrap().state, "paused");

        let resumed = jobs.resume(&waiting_id).unwrap();
        assert!(matches!(resumed.state.as_str(), "queued" | "running"));
        let completed = wait_for_terminal(&jobs, &waiting_id);
        assert_eq!(completed.state, "completed");

        let _ = slow_server.join();
        let _ = waiting_server.join();
    }

    #[test]
    fn a_paused_download_is_still_paused_after_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let state_path = dir.path().join("queue-v1.json");
        let body = vec![4; 256 * 1024];
        let (url, server) = fixture(body, true);

        let waiting_id = {
            let jobs = Session::load(state_path.clone(), 1).unwrap();
            let created = jobs
                .enqueue(vec![JobDraft {
                    checksum: None,
                    url,
                    destination: dir.path().join("paused.bin").display().to_string(),
                    not_before_ms: None,
                }])
                .unwrap();
            let id = created[0].job_id.clone();
            jobs.pause(&id).unwrap();
            id
        };

        // A fresh process reading the same queue file.
        let reopened = Session::load(state_path, 1).unwrap();
        let restored = reopened.snapshot(&waiting_id).unwrap();
        assert_eq!(
            restored.state, "paused",
            "restoring a paused download as queued would start a transfer the user stopped"
        );

        // And it is still resumable from the new process.
        let resumed = reopened.resume(&waiting_id).unwrap();
        assert!(matches!(resumed.state.as_str(), "queued" | "running"));
        let _ = server.join();
    }

    #[test]
    fn media_downloads_refuse_to_pause_rather_than_pretending() {
        let dir = tempfile::tempdir().unwrap();
        let body = vec![1; 64 * 1024];
        let (url, server) = fixture(body, true);
        let jobs = Session::in_memory(1);
        let created = jobs
            .enqueue(vec![JobDraft {
                checksum: None,
                url,
                destination: dir.path().join("file.bin").display().to_string(),
                not_before_ms: None,
            }])
            .unwrap();
        let id = created[0].job_id.clone();

        // Force the record to look like a media job, which has no checkpoint to
        // resume from. The command must say so instead of dropping the bytes.
        {
            let mut state = jobs.inner.lock().unwrap();
            find_record_mut(&mut state, &id).unwrap().view.kind = "media".into();
        }
        let error = jobs.pause(&id).unwrap_err();
        assert!(
            error.contains("cancelled and started again"),
            "unexpected message: {error}"
        );
        let _ = server.join();
    }

    #[test]
    fn settings_bound_concurrency_and_take_effect_immediately() {
        let dir = tempfile::tempdir().unwrap();
        let jobs = Session::load(dir.path().join("queue-v1.json"), 3).unwrap();
        assert_eq!(jobs.max_active(), 3);

        let mut next = jobs.settings();
        next.max_active_downloads = 999;
        let applied = jobs.update_settings(next).unwrap();
        assert_eq!(applied.max_active_downloads, settings::MAX_ACTIVE_DOWNLOADS);
        assert_eq!(jobs.max_active(), settings::MAX_ACTIVE_DOWNLOADS);

        // And the choice survives a restart.
        let reopened = Session::load(dir.path().join("queue-v1.json"), 3).unwrap();
        assert_eq!(
            reopened.max_active(),
            settings::MAX_ACTIVE_DOWNLOADS,
            "a stored concurrency must win over the launch default"
        );
    }

    #[test]
    fn stats_count_the_same_rows_the_queue_shows() {
        let dir = tempfile::tempdir().unwrap();
        let body = vec![2; 128 * 1024];
        let (first_url, first_server) = fixture(body.clone(), false);
        let (second_url, second_server) = fixture(body, true);
        let jobs = Session::in_memory(1);
        let created = jobs
            .enqueue(vec![
                JobDraft {
                    checksum: None,
                    url: first_url,
                    destination: dir.path().join("one.bin").display().to_string(),
                    not_before_ms: None,
                },
                JobDraft {
                    checksum: None,
                    url: second_url,
                    destination: dir.path().join("two.bin").display().to_string(),
                    not_before_ms: None,
                },
            ])
            .unwrap();
        wait_for_terminal(&jobs, &created[0].job_id);
        jobs.pause(&created[1].job_id).ok();

        let stats = jobs.stats();
        let rows = jobs.list().unwrap();
        let counted = stats.running
            + stats.queued
            + stats.scheduled
            + stats.paused
            + stats.completed
            + stats.failed;
        assert_eq!(
            counted,
            rows.len(),
            "every row has to be counted exactly once: {stats:?} against {} rows",
            rows.len()
        );
        assert_eq!(stats.max_active_downloads, 1);
        assert!(stats.completed_bytes > 0);

        // Neither fixture is joined. Pausing can win the race against the
        // worker's first socket write, in which case that fixture is still
        // blocked in `accept` and joining it would hang the test rather than
        // reveal anything about the queue.
        drop((first_server, second_server));
    }
    #[test]
    fn production_command_path_downloads_real_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let destination = dir.path().join("desktop.bin");
        let body = vec![0x5a; 128 * 1024];
        let (url, server) = fixture(body.clone(), false);
        let jobs = Session::in_memory(3);
        let started = jobs
            .enqueue(vec![JobDraft {
                checksum: None,
                url,
                destination: destination.display().to_string(),
                not_before_ms: None,
            }])
            .unwrap()
            .remove(0);
        let completed = wait_for_terminal(&jobs, &started.job_id);
        server.join().unwrap();
        assert_eq!(completed.state, "completed");
        assert_eq!(fs::read(&destination).unwrap(), body);
        assert!(completed.observed_sha256.is_some());
    }

    #[test]
    fn a_download_into_a_folder_not_yet_made_gets_it_as_it_starts() {
        let dir = tempfile::tempdir().unwrap();
        let destination = dir
            .path()
            .join("tiny-model")
            .join("onnx")
            .join("model.onnx");
        let body = vec![0x5a; 64 * 1024];
        let (url, server) = fixture(body.clone(), false);
        let jobs = Session::in_memory(3);
        let started = jobs
            .enqueue(vec![JobDraft {
                checksum: None,
                url,
                destination: destination.display().to_string(),
                not_before_ms: None,
            }])
            .unwrap()
            .remove(0);
        let completed = wait_for_terminal(&jobs, &started.job_id);
        server.join().unwrap();
        assert_eq!(completed.state, "completed", "{:?}", completed.error);
        assert_eq!(fs::read(&destination).unwrap(), body);
    }

    #[test]
    fn batch_queue_obeys_concurrency_and_catches_up_due_schedules() {
        let dir = tempfile::tempdir().unwrap();
        let body = vec![7; 512 * 1024];
        let (first_url, first_server) = fixture(body.clone(), true);
        let (second_url, second_server) = fixture(body, false);
        let jobs = Session::in_memory(1);
        let due = now_ms() + 40;
        let created = jobs
            .enqueue(vec![
                JobDraft {
                    checksum: None,
                    url: first_url,
                    destination: dir.path().join("first.bin").display().to_string(),
                    not_before_ms: None,
                },
                JobDraft {
                    checksum: None,
                    url: second_url,
                    destination: dir.path().join("second.bin").display().to_string(),
                    not_before_ms: Some(due),
                },
            ])
            .unwrap();
        assert_eq!(
            jobs.snapshot(&created[1].job_id).unwrap().state,
            "scheduled"
        );
        thread::sleep(Duration::from_millis(60));
        let after_due = jobs.list().unwrap();
        assert!(after_due.iter().any(|job| {
            job.job_id == created[1].job_id && matches!(job.state.as_str(), "queued" | "running")
        }));
        wait_for_terminal(&jobs, &created[0].job_id);
        wait_for_terminal(&jobs, &created[1].job_id);
        first_server.join().unwrap();
        second_server.join().unwrap();
    }

    #[test]
    fn queue_persistence_recovers_safe_sources_and_requests_private_source_refresh() {
        let dir = tempfile::tempdir().unwrap();
        let state_path = dir.path().join("queue.json");
        let mut state = QueueState {
            records: vec![
                QueueRecord::new(
                    "http://127.0.0.1:9/safe".into(),
                    dir.path().join("safe.bin"),
                    Some(now_ms() + 60_000),
                ),
                QueueRecord::new(
                    "https://example.test/file?token=secret".into(),
                    dir.path().join("private.bin"),
                    None,
                ),
            ],
            link_reviews: Vec::new(),
        };
        state.records[1].view.state = "cancelled".into();
        state.records[1].finished_at_ms = Some(now_ms());
        state.records[1].view.finished_at_ms = state.records[1].finished_at_ms;
        save_persisted(&state_path, &state).unwrap();
        let text = fs::read_to_string(&state_path).unwrap();
        assert!(!text.contains("secret"));

        let recovered = Session::load(state_path, 1).unwrap().list().unwrap();
        assert!(recovered.iter().any(|job| job.state == "scheduled"));
        assert!(recovered.iter().any(|job| {
            job.state == "needs_source" && job.action.as_deref() == Some("edit_link")
        }));
    }

    #[test]
    fn destination_conflicts_are_actionable_and_never_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let destination = dir.path().join("existing.bin");
        fs::write(&destination, b"keep").unwrap();
        let jobs = Session::in_memory(1);
        let record = jobs
            .enqueue(vec![JobDraft {
                checksum: None,
                url: "http://127.0.0.1:9/file".into(),
                destination: destination.display().to_string(),
                not_before_ms: None,
            }])
            .unwrap()
            .remove(0);
        assert_eq!(record.state, "failed");
        assert_eq!(record.action.as_deref(), Some("choose_new_path"));
        assert_eq!(fs::read(destination).unwrap(), b"keep");
    }

    #[test]
    fn input_and_unknown_job_errors_are_user_facing() {
        let jobs = Session::in_memory(1);
        assert!(
            jobs.enqueue(Vec::new())
                .unwrap_err()
                .contains("at least one")
        );
        assert!(jobs.snapshot("missing").unwrap_err().contains("no longer"));
    }

    #[test]
    fn browser_inbox_is_consumed_once_into_the_persistent_queue() {
        use crate::browser_inbox::{BrowserCookie, CaptureRequest, SCHEMA_VERSION};

        let dir = tempfile::tempdir().unwrap();
        let download_dir = dir.path().join("downloads");
        fs::create_dir_all(&download_dir).unwrap();
        let state_path = dir.path().join("queue-v1.json");
        let body = b"browser capture bytes".repeat(1024);
        let (url, server) = fixture(body.clone(), false);
        let capture_id = uuid::Uuid::new_v4().to_string();
        let store = BridgeStore::new(dir.path().to_path_buf());
        store
            .accept(&CaptureRequest {
                schema_version: SCHEMA_VERSION,
                capture_id: capture_id.clone(),
                method: "GET".into(),
                url: format!("{url}/archive.bin?token=private"),
                suggested_filename: "archive.bin".into(),
                referrer: None,
                cookies: Vec::<BrowserCookie>::new(),
                user_initiated: true,
            })
            .unwrap();

        let jobs =
            Session::load_with_browser(state_path.clone(), 1, Some(download_dir.clone())).unwrap();
        let queued = jobs.list().unwrap();
        assert_eq!(queued.len(), 1);
        let completed = wait_for_terminal(&jobs, &queued[0].job_id);
        server.join().unwrap();
        assert_eq!(completed.state, "completed");
        assert_eq!(fs::read(download_dir.join("archive.bin")).unwrap(), body);
        assert!(store.pending().unwrap().is_empty());
        let persisted = fs::read_to_string(&state_path).unwrap();
        assert!(!persisted.contains("token=private"));

        let restored = Session::load_with_browser(state_path, 1, Some(download_dir))
            .unwrap()
            .list()
            .unwrap();
        assert_eq!(restored.len(), 1);
        assert_eq!(restored[0].job_id, queued[0].job_id);
    }

    #[test]
    fn browser_captures_follow_the_rules_like_any_new_download() {
        use crate::browser_inbox::{BrowserCookie, CaptureRequest, SCHEMA_VERSION};
        use fetchpath_protocol::model::{RuleActions, RuleConditions, RuleSpec};

        let dir = tempfile::tempdir().unwrap();
        let (downloads, isos) = (dir.path().join("downloads"), dir.path().join("isos"));
        let jobs = Session::load_with_browser(
            dir.path().join("queue-v1.json"),
            1,
            Some(downloads.clone()),
        )
        .unwrap();
        let rule = |types: &str, then: RuleActions| RuleSpec {
            name: None,
            when: RuleConditions {
                file_types: vec![types.into()],
                ..Default::default()
            },
            then,
        };
        let folder = RuleActions {
            folder: Some(isos.display().to_string()),
            ..Default::default()
        };
        let checksum = RuleActions {
            require_checksum: true,
            ..Default::default()
        };
        jobs.add_rule(rule("iso", folder), None).unwrap();
        jobs.add_rule(rule("exe", checksum), None).unwrap();

        let store = BridgeStore::new(dir.path().to_path_buf());
        for name in ["disc.iso", "setup.exe", "notes.txt"] {
            store
                .accept(&CaptureRequest {
                    schema_version: SCHEMA_VERSION,
                    capture_id: uuid::Uuid::new_v4().to_string(),
                    method: "GET".into(),
                    url: format!("http://127.0.0.1:9/{name}"),
                    suggested_filename: name.into(),
                    referrer: None,
                    cookies: Vec::<BrowserCookie>::new(),
                    user_initiated: true,
                })
                .unwrap();
        }
        let queued = jobs.list().unwrap();
        let placed = |name: &str| {
            queued
                .iter()
                .filter_map(|job| job.destination.as_deref())
                .find(|path| path.ends_with(name))
                .map(PathBuf::from)
        };
        assert_eq!(placed("disc.iso"), Some(isos.join("disc.iso")));
        assert_eq!(placed("notes.txt"), Some(downloads.join("notes.txt")));
        // No checksum yet, so it waits in Add download instead of queuing.
        assert_eq!(placed("setup.exe"), None);
        assert_eq!(jobs.take_link_reviews(), ["http://127.0.0.1:9/setup.exe"]);
        assert!(store.pending().unwrap().is_empty());
    }

    /// A YouTube page sent from the browser used to be queued as a file, which
    /// saved the page's HTML. It now waits for Add download instead.
    #[test]
    fn a_captured_media_page_goes_to_review_instead_of_the_file_queue() {
        use crate::browser_inbox::{BrowserCookie, CaptureRequest, SCHEMA_VERSION};

        let dir = tempfile::tempdir().unwrap();
        let download_dir = dir.path().join("downloads");
        fs::create_dir_all(&download_dir).unwrap();
        let store = BridgeStore::new(dir.path().to_path_buf());
        let page = "https://www.youtube.com/watch?v=arj7oStGLkU";
        store
            .accept(&CaptureRequest {
                schema_version: SCHEMA_VERSION,
                capture_id: uuid::Uuid::new_v4().to_string(),
                method: "GET".into(),
                url: page.into(),
                suggested_filename: "watch".into(),
                referrer: None,
                cookies: Vec::<BrowserCookie>::new(),
                user_initiated: true,
            })
            .unwrap();

        let credential_ref = store.pending().unwrap()[0].credential_ref.clone();
        let queue = dir.path().join("queue-v1.json");
        let jobs =
            Session::load_with_browser(queue.clone(), 1, Some(download_dir.clone())).unwrap();
        assert!(jobs.list().unwrap().is_empty(), "not queued as a file");
        jobs.list().unwrap();
        // FP-056: taken in, but still pending in the inbox until a client
        // takes it, so an engine leaving now loses nothing.
        assert_eq!(store.pending().unwrap().len(), 1, "kept until handed out");
        drop(jobs);
        let jobs =
            Session::load_with_browser(queue.clone(), 1, Some(download_dir.clone())).unwrap();
        jobs.list().unwrap();
        jobs.list().unwrap();
        assert_eq!(
            jobs.take_link_reviews(),
            vec![page.to_owned()],
            "once, after a restart"
        );
        assert!(store.pending().unwrap().is_empty(), "handed out, then done");
        assert!(
            store.load_secret(&credential_ref).is_err(),
            "its unused protected context is deleted"
        );
        jobs.list().unwrap();
        assert!(jobs.take_link_reviews().is_empty(), "never offered twice");
        drop(jobs);
        let jobs = Session::load_with_browser(queue, 1, Some(download_dir)).unwrap();
        jobs.list().unwrap();
        assert!(jobs.take_link_reviews().is_empty());
    }

    #[test]
    fn media_pages_are_told_apart_from_files_on_media_sites() {
        assert!(is_media_page("https://www.youtube.com/watch?v=abc"));
        assert!(is_media_page("https://youtu.be/abc"));
        assert!(is_media_page("https://m.youtube.com/shorts/abc"));
        assert!(is_media_page("https://vimeo.com/12345"));
        assert!(!is_media_page("https://archive.org/download/item/film.mp4"));
        assert!(!is_media_page("https://example.com/watch?v=abc"));
        assert!(!is_media_page("https://notyoutube.com/watch"));
        assert!(!is_media_page("not a url"));
    }

    /// An agent's video download stops at the agent's size limit and waits
    /// for the person, as a file download does (FP-067): the media helper's
    /// bytes are measured while it writes.
    #[cfg(windows)]
    #[test]
    fn an_agent_media_download_stops_at_its_size_limit() {
        let fixtures =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../adapters/media/tests/fixtures");
        let tools = MediaTools::new(
            fixtures.join("growing-helper.cmd"),
            &fixtures,
            fixtures.join("fake-ffprobe.cmd"),
        )
        .unwrap();
        let output = tempfile::tempdir().unwrap();
        let destination = output.path().join("video.mp4");
        let jobs = Session::in_memory_with_media(1, tools.clone());
        let agent = AgentName::try_from("helper").unwrap();
        jobs.set_agent_policy(
            agent.clone(),
            Some(AgentPolicy {
                folders: vec![output.path().display().to_string()],
                max_bytes: 256 * 1024,
                max_new_jobs_per_hour: 20,
            }),
        )
        .unwrap();
        let mut record = QueueRecord::new_media(
            "https://example.test/watch?v=1".into(),
            destination.clone(),
            None,
            "video-18".into(),
            "360p".into(),
            tools,
        );
        record.principal = Principal::Agent(agent);
        let job_id = record.id.clone();
        jobs.inner
            .lock()
            .expect("desktop jobs poisoned")
            .records
            .push(record);

        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        loop {
            let snapshot = jobs.snapshot(&job_id).unwrap();
            if snapshot.state == "awaiting_approval" {
                break;
            }
            assert!(
                !is_terminal(&snapshot.state),
                "finished instead: {snapshot:?}"
            );
            assert!(
                std::time::Instant::now() < deadline,
                "never stopped: {snapshot:?}"
            );
            thread::sleep(Duration::from_millis(50));
        }
        let reasons = {
            let state = jobs.inner.lock().expect("desktop jobs poisoned");
            find_record(&state, &job_id)
                .unwrap()
                .approval
                .as_ref()
                .map(|approval| approval.reasons.clone())
        };
        assert_eq!(reasons, Some(vec![ApprovalReason::SizeLimit]));
        thread::sleep(Duration::from_secs(1));
        assert!(!destination.exists(), "nothing is published while it waits");
    }

    /// A helper that writes everything at once still cannot get past the
    /// agent's limit (FP-067 review): the adapter's cap refuses to publish,
    /// and the stop waits for the person as a size stop. Approving it lifts
    /// the cap and the download is saved.
    #[cfg(windows)]
    #[test]
    fn an_agent_media_burst_past_its_limit_waits_and_approval_lifts_it() {
        let fixtures =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../adapters/media/tests/fixtures");
        let tools = MediaTools::new(
            fixtures.join("burst-helper.cmd"),
            &fixtures,
            fixtures.join("fake-ffprobe-video.cmd"),
        )
        .unwrap();
        let output = tempfile::tempdir().unwrap();
        let destination = output.path().join("video.mp4");
        let jobs = Session::in_memory_with_media(1, tools.clone());
        let agent = AgentName::try_from("helper").unwrap();
        jobs.set_agent_policy(
            agent.clone(),
            Some(AgentPolicy {
                folders: vec![output.path().display().to_string()],
                max_bytes: 256 * 1024,
                max_new_jobs_per_hour: 20,
            }),
        )
        .unwrap();
        let mut record = QueueRecord::new_media(
            "https://example.test/watch?v=2".into(),
            destination.clone(),
            None,
            "video-18".into(),
            "360p".into(),
            tools,
        );
        record.principal = Principal::Agent(agent);
        let job_id = record.id.clone();
        jobs.inner
            .lock()
            .expect("desktop jobs poisoned")
            .records
            .push(record);

        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        loop {
            let snapshot = jobs.snapshot(&job_id).unwrap();
            if snapshot.state == "awaiting_approval" {
                break;
            }
            assert!(
                !is_terminal(&snapshot.state),
                "finished instead: {snapshot:?}"
            );
            assert!(
                std::time::Instant::now() < deadline,
                "never stopped: {snapshot:?}"
            );
            thread::sleep(Duration::from_millis(50));
        }
        assert!(
            !destination.exists(),
            "a burst past the limit was published"
        );

        jobs.approve(&job_id).unwrap();
        // The helpers are PowerShell scripts, slow to start under load.
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        let finished = loop {
            let snapshot = jobs.snapshot(&job_id).unwrap();
            if is_terminal(&snapshot.state) {
                break snapshot;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "never finished: {snapshot:?}"
            );
            thread::sleep(Duration::from_millis(50));
        };
        assert_eq!(finished.state, "completed", "{finished:?}");
        assert_eq!(
            std::fs::metadata(&destination).unwrap().len(),
            2 * 1024 * 1024
        );
    }

    #[cfg(windows)]
    #[test]
    fn media_expiry_is_a_refreshable_queue_failure() {
        let fixtures =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../adapters/media/tests/fixtures");
        let tools = MediaTools::new(
            fixtures.join("expired-helper.cmd"),
            &fixtures,
            fixtures.join("fake-ffprobe.cmd"),
        )
        .unwrap();
        let output = tempfile::tempdir().unwrap();
        let destination = output.path().join("media.mp4");
        let jobs = Session::in_memory_with_media(1, tools.clone());
        let record = QueueRecord::new_media(
            "https://example.test/watch?session=private".into(),
            destination.clone(),
            None,
            "video-fixture".into(),
            "720p".into(),
            tools,
        );
        let job_id = record.id.clone();
        jobs.inner
            .lock()
            .expect("desktop jobs poisoned")
            .records
            .push(record);

        let failed = wait_for_terminal(&jobs, &job_id);
        assert_eq!(failed.kind, "media");
        assert_eq!(failed.quality_label.as_deref(), Some("720p"));
        assert_eq!(failed.state, "failed");
        assert_eq!(failed.action.as_deref(), Some("refresh_source"));
        assert!(!failed.error.unwrap().contains("private"));
        assert!(!destination.exists());
    }

    fn draft(url: &str, destination: &Path) -> JobDraft {
        JobDraft {
            checksum: None,
            url: url.into(),
            destination: destination.display().to_string(),
            not_before_ms: None,
        }
    }

    #[test]
    fn destinations_cannot_escape_the_chosen_directory() {
        let dir = tempfile::tempdir().unwrap();
        let jobs = Session::in_memory(1);

        let traversal = dir.path().join("..").join("escaped.bin");
        let error = jobs
            .enqueue(vec![draft("http://127.0.0.1:9/file", &traversal)])
            .unwrap_err();
        assert!(error.contains(".."), "unexpected traversal error: {error}");

        let relative = jobs
            .enqueue(vec![JobDraft {
                checksum: None,
                url: "http://127.0.0.1:9/file".into(),
                destination: "escaped.bin".into(),
                not_before_ms: None,
            }])
            .unwrap_err();
        assert!(
            relative.contains("full destination path"),
            "unexpected relative-path error: {relative}"
        );

        let sibling = dir.path().join("sibling");
        fs::create_dir_all(&sibling).unwrap();
        let split_batch = jobs
            .enqueue(vec![
                draft("http://127.0.0.1:9/one", &dir.path().join("one.bin")),
                draft("http://127.0.0.1:9/two", &sibling.join("two.bin")),
            ])
            .unwrap_err();
        assert!(
            split_batch.contains("same folder"),
            "unexpected batch-containment error: {split_batch}"
        );
        assert!(!dir.path().join("one.bin").exists());
    }

    #[test]
    fn malformed_ipc_input_is_rejected_with_a_user_facing_error() {
        let dir = tempfile::tempdir().unwrap();
        let jobs = Session::in_memory(1);

        for (destination, expected) in [
            ("NUL", "full destination path"),
            ("trailing.", "full destination path"),
        ] {
            let error = jobs
                .enqueue(vec![JobDraft {
                    checksum: None,
                    url: "http://127.0.0.1:9/file".into(),
                    destination: destination.into(),
                    not_before_ms: None,
                }])
                .unwrap_err();
            assert!(error.contains(expected), "{destination}: {error}");
        }

        for name in ["NUL.bin", "COM1.bin"] {
            let error = jobs
                .enqueue(vec![draft(
                    "http://127.0.0.1:9/file",
                    &dir.path().join(name),
                )])
                .unwrap_err();
            assert!(error.contains("reserved by Windows"), "{name}: {error}");
        }

        let trailing_dot = jobs
            .enqueue(vec![draft(
                "http://127.0.0.1:9/file",
                &dir.path().join("name."),
            )])
            .unwrap_err();
        assert!(trailing_dot.contains("dot or a space"), "{trailing_dot}");

        // An alternate data stream would otherwise pass every other check and
        // attach bytes to a file the user never chose.
        let stream = jobs
            .enqueue(vec![JobDraft {
                checksum: None,
                url: "http://127.0.0.1:9/file".into(),
                destination: format!("{}\\notes.txt:hidden", dir.path().display()),
                not_before_ms: None,
            }])
            .unwrap_err();
        assert!(stream.contains("cannot contain :"), "{stream}");
        assert!(!dir.path().join("notes.txt").exists());

        // The drive letter's colon must still be accepted.
        let accepted = jobs.enqueue(vec![draft(
            "http://127.0.0.1:9/file",
            &dir.path().join("plain.bin"),
        )]);
        assert!(accepted.is_ok(), "{accepted:?}");
        jobs.remove(&accepted.unwrap()[0].job_id).unwrap();

        let control = jobs
            .enqueue(vec![JobDraft {
                checksum: None,
                url: "http://127.0.0.1:9/file".into(),
                destination: format!("{}\u{1}bad.bin", dir.path().display()),
                not_before_ms: None,
            }])
            .unwrap_err();
        assert!(control.contains("Windows cannot use"), "{control}");

        for (url, expected) in [
            (
                "file:///C:/Windows/System32/drivers/etc/hosts",
                "not an HTTP or HTTPS address",
            ),
            ("javascript:alert(1)", "not an HTTP or HTTPS address"),
            ("http://", "could not be read as a web address"),
            ("http://host\u{1}/file", "characters Fetchpath cannot use"),
            ("   ", "at least one download address"),
        ] {
            let error = jobs
                .enqueue(vec![draft(url, &dir.path().join("out.bin"))])
                .unwrap_err();
            assert!(error.contains(expected), "{url}: {error}");
        }

        let oversized = format!("https://example.test/{}", "a".repeat(MAX_SOURCE_LENGTH));
        let error = jobs
            .enqueue(vec![draft(&oversized, &dir.path().join("out.bin"))])
            .unwrap_err();
        assert!(error.contains("too long"), "{error}");
        assert!(!error.contains("aaaa"), "oversized address was echoed back");

        assert!(
            jobs.retry("not-a-job", None, None, None)
                .unwrap_err()
                .contains("no longer")
        );
        assert!(jobs.cancel("not-a-job").is_err());
        assert!(jobs.remove("not-a-job").is_err());
        assert!(jobs.start_now("not-a-job").is_err());
        assert!(jobs.list().unwrap().is_empty());
    }

    #[test]
    fn credentials_and_private_queries_never_reach_state_or_errors() {
        let dir = tempfile::tempdir().unwrap();
        let jobs = Session::in_memory(1);

        let error = jobs
            .enqueue(vec![draft(
                "https://user:hunter2@example.test/file.bin",
                &dir.path().join("out.bin"),
            )])
            .unwrap_err();
        assert!(!error.contains("hunter2"), "password echoed: {error}");
        assert!(error.contains("user name or password"), "{error}");

        assert_eq!(
            display_url("https://user:hunter2@example.test/file?token=abc"),
            "https://…@example.test/file?…"
        );
        assert_eq!(
            display_url("https://example.test/plain.bin"),
            "https://example.test/plain.bin"
        );

        let state_path = dir.path().join("queue-v1.json");
        let state = QueueState {
            records: vec![QueueRecord::new(
                "https://example.test/file?token=secret-value#frag".into(),
                dir.path().join("private.bin"),
                None,
            )],
            link_reviews: Vec::new(),
        };
        save_persisted(&state_path, &state).unwrap();
        let text = fs::read_to_string(&state_path).unwrap();
        assert!(!text.contains("secret-value"));
        assert!(!text.contains("frag"));
        for record in &state.records {
            assert!(record.restart_url.is_none());
            assert!(!record.view.source.contains("secret-value"));
        }
    }
}

#[cfg(test)]
mod characterization;
