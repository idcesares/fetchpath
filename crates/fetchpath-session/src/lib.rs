//! The download session: the queue, its persistence, scheduling, retry
//! policy, rate estimates, history, settings and media orchestration.
//!
//! Moved out of the desktop host unchanged (FP-049). It holds no UI or
//! transport types; the desktop calls it in-process.

pub use fetchpath_browser_inbox as browser_inbox;
mod durable;
pub mod engine;
pub mod policy;
pub mod settings;
mod wire;

use browser_inbox::BridgeStore;
use durable::{Durable, DurableEngine, RecordDurable, RemovedJob, Reported};
use fetchpath_core::{CancelResult, FileJob, FileJobState, RequestContext, normalize_sha256};
use fetchpath_media::{MediaInspection, MediaJob, MediaJobState, MediaTools};
use fetchpath_protocol::principal::{AgentName, AgentPolicy, ApprovalReason, Principal};
use serde::{Deserialize, Serialize};
use settings::Settings;
use std::collections::{BTreeMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

const QUEUE_SCHEMA_VERSION: u32 = 1;
pub const DEFAULT_MAX_ACTIVE: usize = 3;
/// Upper bound for an inter-process address. Long enough for real signed links,
/// short enough that a malformed renderer message cannot force unbounded work.
const MAX_SOURCE_LENGTH: usize = 8_192;
/// Upper bound for a destination path. Windows long paths stop well below this.
pub const MAX_DESTINATION_LENGTH: usize = 4_096;

pub struct Session {
    inner: Mutex<QueueState>,
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
    /// Set when the engine stops: no job starts any more (FP-053).
    halted: std::sync::atomic::AtomicBool,
    /// Set when the queue was written by a newer Fetchpath (FP-070): the
    /// saved list is shown as it is, nothing starts, and nothing is written
    /// over it. Holds the explanation.
    read_only: Option<String>,
    /// What each configured agent may do without asking (contract D1).
    agents: Mutex<BTreeMap<AgentName, AgentPolicy>>,
    agents_path: Option<PathBuf>,
}

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
}

impl JobHandle {
    fn start(&self) -> Result<(), &'static str> {
        match self {
            Self::File(job) => job.start(),
            Self::Media(job) => job.start(),
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
        }
    }

    fn join(&self) {
        match self {
            Self::File(job) => job.join(),
            Self::Media(job) => job.join(),
        }
    }

    fn completed(&self) -> bool {
        match self {
            Self::File(job) => job.snapshot().state == FileJobState::Completed,
            Self::Media(job) => job.snapshot().state == MediaJobState::Completed,
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
}

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
            .is_some_and(|approval| !approval.denied && !approval.withdrawn)
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
}

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
            halted: std::sync::atomic::AtomicBool::new(read_only.is_some()),
            read_only,
            agents: Mutex::new(policy::AgentsFile::load(&agents_path)),
            agents_path: Some(agents_path),
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
            inner: Mutex::new(QueueState::default()),
            state_path: None,
            settings_path: None,
            settings: Mutex::new(stored),
            settings_repaired: false,
            browser_store: None,
            browser_download_dir: None,
            media_tools: Mutex::new(None),
            durable: Mutex::new(Durable::default()),
            halted: std::sync::atomic::AtomicBool::new(false),
            read_only: None,
            agents: Mutex::new(BTreeMap::new()),
            agents_path: None,
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
        *self.settings.lock().expect("settings poisoned") = next.clone();
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
        let tools = self.media_tools().ok_or_else(|| {
            "Media tools are not set up yet. Open Settings to install or locate yt-dlp and ffmpeg."
                .to_string()
        })?;
        let source = validated_source(source)?;
        tools.inspect(&source).map_err(|error| error.to_string())
    }

    pub fn enqueue_media(&self, draft: MediaDraft) -> Result<JobSnapshot, String> {
        self.enqueue_media_for(draft, &Origin::default())
    }

    pub(crate) fn enqueue_media_for(
        &self,
        draft: MediaDraft,
        origin: &Origin,
    ) -> Result<JobSnapshot, String> {
        let tools = self.media_tools().ok_or_else(|| {
            "Media tools are not set up yet. Open Settings to install or locate yt-dlp and ffmpeg."
                .to_string()
        })?;
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
            // An agent retrying what it withdrew asks the person again.
            if approval.withdrawn && !by.is_user() {
                for reason in &approval.reasons {
                    if !hold.contains(reason) {
                        hold.push(*reason);
                    }
                }
            }
        }
        let url = url.map(|url| validated_source(&url)).transpose()?;
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
        if let Some(url) = url {
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
        if let Some(destination) = destination {
            record.destination = destination;
        }
        // An empty field clears the checksum; anything else replaces it.
        if let Some(checksum) = checksum {
            record.view.expected_sha256 = checksum;
        }
        let live_url = record.live_url.clone().ok_or_else(|| {
            "This source included private query values. Paste a refreshed link to continue."
                .to_string()
        })?;
        if record.destination.exists() {
            record.view.state = "failed".into();
            record.view.error = Some("A file already exists at this destination.".into());
            record.view.action = Some("choose_new_path".into());
            record.view.retryable = true;
        } else {
            record.job = Some(if let Some(variant_id) = record.media_variant_id.clone() {
                let tools = self.media_tools().ok_or_else(|| {
                    "Media tools are not set up yet. Open Settings to install or locate yt-dlp and ffmpeg."
                        .to_string()
                })?;
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
        self.save_locked(&mut state)
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
            Some(_) if record.destination.exists() => Err((
                "failed",
                "A file already exists at this destination.",
                "choose_new_path",
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
                approval.denied = true;
            }
            let now = now_ms();
            record.view.state = "cancelled".into();
            record.view.error = Some("The person declined this download.".into());
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
            .map(|record| record.destination.clone())
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

    /// Sets or removes one agent's access and saves every agent's.
    pub fn set_agent_policy(
        &self,
        agent: AgentName,
        next: Option<AgentPolicy>,
    ) -> Result<(), String> {
        let next = next.map(policy::validated).transpose()?;
        let mut agents = self.agents.lock().expect("agents poisoned");
        let mut updated = agents.clone();
        match next {
            Some(policy) => updated.insert(agent, policy),
            None => updated.remove(&agent),
        };
        if let Some(path) = self.agents_path.as_ref() {
            write_json_atomically(path, &policy::AgentsFile::new(updated.clone())).map_err(
                |error| format!("Could not save agent access at {}: {error}", path.display()),
            )?;
        }
        *agents = updated;
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
                record.job = if let Some(variant_id) = record.media_variant_id.clone() {
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
            if let Err(code) = job.start() {
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
            if record.destination.exists() {
                continue;
            }
            record.attempt += 1;
            let delay = settings.retry_delay_seconds(record.attempt);
            let due_at = now.saturating_add(delay.saturating_mul(1_000));
            record.job = if let Some(variant_id) = record.media_variant_id.clone() {
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
                engine.schema_version = QUEUE_SCHEMA_VERSION;
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
            // A page on a media site saved as a file would be its HTML. It goes
            // to Add download instead, where the video and its quality are
            // found. Its cookies are not carried over.
            if is_media_page(&secret.url) {
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
            let destination = unique_browser_destination(
                download_dir,
                &capture.suggested_filename,
                &state.records,
            );
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
            },
        }
    }

    fn restore(
        saved: PersistedRecord,
        now: u64,
        browser_store: Option<&BridgeStore>,
        media_tools: Option<&MediaTools>,
    ) -> Self {
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
        let awaiting_approval = saved
            .approval
            .as_ref()
            .is_some_and(|approval| !approval.denied);
        let (job, state, error, action, retryable) = if awaiting_approval {
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
            let job = if let Some(variant_id) = saved.media_variant_id.clone() {
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
            if saved.media_variant_id.is_none() && job.is_none() {
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
        "storage.destination_conflict" | "media.destination_conflict" => "choose_new_path",
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
fn error_code(error: &str) -> String {
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

/// Validates a destination arriving over IPC. The path must be absolute and free
/// of traversal, so a renderer cannot steer a write outside the folder the user
/// actually chose, and the leaf must be a name Windows can really create.
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
    if path
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err("A destination path cannot contain \"..\".".into());
    }
    if !path.is_absolute()
        || path
            .components()
            .next()
            .is_none_or(|component| !matches!(component, Component::Prefix(_)))
    {
        return Err("Choose a full destination path, including its drive.".into());
    }
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return Err("Every queued download needs a destination filename.".into());
    };
    if name != name.trim_end_matches(['.', ' ']) {
        return Err("A destination filename cannot end with a dot or a space.".into());
    }
    // A colon in the leaf names an NTFS alternate data stream, so
    // `notes.txt:hidden` would attach bytes to a file the user never chose while
    // still satisfying the create-only publication fence. The drive letter's
    // colon lives in the prefix component, not here, so it is unaffected.
    if let Some(offending) = name.chars().find(|character| {
        matches!(
            character,
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*'
        )
    }) {
        return Err(format!(
            "A destination filename cannot contain {offending}."
        ));
    }
    if is_reserved_device_name(name) {
        return Err("That destination filename is reserved by Windows.".into());
    }
    Ok(path)
}

/// Windows refuses these names in any directory and with any extension.
fn is_reserved_device_name(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or(name).to_ascii_uppercase();
    matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.as_bytes()[3].is_ascii_digit()
            && stem.as_bytes()[3] != b'0')
}

fn restartable_url(url: &str) -> Option<String> {
    (!url.contains(['?', '#'])).then(|| url.to_owned())
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
        .filter(|engine| engine.schema_version == QUEUE_SCHEMA_VERSION)
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
    serde_json::to_writer(io::BufWriter::new(&mut file), value)?;
    file.flush()?;
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
    serde_json::to_writer_pretty(&mut file, &queue)?;
    file.write_all(b"\n")?;
    file.flush()?;
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
            Some(number) if number == u64::from(QUEUE_SCHEMA_VERSION) => Version::Current,
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
            durable: RecordDurable::default(),
            principal: Principal::User,
            approval: None,
            size_approved: false,
            view,
        };
        let restored = QueueRecord::restore(saved, now_ms(), None, None);
        assert!(
            restored.job.is_none(),
            "never a job that downloads unchecked"
        );
        assert_eq!(restored.view.state, "failed");
        assert_eq!(restored.view.action.as_deref(), Some("check_checksum"));
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
