pub mod browser_bridge;

use browser_bridge::BridgeStore;
use fetchpath_core::{CancelResult, FileJob, FileJobState, RequestContext};
use fetchpath_media::{MediaInspection, MediaJob, MediaJobState, MediaTools};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, State, WindowEvent};

const QUEUE_SCHEMA_VERSION: u32 = 1;
const DEFAULT_MAX_ACTIVE: usize = 3;

struct DesktopJobs {
    inner: Mutex<QueueState>,
    state_path: Option<PathBuf>,
    max_active: usize,
    browser_store: Option<BridgeStore>,
    browser_download_dir: Option<PathBuf>,
    media_tools: Option<MediaTools>,
}

#[derive(Default)]
struct QueueState {
    records: Vec<QueueRecord>,
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
    view: JobSnapshot,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct JobDraft {
    url: String,
    destination: String,
    not_before_ms: Option<u64>,
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
    destination: Option<String>,
    observed_sha256: Option<String>,
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
        let media_tools = MediaTools::discover().ok();
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
            inner: Mutex::new(QueueState { records }),
            state_path: Some(state_path),
            max_active: max_active.max(1),
            browser_store,
            browser_download_dir,
            media_tools,
        })
    }

    #[cfg(test)]
    fn in_memory(max_active: usize) -> Self {
        Self {
            inner: Mutex::new(QueueState::default()),
            state_path: None,
            max_active: max_active.max(1),
            browser_store: None,
            browser_download_dir: None,
            media_tools: None,
        }
    }

    #[cfg(test)]
    fn in_memory_with_media(max_active: usize, media_tools: MediaTools) -> Self {
        let mut jobs = Self::in_memory(max_active);
        jobs.media_tools = Some(media_tools);
        jobs
    }

    fn enqueue(&self, drafts: Vec<JobDraft>) -> Result<Vec<JobSnapshot>, String> {
        if drafts.is_empty() {
            return Err("Add at least one download address.".into());
        }
        if drafts.len() > 100 {
            return Err("A batch can contain at most 100 downloads.".into());
        }
        let mut state = self.inner.lock().expect("desktop jobs poisoned");
        self.reconcile_locked(&mut state);
        let mut destinations = HashSet::new();
        let mut created_ids = Vec::with_capacity(drafts.len());

        for draft in drafts {
            let url = draft.url.trim();
            let destination = PathBuf::from(draft.destination.trim());
            if !(url.starts_with("http://") || url.starts_with("https://")) {
                return Err(format!(
                    "{} is not an HTTP or HTTPS address.",
                    display_url(url)
                ));
            }
            if destination.file_name().is_none() {
                return Err("Every queued download needs a destination filename.".into());
            }
            if !destinations.insert(destination.clone()) {
                return Err(format!(
                    "The batch contains the destination {} more than once.",
                    destination.display()
                ));
            }
            let record = QueueRecord::new(url.to_owned(), destination, draft.not_before_ms);
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
        let tools = self.media_tools.as_ref().ok_or_else(|| {
            "Media tools are unavailable. Set FETCHPATH_YT_DLP and FETCHPATH_FFMPEG_DIR, then restart Fetchpath."
                .to_string()
        })?;
        tools
            .inspect(source.trim())
            .map_err(|error| error.to_string())
    }

    fn enqueue_media(&self, draft: MediaDraft) -> Result<JobSnapshot, String> {
        let tools = self.media_tools.clone().ok_or_else(|| {
            "Media tools are unavailable. Set FETCHPATH_YT_DLP and FETCHPATH_FFMPEG_DIR, then restart Fetchpath."
                .to_string()
        })?;
        let url = draft.url.trim().to_owned();
        let destination = PathBuf::from(draft.destination.trim());
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            return Err("Enter an HTTP or HTTPS media address.".into());
        }
        if destination.file_name().is_none() {
            return Err("Choose a destination filename.".into());
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
    ) -> Result<JobSnapshot, String> {
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
            let url = url.trim().to_owned();
            if !(url.starts_with("http://") || url.starts_with("https://")) {
                return Err("Enter an HTTP or HTTPS address.".into());
            }
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
            let destination = PathBuf::from(destination.trim());
            if destination.file_name().is_none() {
                return Err("Choose a destination filename.".into());
            }
            record.destination = destination;
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
                let tools = self.media_tools.clone().ok_or_else(|| {
                    "Media tools are unavailable. Configure them before retrying.".to_string()
                })?;
                JobHandle::Media(MediaJob::create(
                    live_url,
                    variant_id,
                    record.destination.clone(),
                    tools,
                ))
            } else {
                JobHandle::File(FileJob::create_recoverable_with_context(
                    live_url,
                    record.destination.clone(),
                    record.live_context.clone(),
                ))
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
                    self.media_tools.clone().map(|tools| {
                        JobHandle::Media(MediaJob::create(
                            url,
                            variant_id,
                            record.destination.clone(),
                            tools,
                        ))
                    })
                } else {
                    Some(JobHandle::File(FileJob::create_recoverable_with_context(
                        url,
                        record.destination.clone(),
                        record.live_context.clone(),
                    )))
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
        for record in &mut state.records {
            refresh_record(record);
            if record.view.state == "completed"
                && let Some(credential_ref) = record.credential_ref.take()
                && let Some(store) = self.browser_store.as_ref()
            {
                let _ = store.remove_secret(&credential_ref);
            }
        }
        let mut active = state
            .records
            .iter()
            .filter(|record| matches!(record.view.state.as_str(), "running" | "cancelling"))
            .count();
        let now = now_ms();
        for record in &mut state.records {
            if active >= self.max_active {
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
            );
            let job_id = record.id.clone();
            state.records.push(record);
            processed.push((capture.capture_id, job_id));
        }
        Ok(processed)
    }
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
    fn new(url: String, destination: PathBuf, not_before_ms: Option<u64>) -> Self {
        Self::new_with_context(
            url,
            destination,
            not_before_ms,
            None,
            RequestContext::default(),
        )
    }

    fn new_with_context(
        url: String,
        destination: PathBuf,
        not_before_ms: Option<u64>,
        credential_ref: Option<String>,
        live_context: RequestContext,
    ) -> Self {
        let now = now_ms();
        let id = uuid::Uuid::new_v4().to_string();
        let display = display_url(&url);
        let restart_url = restartable_url(&url);
        let conflict = destination.exists();
        let scheduled = not_before_ms.is_some_and(|due| due > now);
        let job = (!conflict).then(|| {
            JobHandle::File(FileJob::create_recoverable_with_context(
                url.clone(),
                destination.clone(),
                live_context.clone(),
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
            view: JobSnapshot {
                job_id: id,
                source: display,
                state: state.into(),
                bytes_received: 0,
                destination: Some(destination.display().to_string()),
                observed_sha256: None,
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
            view: JobSnapshot {
                job_id: id,
                source: display,
                state: state.into(),
                bytes_received: 0,
                destination: Some(destination.display().to_string()),
                observed_sha256: None,
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
        let (job, state, error, action, retryable) = if private_source_needs_refresh {
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
                Some(JobHandle::File(FileJob::create_recoverable_with_context(
                    url.clone(),
                    PathBuf::from(&saved.destination),
                    live_context.clone(),
                )))
            };
            if saved.media_variant_id.is_some() && job.is_none() {
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
            view,
        }
    }
}

fn refresh_record(record: &mut QueueRecord) {
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
}

fn recovery_action(snapshot: &JobSnapshot) -> (Option<String>, bool) {
    if snapshot.state != "failed" {
        return (None, false);
    }
    let error = snapshot.error.as_deref().unwrap_or_default();
    if error.contains("source_expired") || error.contains("unknown_variant") {
        (Some("refresh_source".into()), true)
    } else if error.contains("helper_unavailable") {
        (Some("configure_media_tools".into()), true)
    } else if error.contains("destination_conflict") || error.contains("already exists") {
        (Some("choose_new_path".into()), true)
    } else if error.contains("invalid_url") || error.contains("invalid_destination") {
        (Some("edit_link".into()), true)
    } else {
        (Some("retry".into()), true)
    }
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
    if base == url {
        base.to_owned()
    } else {
        format!("{base}?…")
    }
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

#[tauri::command]
fn get_download(job_id: String, jobs: State<'_, DesktopJobs>) -> Result<JobSnapshot, String> {
    jobs.snapshot(&job_id)
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
    jobs: State<'_, DesktopJobs>,
) -> Result<JobSnapshot, String> {
    jobs.retry(&job_id, url, destination)
}

#[tauri::command]
fn remove_download(job_id: String, jobs: State<'_, DesktopJobs>) -> Result<(), String> {
    jobs.remove(&job_id)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            start_download,
            start_batch,
            inspect_media,
            start_media_download,
            list_downloads,
            get_download,
            cancel_download,
            start_now,
            retry_download,
            remove_download
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
                api.prevent_close();
                let _ = window.hide();
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
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;
    use std::time::Duration;

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

    #[test]
    fn production_command_path_downloads_real_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let destination = dir.path().join("desktop.bin");
        let body = vec![0x5a; 128 * 1024];
        let (url, server) = fixture(body.clone(), false);
        let jobs = DesktopJobs::in_memory(3);
        let started = jobs
            .enqueue(vec![JobDraft {
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
                    url: first_url,
                    destination: dir.path().join("first.bin").display().to_string(),
                    not_before_ms: None,
                },
                JobDraft {
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
}
