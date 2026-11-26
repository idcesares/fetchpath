//! Per-job runtime handles, queue records, state refreshes, and failure classification.

use crate::browser_inbox::BridgeStore;
use crate::durable::{RecordDurable, Reported};
use crate::persist::PersistedRecord;
use crate::policy;
use crate::rate::RateEstimate;
use crate::types::{APPROVAL_EXPIRY_MS, Approval, JobSnapshot, Origin, TorrentPolicy};
use crate::validation::{
    display_url, published_inside, restartable_url, torrent_metadata_path, verify_torrent_metadata,
};
use crate::{QueueState, ReconcileWake};
use fetchpath_core::{CancelResult, FileJob, FileJobState, RequestContext, normalize_sha256};
use fetchpath_media::{MediaJob, MediaJobState, MediaTools};
use fetchpath_protocol::principal::{ApprovalReason, Principal};
use fetchpath_torrent::{JobState as TorrentJobState, TorrentJob};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone)]
pub(crate) enum JobHandle {
    File(FileJob),
    Media(MediaJob),
    Torrent(TorrentJob),
}

impl JobHandle {
    pub(crate) fn start(&self, wake: Arc<ReconcileWake>) -> Result<(), &'static str> {
        match self {
            Self::File(job) => job.start_with_completion(move || wake.notify()),
            Self::Media(job) => job.start_with_completion(move || wake.notify()),
            Self::Torrent(job) => job.start_with_completion(move || wake.notify()),
        }
    }

    pub(crate) fn cancel(&self) -> &'static str {
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

    pub(crate) fn join(&self) {
        match self {
            Self::File(job) => job.join(),
            Self::Media(job) => job.join(),
            Self::Torrent(job) => job.join(),
        }
    }

    pub(crate) fn completed(&self) -> bool {
        match self {
            Self::File(job) => job.snapshot().state == FileJobState::Completed,
            Self::Media(job) => job.snapshot().state == MediaJobState::Completed,
            Self::Torrent(job) => job.snapshot().state == TorrentJobState::Completed,
        }
    }
}

pub(crate) struct QueueRecord {
    pub(crate) id: String,
    pub(crate) live_url: Option<String>,
    pub(crate) live_context: RequestContext,
    pub(crate) credential_ref: Option<String>,
    pub(crate) restart_url: Option<String>,
    pub(crate) display_url: String,
    pub(crate) destination: PathBuf,
    pub(crate) not_before_ms: Option<u64>,
    pub(crate) created_at_ms: u64,
    pub(crate) finished_at_ms: Option<u64>,
    pub(crate) media_variant_id: Option<String>,
    pub(crate) media_quality: Option<String>,
    pub(crate) torrent_policy: Option<TorrentPolicy>,
    pub(crate) torrent_metadata_sha256: Option<String>,
    pub(crate) torrent_metadata_path: Option<PathBuf>,
    /// A torrent whose `destination` is still the root folder: the helper names
    /// the child folder from metadata and `destination` becomes that folder once
    /// it is published.
    pub(crate) torrent_auto: bool,
    pub(crate) job: Option<JobHandle>,
    /// Live only. Deliberately not persisted: a rate measured before a restart
    /// describes a transfer that is no longer running.
    pub(crate) rate: RateEstimate,
    /// Automatic retries already spent on this download.
    pub(crate) attempt: u32,
    /// When an automatic retry is due, for the row to show and reconcile to act on.
    pub(crate) retry_at_ms: Option<u64>,
    /// Revision, event sequence and last durable report (FP-051).
    pub(crate) durable: RecordDurable,
    /// Who created the job (contract D1).
    pub(crate) principal: Principal,
    /// A wait for the person's decision, or its refusal.
    pub(crate) approval: Option<Approval>,
    /// The person approved this job past its agent's size limit.
    pub(crate) size_approved: bool,
    /// The person approved this job outside its agent's granted folders.
    pub(crate) folder_approved: bool,
    /// The job never used a cookie, a stored credential, a referer or link
    /// credentials, so its output may be offered to another principal
    /// (contract D8). Fixed when the job is made; a record saved before this
    /// existed, and every job of another kind, reads false.
    pub(crate) public_origin: bool,
    /// The output of a credentialed job carries its marker (contract D8,
    /// FP-121). Persisted with the row; marking repeats until it holds.
    pub(crate) provenance_marked: bool,
    pub(crate) view: JobSnapshot,
}

impl QueueRecord {
    /// Records who asked for the job and holds it for approval when needed.
    pub(crate) fn apply(&mut self, origin: &Origin) {
        self.principal = origin.principal.clone();
        if !origin.approval.is_empty() {
            self.hold(origin.approval.clone());
        }
    }

    /// Puts the job in `awaiting_approval`. Whatever was prepared for it
    /// stays unstarted until the person decides.
    pub(crate) fn hold(&mut self, reasons: Vec<ApprovalReason>) {
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
    pub(crate) fn awaiting_approval(&self) -> bool {
        self.approval
            .as_ref()
            .is_some_and(|approval| !approval.denied && !approval.withdrawn && !approval.expired)
    }

    /// Waiting for the person for longer than the expiry period at `now`.
    pub(crate) fn approval_expired_at(&self, now: u64) -> bool {
        self.awaiting_approval()
            && self.approval.as_ref().is_some_and(|approval| {
                let since = approval.requested_at_ms.unwrap_or(self.created_at_ms);
                now.saturating_sub(since) >= APPROVAL_EXPIRY_MS
            })
    }
}

impl QueueRecord {
    #[cfg(test)]
    pub(crate) fn new(url: String, destination: PathBuf, not_before_ms: Option<u64>) -> Self {
        Self::new_checked(url, destination, not_before_ms, None)
    }

    /// A record for a pasted link. `expected_sha256` must already have been
    /// normalized by [`checked_checksum`].
    pub(crate) fn new_checked(
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

    pub(crate) fn new_with_context(
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
        let public_origin = credential_ref.is_none()
            && live_context.is_credential_free()
            && !policy::has_userinfo(&url);
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
            folder_approved: false,
            public_origin,
            provenance_marked: false,
            view: JobSnapshot {
                waiting_for_space: false,
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

    pub(crate) fn new_media(
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
            folder_approved: false,
            public_origin: false,
            provenance_marked: false,
            view: JobSnapshot {
                waiting_for_space: false,
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

    pub(crate) fn new_torrent(
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
    pub(crate) fn grant_path(&self) -> PathBuf {
        if self.torrent_auto {
            self.destination.join("torrent")
        } else {
            self.destination.clone()
        }
    }

    /// An existing destination blocks a new download, except the root of an
    /// automatic torrent, which is meant to exist.
    pub(crate) fn destination_taken(&self) -> bool {
        !self.torrent_auto && self.destination.exists()
    }

    pub(crate) fn restore(
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
                waiting_for_space: false,
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
            folder_approved: saved.folder_approved,
            public_origin: saved.public_origin,
            provenance_marked: saved.provenance_marked,
            view,
        }
    }

    /// A record from a newer build's queue, only to be shown (FP-070): no
    /// download is prepared, no browser secret opened, and its saved state
    /// is kept as that build wrote it.
    pub(crate) fn shown(saved: PersistedRecord) -> Self {
        let mut view = saved.view;
        view.bytes_per_second = None;
        view.eta_seconds = None;
        view.error_code = failure_code(&view);
        let mut durable = saved.durable;
        if durable.reported.is_none() {
            durable.reported = Some(Reported {
                state: view.state.clone(),
                not_before_ms: saved.not_before_ms,
                waiting_for_space: false,
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
            folder_approved: saved.folder_approved,
            public_origin: saved.public_origin,
            provenance_marked: saved.provenance_marked,
            view,
        }
    }
}

pub(crate) fn refresh_record(record: &mut QueueRecord) {
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
            // Show the published folder immediately, but retain the original
            // request identity until completion. A save between Published and
            // Completed must recover with the auto root and its marker path.
            if let Some(published) = snapshot.published
                && record.torrent_auto
            {
                if published_inside(&record.destination, Path::new(&published)) {
                    if snapshot.state == TorrentJobState::Completed {
                        record.destination = PathBuf::from(&published);
                        record.torrent_auto = false;
                    }
                    record.view.destination = Some(published);
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
pub(crate) fn sample_rate(record: &mut QueueRecord) {
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

pub(crate) fn has_checksum(text: Option<&str>) -> bool {
    text.is_some_and(|text| !text.trim().is_empty())
}

/// Normalizes a pasted checksum. Blank means none; anything else must be a
/// SHA-256, and a person gets a plain explanation when it is not.
pub(crate) fn checked_checksum(text: &str) -> Result<Option<String>, String> {
    if text.trim().is_empty() {
        return Ok(None);
    }
    normalize_sha256(text).map(Some).ok_or_else(|| {
        "That checksum isn't a SHA-256. It should be 64 characters, each 0-9 or a-f.".to_string()
    })
}

pub(crate) const UNREADABLE_CHECKSUM: &str = "integrity.checksum_unreadable: the checksum saved with this download cannot be read, so it will not be downloaded unchecked. Remove it and add it again.";

/// Creates a file job that enforces the record's checksum when it has one.
///
/// Fails closed: a checksum that cannot be read never produces a job that
/// downloads without it.
pub(crate) fn file_job(
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

pub(crate) fn torrent_job(
    record: &QueueRecord,
    source: String,
    policy: TorrentPolicy,
) -> JobHandle {
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

pub(crate) fn recovery_action(snapshot: &JobSnapshot) -> (Option<String>, bool) {
    if snapshot.state != "failed" {
        return (None, false);
    }
    let error = snapshot.error.as_deref().unwrap_or_default();
    (Some(action_for_error(error).into()), true)
}

/// The next step a failure calls for, from its code (finding F3: never from
/// words elsewhere in the message). Only `retry` can lead to an automatic
/// retry, and only for the codes [`retried_automatically`] accepts.
pub(crate) fn action_for_error(error: &str) -> &'static str {
    action_for_code(&error_code(error), error)
}

pub(crate) fn action_for_code(code: &str, error: &str) -> &'static str {
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
pub(crate) fn failure_code(view: &JobSnapshot) -> Option<String> {
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
pub(crate) fn retried_automatically(code: &str) -> bool {
    // A full drive repeats until a person frees space.
    !code.starts_with("internal.") && code != "storage.disk_full"
}

/// True for an HTTP client error that repeating will not change, such as 404
/// or 403. The engine reports every HTTP error as a transfer failure, so the
/// status is read from its message here. 408 and 429 are the server asking
/// for time, and stay retryable along with every 5xx.
pub(crate) fn refused_by_server(error: &str) -> bool {
    error
        .split("HTTP status ")
        .nth(1)
        .and_then(|rest| rest.get(..3))
        .and_then(|code| code.parse::<u16>().ok())
        .is_some_and(|code| (400..500).contains(&code) && code != 408 && code != 429)
}

pub(crate) fn find_record<'a>(
    state: &'a QueueState,
    job_id: &str,
) -> Result<&'a QueueRecord, String> {
    state
        .records
        .iter()
        .find(|record| record.id == job_id)
        .ok_or_else(|| "This download is no longer available.".to_string())
}

pub(crate) fn find_record_mut<'a>(
    state: &'a mut QueueState,
    job_id: &str,
) -> Result<&'a mut QueueRecord, String> {
    state
        .records
        .iter_mut()
        .find(|record| record.id == job_id)
        .ok_or_else(|| "This download is no longer available.".to_string())
}

pub(crate) fn state_name(state: FileJobState) -> &'static str {
    match state {
        FileJobState::Queued => "queued",
        FileJobState::Running => "running",
        FileJobState::Cancelling => "cancelling",
        FileJobState::Completed => "completed",
        FileJobState::Cancelled => "cancelled",
        FileJobState::Failed => "failed",
    }
}

pub(crate) fn media_state_name(state: MediaJobState) -> &'static str {
    match state {
        MediaJobState::Queued => "queued",
        MediaJobState::Running => "running",
        MediaJobState::Cancelling => "cancelling",
        MediaJobState::Completed => "completed",
        MediaJobState::Cancelled => "cancelled",
        MediaJobState::Failed => "failed",
    }
}

pub(crate) fn is_terminal(state: &str) -> bool {
    matches!(state, "completed" | "cancelled" | "failed")
}

/// Resolves the media helpers, preferring a directory the user chose.
///
/// A configured directory that no longer contains the helpers falls through to
/// the ambient discovery rather than failing outright, so a moved folder
/// degrades to "not set up" instead of hiding tools that are still findable.
pub(crate) fn discover_media_tools(configured: Option<&str>) -> Option<MediaTools> {
    configured
        .map(PathBuf::from)
        .and_then(|root| MediaTools::discover_in(&root).ok())
        .or_else(|| MediaTools::discover().ok())
}

/// Shows a download stopped at the disk reserve as queued and waiting for
/// space, whatever its stopping worker reports.
pub(crate) fn show_waiting_for_space(record: &mut QueueRecord) {
    record.view.state = "queued".into();
    record.view.waiting_for_space = true;
    record.view.error = None;
    record.view.action = None;
    record.view.retryable = false;
    record.finished_at_ms = None;
    record.view.finished_at_ms = None;
    record.view.bytes_per_second = None;
    record.view.eta_seconds = None;
}

/// What a download still has to write, when its total is known.
pub(crate) fn remaining_bytes(view: &JobSnapshot) -> u64 {
    view.total_bytes
        .map_or(0, |total| total.saturating_sub(view.bytes_received))
}

pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}
