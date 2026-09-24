pub mod browser_bridge;
pub mod browser_setup;
pub mod media_setup;
pub mod settings;

use browser_bridge::BridgeStore;
use fetchpath_core::{CancelResult, FileJob, FileJobState, RequestContext, normalize_sha256};
use fetchpath_media::{MediaInspection, MediaJob, MediaJobState, MediaTools};
use serde::{Deserialize, Serialize};
use settings::Settings;
use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, State, WindowEvent};

const QUEUE_SCHEMA_VERSION: u32 = 1;
const DEFAULT_MAX_ACTIVE: usize = 3;
/// Upper bound for an inter-process address. Long enough for real signed links,
/// short enough that a malformed renderer message cannot force unbounded work.
const MAX_SOURCE_LENGTH: usize = 8_192;
/// Upper bound for a destination path. Windows long paths stop well below this.
const MAX_DESTINATION_LENGTH: usize = 4_096;

struct DesktopJobs {
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
}

#[derive(Default)]
struct QueueState {
    records: Vec<QueueRecord>,
    /// Pages captured from the browser that are media rather than files. They
    /// open in Add download, where the quality is chosen, instead of being
    /// saved as a web page. In memory only: the interface takes them within a
    /// poll, and the browser inbox has already recorded the capture.
    link_reviews: Vec<String>,
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
    view: JobSnapshot,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct JobDraft {
    url: String,
    destination: String,
    not_before_ms: Option<u64>,
    /// A SHA-256 as the person pasted it. Normalized when the draft is queued.
    #[serde(default)]
    checksum: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MediaDraft {
    url: String,
    variant_id: String,
    quality_label: String,
    destination: String,
    not_before_ms: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct JobSnapshot {
    job_id: String,
    source: String,
    state: String,
    bytes_received: u64,
    /// Engine-confirmed total. Absent whenever the source never stated a
    /// length, which the interface shows as an unknown size rather than a
    /// percentage it cannot support.
    #[serde(default)]
    total_bytes: Option<u64>,
    /// Smoothed transfer rate, present only while bytes are actually moving.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    bytes_per_second: Option<u64>,
    /// Remaining time, present only when both a total and a rate are known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    eta_seconds: Option<u64>,
    /// How many automatic retries this download has already consumed.
    #[serde(default)]
    attempt: u32,
    destination: Option<String>,
    observed_sha256: Option<String>,
    /// The SHA-256 the person supplied, normalized. When present the engine
    /// publishes nothing that does not match it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    expected_sha256: Option<String>,
    cleanup_pending: bool,
    error: Option<String>,
    action: Option<String>,
    retryable: bool,
    created_at_ms: u64,
    not_before_ms: Option<u64>,
    finished_at_ms: Option<u64>,
    #[serde(default = "default_job_kind")]
    kind: String,
    #[serde(default)]
    quality_label: Option<String>,
}

/// Aggregate queue figures, for the statistics panel.
///
/// Every field is counted from the same reconciled snapshot, so the totals
/// agree with the rows on screen rather than describing a slightly different
/// moment.
#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct QueueStats {
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
struct JobDetails {
    job: JobSnapshot,
    segments: Vec<SegmentView>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CancelResponse {
    outcome: &'static str,
    job: JobSnapshot,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PersistedQueue {
    schema_version: u32,
    records: Vec<PersistedRecord>,
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
    view: JobSnapshot,
}

impl DesktopJobs {
    #[cfg(test)]
    fn load(state_path: PathBuf, max_active: usize) -> io::Result<Self> {
        Self::load_with_browser(state_path, max_active, None)
    }

    fn load_with_browser(
        state_path: PathBuf,
        max_active: usize,
        browser_download_dir: Option<PathBuf>,
    ) -> io::Result<Self> {
        let persisted = load_persisted(&state_path)?;
        let now = now_ms();
        let browser_store = state_path
            .parent()
            .map(|parent| BridgeStore::new(parent.to_path_buf()));
        let settings_path = state_path.with_file_name("settings-v1.json");
        let loaded = settings::load(&settings_path);
        let mut stored = loaded.settings;
        // A caller-supplied concurrency (the tests, and the launch default) only
        // applies when the user has never chosen one themselves.
        if !settings_path.exists() {
            stored.max_active_downloads = max_active;
            stored.clamp();
        }
        let media_tools = discover_media_tools(stored.media_tools_dir.as_deref());
        let records = persisted
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
        }
    }

    #[cfg(test)]
    fn in_memory_with_media(max_active: usize, media_tools: MediaTools) -> Self {
        let jobs = Self::in_memory(max_active);
        *jobs.media_tools.lock().expect("media tools poisoned") = Some(media_tools);
        jobs
    }

    fn settings(&self) -> Settings {
        self.settings.lock().expect("settings poisoned").clone()
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
    fn update_settings(&self, mut next: Settings) -> Result<Settings, String> {
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
        let _ = self.save_locked(&state);
        Ok(next)
    }

    fn enqueue(&self, drafts: Vec<JobDraft>) -> Result<Vec<JobSnapshot>, String> {
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
        let mut state = self.inner.lock().expect("desktop jobs poisoned");
        self.reconcile_locked(&mut state);
        let mut destinations = HashSet::new();
        let mut created_ids = Vec::with_capacity(drafts.len());
        let mut batch_directory: Option<PathBuf> = None;

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
            let record = QueueRecord::new_checked(url, destination, draft.not_before_ms, checksum);
            created_ids.push(record.id.clone());
            state.records.push(record);
        }
        self.reconcile_locked(&mut state);
        self.save_locked(&state)?;
        Ok(state
            .records
            .iter()
            .filter(|record| created_ids.contains(&record.id))
            .map(|record| record.view.clone())
            .collect())
    }

    fn inspect_media(&self, source: &str) -> Result<MediaInspection, String> {
        let tools = self.media_tools().ok_or_else(|| {
            "Media tools are not set up yet. Open Settings to install or locate yt-dlp and ffmpeg."
                .to_string()
        })?;
        let source = validated_source(source)?;
        tools.inspect(&source).map_err(|error| error.to_string())
    }

    fn enqueue_media(&self, draft: MediaDraft) -> Result<JobSnapshot, String> {
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
        let record = QueueRecord::new_media(
            url,
            destination,
            draft.not_before_ms,
            draft.variant_id,
            draft.quality_label,
            tools,
        );
        let id = record.id.clone();
        let mut state = self.inner.lock().expect("desktop jobs poisoned");
        self.reconcile_locked(&mut state);
        state.records.push(record);
        self.reconcile_locked(&mut state);
        self.save_locked(&state)?;
        Ok(find_record(&state, &id)?.view.clone())
    }

    fn list(&self) -> Result<Vec<JobSnapshot>, String> {
        let mut state = self.inner.lock().expect("desktop jobs poisoned");
        let processed = self.ingest_browser_locked(&mut state)?;
        self.reconcile_locked(&mut state);
        self.save_locked(&state)?;
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

    fn snapshot(&self, job_id: &str) -> Result<JobSnapshot, String> {
        self.list()?
            .into_iter()
            .find(|job| job.job_id == job_id)
            .ok_or_else(|| "This download is no longer available.".to_string())
    }

    fn details(&self, job_id: &str) -> Result<JobDetails, String> {
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

    fn cancel(&self, job_id: &str) -> Result<CancelResponse, String> {
        let mut state = self.inner.lock().expect("desktop jobs poisoned");
        self.reconcile_locked(&mut state);
        let record = find_record_mut(&mut state, job_id)?;
        let outcome = if let Some(job) = record.job.as_ref() {
            job.cancel()
        } else {
            "already_terminal"
        };
        refresh_record(record);
        self.save_locked(&state)?;
        Ok(CancelResponse {
            outcome,
            job: find_record(&state, job_id)?.view.clone(),
        })
    }

    fn start_now(&self, job_id: &str) -> Result<JobSnapshot, String> {
        let mut state = self.inner.lock().expect("desktop jobs poisoned");
        let record = find_record_mut(&mut state, job_id)?;
        if record.view.state != "scheduled" && record.view.state != "queued" {
            return Err("Only queued or scheduled downloads can start now.".into());
        }
        record.not_before_ms = None;
        record.view.not_before_ms = None;
        record.view.state = "queued".into();
        self.reconcile_locked(&mut state);
        self.save_locked(&state)?;
        Ok(find_record(&state, job_id)?.view.clone())
    }

    fn retry(
        &self,
        job_id: &str,
        url: Option<String>,
        destination: Option<String>,
        checksum: Option<String>,
    ) -> Result<JobSnapshot, String> {
        let checksum = checksum.map(|text| checked_checksum(&text)).transpose()?;
        let mut state = self.inner.lock().expect("desktop jobs poisoned");
        let record = find_record_mut(&mut state, job_id)?;
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
            let url = validated_source(&url)?;
            record.display_url = display_url(&url);
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
            record.destination = validated_destination(&destination)?;
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
        self.reconcile_locked(&mut state);
        self.save_locked(&state)?;
        Ok(find_record(&state, job_id)?.view.clone())
    }

    /// Stops a running download at its last checkpoint so it can continue
    /// later from that offset.
    ///
    /// Pausing races publication. Cancellation is refused once the engine has
    /// committed to publishing, and when that happens the download really did
    /// finish: this reports the completion instead of claiming a paused state
    /// for a file that is already on disk.
    fn pause(&self, job_id: &str) -> Result<JobSnapshot, String> {
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
                    self.save_locked(&state)?;
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
        self.save_locked(&state)?;
        Ok(view)
    }

    /// Returns a paused download to the queue, continuing from its checkpoint.
    fn resume(&self, job_id: &str) -> Result<JobSnapshot, String> {
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
        self.save_locked(&state)?;
        Ok(find_record(&state, job_id)?.view.clone())
    }

    fn remove(&self, job_id: &str) -> Result<(), String> {
        let mut state = self.inner.lock().expect("desktop jobs poisoned");
        let index = state
            .records
            .iter()
            .position(|record| record.id == job_id)
            .ok_or_else(|| "This download is no longer available.".to_string())?;
        let record = state.records.remove(index);
        if let Some(credential_ref) = record.credential_ref.as_deref()
            && let Some(store) = self.browser_store.as_ref()
        {
            let _ = store.remove_secret(credential_ref);
        }
        if let Some(job) = record.job {
            job.cancel();
            job.join();
        }
        self.save_locked(&state)
    }

    fn cancel_all_and_join(&self) {
        let jobs: Vec<(String, JobHandle)> = {
            let state = self.inner.lock().expect("desktop jobs poisoned");
            state
                .records
                .iter()
                .filter(|record| matches!(record.view.state.as_str(), "running" | "cancelling"))
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
        let _ = self.save_locked(&state);
    }

    fn reconcile_locked(&self, state: &mut QueueState) {
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
        if settings.auto_retry {
            self.schedule_automatic_retries(state, &settings);
        }
        let mut active = state
            .records
            .iter()
            .filter(|record| matches!(record.view.state.as_str(), "running" | "cancelling"))
            .count();
        let now = now_ms();
        for record in &mut state.records {
            if active >= max_active {
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
            // specific user action attached, which is exactly the transport case.
            if record.view.action.as_deref() != Some("retry") {
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

    /// Aggregate figures for the statistics panel.
    fn stats(&self) -> QueueStats {
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

    fn save_locked(&self, state: &QueueState) -> Result<(), String> {
        let Some(path) = self.state_path.as_ref() else {
            return Ok(());
        };
        save_persisted(path, state).map_err(|error| {
            format!(
                "Could not save download history at {}: {error}",
                path.display()
            )
        })
    }

    fn ingest_browser_locked(
        &self,
        state: &mut QueueState,
    ) -> Result<Vec<(String, String)>, String> {
        let (Some(store), Some(download_dir)) = (
            self.browser_store.as_ref(),
            self.browser_download_dir.as_ref(),
        ) else {
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
                if !state.link_reviews.contains(&secret.url) {
                    state.link_reviews.push(secret.url);
                }
                processed.push((capture.capture_id, "link-review".into()));
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
        let (job, state, error, action, retryable) = if paused && live_url.is_some() {
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
            view,
        }
    }
}

fn refresh_record(record: &mut QueueRecord) {
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

/// The next step a failure calls for. Only `retry` is eligible for automatic
/// retry, so anything that needs a person must map to something else.
fn action_for_error(error: &str) -> &'static str {
    // A checksum failure needs a person: the source or the checksum is wrong.
    // It is deliberately not "retry", so automatic retry never repeats it.
    if error.contains("checksum_mismatch") || error.contains("checksum_unreadable") {
        "check_checksum"
    } else if error.contains("source_expired") || error.contains("unknown_variant") {
        "refresh_source"
    } else if error.contains("helper_unavailable") {
        "configure_media_tools"
    } else if error.contains("destination_conflict") || error.contains("already exists") {
        "choose_new_path"
    } else if error.contains("invalid_url")
        || error.contains("invalid_destination")
        || refused_by_server(error)
    {
        "edit_link"
    } else {
        "retry"
    }
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

fn save_persisted(path: &Path, state: &QueueState) -> io::Result<()> {
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
                view: record.view.clone(),
            })
            .collect(),
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

fn load_persisted(path: &Path) -> io::Result<Option<PersistedQueue>> {
    let backup = path.with_extension("json.bak");
    for candidate in [path, backup.as_path()] {
        let file = match File::open(candidate) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        let queue: PersistedQueue = match serde_json::from_reader(file) {
            Ok(queue) => queue,
            Err(_) => continue,
        };
        if queue.schema_version == QUEUE_SCHEMA_VERSION {
            return Ok(Some(queue));
        }
    }
    Ok(None)
}

#[tauri::command]
fn start_download(
    url: String,
    destination: String,
    jobs: State<'_, DesktopJobs>,
) -> Result<JobSnapshot, String> {
    jobs.enqueue(vec![JobDraft {
        url,
        destination,
        not_before_ms: None,
        checksum: None,
    }])?
    .into_iter()
    .next()
    .ok_or_else(|| "The download was not queued.".to_string())
}

#[tauri::command]
fn start_batch(
    drafts: Vec<JobDraft>,
    jobs: State<'_, DesktopJobs>,
) -> Result<Vec<JobSnapshot>, String> {
    jobs.enqueue(drafts)
}

#[tauri::command]
fn inspect_media(url: String, jobs: State<'_, DesktopJobs>) -> Result<MediaInspection, String> {
    jobs.inspect_media(&url)
}

#[tauri::command]
fn start_media_download(
    draft: MediaDraft,
    jobs: State<'_, DesktopJobs>,
) -> Result<JobSnapshot, String> {
    jobs.enqueue_media(draft)
}

#[tauri::command]
fn list_downloads(jobs: State<'_, DesktopJobs>) -> Result<Vec<JobSnapshot>, String> {
    jobs.list()
}

/// Media pages sent from the browser, taken once each by the interface.
#[tauri::command]
fn take_link_reviews(jobs: State<'_, DesktopJobs>) -> Vec<String> {
    std::mem::take(
        &mut jobs
            .inner
            .lock()
            .expect("desktop jobs poisoned")
            .link_reviews,
    )
}

#[tauri::command]
fn get_download(job_id: String, jobs: State<'_, DesktopJobs>) -> Result<JobSnapshot, String> {
    jobs.snapshot(&job_id)
}

#[tauri::command]
fn download_details(job_id: String, jobs: State<'_, DesktopJobs>) -> Result<JobDetails, String> {
    jobs.details(&job_id)
}

#[tauri::command]
fn cancel_download(job_id: String, jobs: State<'_, DesktopJobs>) -> Result<CancelResponse, String> {
    jobs.cancel(&job_id)
}

#[tauri::command]
fn start_now(job_id: String, jobs: State<'_, DesktopJobs>) -> Result<JobSnapshot, String> {
    jobs.start_now(&job_id)
}

#[tauri::command]
fn retry_download(
    job_id: String,
    url: Option<String>,
    destination: Option<String>,
    checksum: Option<String>,
    jobs: State<'_, DesktopJobs>,
) -> Result<JobSnapshot, String> {
    jobs.retry(&job_id, url, destination, checksum)
}

#[tauri::command]
fn pause_download(job_id: String, jobs: State<'_, DesktopJobs>) -> Result<JobSnapshot, String> {
    jobs.pause(&job_id)
}

#[tauri::command]
fn resume_download(job_id: String, jobs: State<'_, DesktopJobs>) -> Result<JobSnapshot, String> {
    jobs.resume(&job_id)
}

#[tauri::command]
fn queue_stats(jobs: State<'_, DesktopJobs>) -> Result<QueueStats, String> {
    Ok(jobs.stats())
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SettingsView {
    settings: Settings,
    /// True when the stored settings file was unusable and defaults were
    /// substituted, so the interface can say so instead of presenting the
    /// defaults as the user's own choices.
    repaired: bool,
    max_active_limit: usize,
    max_retry_attempts: u32,
    /// The folder used when no default destination is set.
    system_download_dir: Option<String>,
}

#[tauri::command]
fn get_settings(app: AppHandle, jobs: State<'_, DesktopJobs>) -> Result<SettingsView, String> {
    Ok(SettingsView {
        settings: jobs.settings(),
        repaired: jobs.settings_repaired,
        max_active_limit: settings::MAX_ACTIVE_DOWNLOADS,
        max_retry_attempts: settings::MAX_RETRY_ATTEMPTS,
        system_download_dir: app
            .path()
            .download_dir()
            .ok()
            .map(|dir| dir.display().to_string()),
    })
}

#[tauri::command]
fn update_settings(
    next: Settings,
    app: AppHandle,
    jobs: State<'_, DesktopJobs>,
) -> Result<SettingsView, String> {
    jobs.update_settings(next)?;
    get_settings(app, jobs)
}

/// The folder new downloads are saved in: the user's choice, else Windows'
/// own Downloads folder.
#[tauri::command]
fn default_destination_dir(
    app: AppHandle,
    jobs: State<'_, DesktopJobs>,
) -> Result<Option<String>, String> {
    Ok(jobs.settings().default_destination_dir.or_else(|| {
        app.path()
            .download_dir()
            .ok()
            .map(|dir| dir.display().to_string())
    }))
}

/// Opens Explorer with the finished file selected.
///
/// Only ever points at a destination this queue recorded, so a renderer message
/// cannot turn this into a way to launch an arbitrary path.
#[tauri::command]
fn reveal_download(job_id: String, jobs: State<'_, DesktopJobs>) -> Result<(), String> {
    let snapshot = jobs.snapshot(&job_id)?;
    let destination = snapshot
        .destination
        .ok_or_else(|| "This download has no saved file yet.".to_string())?;
    let path = PathBuf::from(&destination);
    if !path.exists() {
        return Err(format!("{destination} is no longer on disk."));
    }
    // Explorer parses its own command line rather than using the standard
    // argv rules, and `/select,` with the path must arrive as one unquoted
    // token followed by a quoted path. Passing it through `arg` lets Rust
    // quote the whole `/select,C:\Some Folder\file` string, which Explorer
    // then fails to split and answers by opening Documents instead. `raw_arg`
    // writes the command line exactly.
    //
    // A quote inside the path would escape the quoting below, so it is refused.
    // `validated_destination` already rejects one, which makes this a second
    // fence rather than the only one.
    if destination.contains('"') {
        return Err("That destination cannot be shown in File Explorer.".into());
    }
    std::os::windows::process::CommandExt::raw_arg(
        &mut std::process::Command::new("explorer.exe"),
        format!("/select,\"{}\"", path.display()),
    )
    .stdin(std::process::Stdio::null())
    .stdout(std::process::Stdio::null())
    .stderr(std::process::Stdio::null())
    .spawn()
    .map(|_| ())
    .map_err(|error| format!("Could not open the folder: {error}"))
}

#[tauri::command]
fn media_tools_status(
    app: AppHandle,
    jobs: State<'_, DesktopJobs>,
) -> Result<media_setup::ToolsStatus, String> {
    let install_dir = media_tools_install_dir(&app)?;
    Ok(media_setup::status(
        jobs.settings().media_tools_dir.as_deref(),
        &install_dir,
    ))
}

#[tauri::command]
fn install_media_tools(
    app: AppHandle,
    jobs: State<'_, DesktopJobs>,
) -> Result<media_setup::ToolsStatus, String> {
    let install_dir = media_tools_install_dir(&app)?;
    media_setup::install(&install_dir)?;
    let mut settings = jobs.settings();
    settings.media_tools_dir = Some(install_dir.display().to_string());
    jobs.update_settings(settings)?;
    media_tools_status(app, jobs)
}

#[tauri::command]
fn use_media_tools_dir(
    directory: String,
    app: AppHandle,
    jobs: State<'_, DesktopJobs>,
) -> Result<media_setup::ToolsStatus, String> {
    if directory.len() > MAX_DESTINATION_LENGTH {
        return Err("That folder path is too long.".into());
    }
    let accepted = media_setup::use_directory(&PathBuf::from(&directory))?;
    let mut settings = jobs.settings();
    settings.media_tools_dir = Some(accepted);
    jobs.update_settings(settings)?;
    media_tools_status(app, jobs)
}

/// Where the `fetchpath` command is, when the installer put it beside the app.
#[tauri::command]
fn cli_path() -> Option<String> {
    let cli = std::env::current_exe()
        .ok()?
        .parent()?
        .join("fetchpath.exe");
    cli.is_file().then(|| cli.display().to_string())
}

#[tauri::command]
fn browser_setup_status(app: AppHandle) -> browser_setup::BrowserSetupStatus {
    browser_setup::status(app.path().resource_dir().ok().as_deref())
}

#[tauri::command]
fn reveal_extension_folder(app: AppHandle) -> Result<(), String> {
    browser_setup::reveal_extension_folder(app.path().resource_dir().ok().as_deref())
}

fn media_tools_install_dir(app: &AppHandle) -> Result<PathBuf, String> {
    app.path()
        .app_data_dir()
        .map(|dir| dir.join("media-tools"))
        .map_err(|error| format!("Could not resolve the Fetchpath data folder: {error}"))
}

#[tauri::command]
fn remove_download(job_id: String, jobs: State<'_, DesktopJobs>) -> Result<(), String> {
    jobs.remove(&job_id)
}

/// One Fetchpath process per Windows user account.
///
/// Two processes would both own `queue-v1.json` and both claim the tray icon, so
/// the second one would race the first over the persisted queue. The guard is a
/// deny-sharing handle on a lock file in the same application-data directory as
/// the queue: whoever opens it first keeps it for the life of the process.
///
/// It fails open. Only a sharing or locking violation means "another instance is
/// running"; any other error (an unwritable directory, a missing `%APPDATA%`)
/// lets the application start, because refusing to launch is worse than the
/// unlikely double-launch it would prevent.
mod single_instance {
    use std::fs::{File, OpenOptions};
    use std::os::windows::fs::OpenOptionsExt;
    use std::path::{Path, PathBuf};
    use std::sync::OnceLock;

    /// `FILE_SHARE_NONE`: no other process may open this file at all.
    const NO_SHARING: u32 = 0;
    const ERROR_SHARING_VIOLATION: i32 = 32;
    const ERROR_LOCK_VIOLATION: i32 = 33;
    const SW_SHOW: i32 = 5;
    const SW_RESTORE: i32 = 9;
    /// The window class tao registers for a Tauri window. Matching on it as well
    /// as the title avoids activating some unrelated window that happens to be
    /// called "Fetchpath" — an Explorer window on a folder of that name, say.
    const WINDOW_CLASS: &str = "Tauri Window";

    /// Held for the lifetime of the process; dropping it would release the claim.
    static HELD: OnceLock<File> = OnceLock::new();

    #[link(name = "user32")]
    unsafe extern "system" {
        fn FindWindowW(class_name: *const u16, window_name: *const u16) -> *mut core::ffi::c_void;
        fn ShowWindow(window: *mut core::ffi::c_void, command: i32) -> i32;
        fn SetForegroundWindow(window: *mut core::ffi::c_void) -> i32;
    }

    pub fn lock_path() -> Option<PathBuf> {
        let roaming = std::env::var_os("APPDATA")?;
        Some(
            Path::new(&roaming)
                .join("app.fetchpath.desktop")
                .join("instance.lock"),
        )
    }

    /// `true` when this process may proceed as the single instance.
    pub fn claim(path: &Path) -> bool {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .share_mode(NO_SHARING)
            .open(path)
        {
            Ok(file) => {
                let _ = HELD.set(file);
                true
            }
            Err(error) => !matches!(
                error.raw_os_error(),
                Some(ERROR_SHARING_VIOLATION) | Some(ERROR_LOCK_VIOLATION)
            ),
        }
    }

    fn wide(value: &str) -> Vec<u16> {
        let mut buffer: Vec<u16> = value.encode_utf16().collect();
        buffer.push(0);
        buffer
    }

    /// Brings the already-running window forward, including from the tray, so a
    /// second launch looks like reopening Fetchpath rather than doing nothing.
    pub fn activate_running_window(title: &str) {
        let class = wide(WINDOW_CLASS);
        let name = wide(title);
        // SAFETY: both buffers are NUL-terminated UTF-16 and outlive the call.
        unsafe {
            let window = FindWindowW(class.as_ptr(), name.as_ptr());
            if window.is_null() {
                return;
            }
            // Hidden-to-tray needs SW_SHOW; minimized needs SW_RESTORE.
            ShowWindow(window, SW_SHOW);
            ShowWindow(window, SW_RESTORE);
            SetForegroundWindow(window);
        }
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    if let Some(path) = single_instance::lock_path()
        && !single_instance::claim(&path)
    {
        single_instance::activate_running_window("Fetchpath");
        return;
    }

    let app = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            start_download,
            start_batch,
            inspect_media,
            start_media_download,
            list_downloads,
            take_link_reviews,
            get_download,
            download_details,
            cancel_download,
            start_now,
            retry_download,
            remove_download,
            pause_download,
            resume_download,
            queue_stats,
            get_settings,
            update_settings,
            default_destination_dir,
            reveal_download,
            media_tools_status,
            install_media_tools,
            use_media_tools_dir,
            browser_setup_status,
            cli_path,
            reveal_extension_folder
        ])
        .setup(|app| {
            let state_path = app.path().app_data_dir()?.join("queue-v1.json");
            let download_dir = app.path().download_dir()?;
            app.manage(DesktopJobs::load_with_browser(
                state_path,
                DEFAULT_MAX_ACTIVE,
                Some(download_dir),
            )?);
            let show = MenuItem::with_id(app, "show", "Show Fetchpath", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "Quit Fetchpath", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show, &quit])?;
            let tray = TrayIconBuilder::new()
                .icon(app.default_window_icon().expect("app icon").clone())
                .tooltip("Fetchpath")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "show" => show_main_window(app),
                    "quit" => {
                        app.state::<DesktopJobs>().cancel_all_and_join();
                        app.exit(0);
                    }
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        show_main_window(tray.app_handle());
                    }
                })
                .build(app)?;
            app.manage(tray);
            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                // Closing to the tray is the default because a running queue
                // should survive a stray click on the X. A user who turned that
                // off means the close button to close, so finish the transfers
                // down cleanly and exit rather than hiding.
                let jobs = window.app_handle().state::<DesktopJobs>();
                if jobs.settings().close_to_tray {
                    api.prevent_close();
                    let _ = window.hide();
                } else {
                    jobs.cancel_all_and_join();
                    window.app_handle().exit(0);
                }
            }
        })
        .build(tauri::generate_context!())
        .expect("error while building Fetchpath");

    app.run(|_, event| {
        if let tauri::RunEvent::ExitRequested {
            code: None, api, ..
        } = event
        {
            api.prevent_exit();
        }
    });
}

fn show_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.set_focus();
    }
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
    fn wait_for_state(jobs: &DesktopJobs, job_id: &str, states: &[&str]) -> JobSnapshot {
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

    fn wait_for_terminal(jobs: &DesktopJobs, job_id: &str) -> JobSnapshot {
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
        let jobs = DesktopJobs::in_memory(3);
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
        let jobs = DesktopJobs::in_memory(3);
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
        let jobs = DesktopJobs::in_memory(3);
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
        let jobs = DesktopJobs::in_memory(0);
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
        let jobs = DesktopJobs::in_memory(3);
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
        let jobs = DesktopJobs::in_memory(3);
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
        let jobs = DesktopJobs::in_memory(3);
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

        let jobs = DesktopJobs::in_memory(3);
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
        let jobs = DesktopJobs::in_memory(1);
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
            let jobs = DesktopJobs::load(state_path.clone(), 1).unwrap();
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
        let reopened = DesktopJobs::load(state_path, 1).unwrap();
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
        let jobs = DesktopJobs::in_memory(1);
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
        let jobs = DesktopJobs::load(dir.path().join("queue-v1.json"), 3).unwrap();
        assert_eq!(jobs.max_active(), 3);

        let mut next = jobs.settings();
        next.max_active_downloads = 999;
        let applied = jobs.update_settings(next).unwrap();
        assert_eq!(applied.max_active_downloads, settings::MAX_ACTIVE_DOWNLOADS);
        assert_eq!(jobs.max_active(), settings::MAX_ACTIVE_DOWNLOADS);

        // And the choice survives a restart.
        let reopened = DesktopJobs::load(dir.path().join("queue-v1.json"), 3).unwrap();
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
        let jobs = DesktopJobs::in_memory(1);
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
        let jobs = DesktopJobs::in_memory(3);
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
        let jobs = DesktopJobs::in_memory(1);
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

        let recovered = DesktopJobs::load(state_path, 1).unwrap().list().unwrap();
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
        let jobs = DesktopJobs::in_memory(1);
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
        let jobs = DesktopJobs::in_memory(1);
        assert!(
            jobs.enqueue(Vec::new())
                .unwrap_err()
                .contains("at least one")
        );
        assert!(jobs.snapshot("missing").unwrap_err().contains("no longer"));
    }

    #[test]
    fn browser_inbox_is_consumed_once_into_the_persistent_queue() {
        use crate::browser_bridge::{BrowserCookie, CaptureRequest, SCHEMA_VERSION};

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
            DesktopJobs::load_with_browser(state_path.clone(), 1, Some(download_dir.clone()))
                .unwrap();
        let queued = jobs.list().unwrap();
        assert_eq!(queued.len(), 1);
        let completed = wait_for_terminal(&jobs, &queued[0].job_id);
        server.join().unwrap();
        assert_eq!(completed.state, "completed");
        assert_eq!(fs::read(download_dir.join("archive.bin")).unwrap(), body);
        assert!(store.pending().unwrap().is_empty());
        let persisted = fs::read_to_string(&state_path).unwrap();
        assert!(!persisted.contains("token=private"));

        let restored = DesktopJobs::load_with_browser(state_path, 1, Some(download_dir))
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
        use crate::browser_bridge::{BrowserCookie, CaptureRequest, SCHEMA_VERSION};

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

        let jobs =
            DesktopJobs::load_with_browser(dir.path().join("queue-v1.json"), 1, Some(download_dir))
                .unwrap();
        assert!(jobs.list().unwrap().is_empty(), "not queued as a file");
        assert!(
            store.pending().unwrap().is_empty(),
            "the capture is consumed"
        );
        let reviews = std::mem::take(&mut jobs.inner.lock().unwrap().link_reviews);
        assert_eq!(reviews, vec![page.to_owned()]);
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
        let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../../adapters/media/tests/fixtures");
        let tools = MediaTools::new(
            fixtures.join("expired-helper.cmd"),
            &fixtures,
            fixtures.join("fake-ffprobe.cmd"),
        )
        .unwrap();
        let output = tempfile::tempdir().unwrap();
        let destination = output.path().join("media.mp4");
        let jobs = DesktopJobs::in_memory_with_media(1, tools.clone());
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
        let jobs = DesktopJobs::in_memory(1);

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
        let jobs = DesktopJobs::in_memory(1);

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
        let jobs = DesktopJobs::in_memory(1);

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

    #[test]
    fn a_second_instance_is_refused_while_the_first_holds_the_lock() {
        use std::fs::OpenOptions;
        use std::os::windows::fs::OpenOptionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("instance.lock");

        // The first claim also creates the application-data directory.
        assert!(single_instance::claim(&path));
        assert!(path.exists());

        // A second process is modelled by an independent deny-sharing open of
        // the same path, because the in-process claim is held by a static.
        let contended = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .share_mode(0)
            .open(&path);
        assert_eq!(
            contended.unwrap_err().raw_os_error(),
            Some(32),
            "a second instance must see ERROR_SHARING_VIOLATION"
        );

        // Failing open: an unusable lock path must not stop the application. A
        // regular file makes an impossible parent directory.
        let blocker = dir.path().join("blocker");
        fs::write(&blocker, b"not a directory").unwrap();
        let unusable = blocker.join("child.lock");
        assert!(single_instance::claim(&unusable));
        assert!(!unusable.exists());
    }
}
