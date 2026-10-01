//! Portable file-download core with recoverable checkpoints and conservative
//! HTTP resume. It makes no H2/H3 or publisher-authenticity claim.

mod checkpoint;
mod transfer;
mod verified;

pub use fetchpath_http::{LinkFacts, SegmentProgress};

/// Looks at an HTTP(S) link's headers without downloading it. Errors carry
/// a stable code first, as download errors do.
pub fn inspect_link(url: &str) -> Result<LinkFacts, String> {
    let decision = fetchpath_http::decide_protocol(fetchpath_http::ProtocolCapabilities::detect());
    fetchpath_http::inspect_link(
        url,
        &fetchpath_http::RequestContext::default(),
        &decision,
        std::time::Duration::from_secs(20),
    )
    .map_err(|error| format!("source.transfer_failed: {error}"))
}
pub use fetchpath_metalink::{
    FileHash, Metalink, MetalinkError, MetalinkFile, MetalinkUrl, ParseLimits, PieceMap,
    PieceVerification, parse_metalink, parse_with_limits,
};
pub use fetchpath_storage::{FaultInjector, FaultPoint, NoFaults};
pub use transfer::download_with_faults;
pub use verified::{
    DEFAULT_MIRROR_ATTEMPT_TIMEOUT, DeliverySource, LOWEST_PRIORITY,
    MAX_CONCURRENT_MIRROR_ATTEMPTS, MAX_MIRRORS, MAX_REPAIR_MIRRORS_PER_PIECE, MAX_REPAIR_ROUNDS,
    MAX_WHOLE_FILE_ATTEMPTS, MirrorOutcome, MirrorReport, MirrorSource, PeerOutcome, PeerReport,
    PeerSource, VerificationLevel, VerifiedDownload, VerifiedDownloadError,
    VerifiedDownloadRequest, download_verified, download_verified_cached, download_verified_shared,
    download_verified_with_faults,
};

/// The bounded content cache, re-exported so callers need only depend on this
/// crate. Completion from it is reuse, not throughput.
pub use fetchpath_cache;

use fetchpath_http::SegmentMonitor;
use std::path::PathBuf;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU64, Ordering},
};

pub const BUFFER_BYTES: usize = 16 * 1024;

struct CancelState {
    cancelled: AtomicBool,
    received: AtomicU64,
    /// Engine-confirmed total size, or 0 while the source has not stated one.
    /// A download manager that guesses a total reports a percentage that walks
    /// backwards, so an unknown length stays unknown all the way to the UI.
    total: AtomicU64,
    published: AtomicBool,
    publication_gate: Mutex<()>,
    /// Ranges a segmented transfer has in flight. Observation only.
    segments: SegmentMonitor,
}
#[derive(Clone)]
pub struct CancellationToken(Arc<CancelState>);
impl Default for CancellationToken {
    fn default() -> Self {
        Self(Arc::new(CancelState {
            cancelled: AtomicBool::new(false),
            received: AtomicU64::new(0),
            total: AtomicU64::new(0),
            published: AtomicBool::new(false),
            publication_gate: Mutex::new(()),
            segments: SegmentMonitor::default(),
        }))
    }
}
impl CancellationToken {
    /// Returns true when cancellation won the publication race.
    pub fn cancel(&self) -> bool {
        let _gate = self
            .0
            .publication_gate
            .lock()
            .expect("publication gate poisoned");
        if self.0.published.load(Ordering::Acquire) {
            return false;
        }
        self.0.cancelled.store(true, Ordering::Release);
        true
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.cancelled.load(Ordering::Acquire)
    }
    fn publication_gate(&self) -> std::sync::MutexGuard<'_, ()> {
        self.0
            .publication_gate
            .lock()
            .expect("publication gate poisoned")
    }
    fn mark_published(&self) {
        self.0.published.store(true, Ordering::Release);
    }
    fn set_received(&self, value: u64) {
        self.0.received.store(value, Ordering::Release);
    }
    /// Bytes received so far, for a progress display.
    pub fn received(&self) -> u64 {
        self.0.received.load(Ordering::Acquire)
    }
    /// Records the total the source actually stated. `None` and a stated zero
    /// both leave the total unknown rather than inventing one.
    fn set_total(&self, value: Option<u64>) {
        if let Some(total) = value.filter(|total| *total > 0) {
            self.0.total.store(total, Ordering::Release);
        }
    }
    /// The size the source stated, if it stated one.
    pub fn total(&self) -> Option<u64> {
        match self.0.total.load(Ordering::Acquire) {
            0 => None,
            total => Some(total),
        }
    }
    pub(crate) fn segment_monitor(&self) -> &SegmentMonitor {
        &self.0.segments
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CancelCleanup {
    RemoveStaging,
    RetainStaging,
}

/// Sensitive request context supplied by an explicitly authorized client.
///
/// Values are deliberately not `Debug` and must never be emitted in routine
/// snapshots, events, or errors.
#[derive(Clone, Default)]
pub struct RequestContext {
    cookie_lines: Vec<String>,
    referer: Option<String>,
}

impl RequestContext {
    pub fn new(cookie_lines: Vec<String>, referer: Option<String>) -> Result<Self, &'static str> {
        let invalid = cookie_lines.iter().any(|line| {
            line.is_empty() || line.contains(['\r', '\n', '\0']) || line.len() > 16 * 1024
        }) || referer
            .as_deref()
            .is_some_and(|value| value.contains(['\r', '\n', '\0']) || value.len() > 8 * 1024);
        if invalid || cookie_lines.len() > 256 {
            return Err("request_context.invalid");
        }
        Ok(Self {
            cookie_lines,
            referer,
        })
    }

    /// True when no credential-bearing context is attached. Used to decide
    /// cache provenance; it must stay consistent with `fingerprint`.
    pub fn is_credential_free(&self) -> bool {
        self.cookie_lines.is_empty() && self.referer.is_none()
    }

    fn fingerprint(&self) -> String {
        use sha2::{Digest, Sha256};

        if self.is_credential_free() {
            return String::new();
        }
        let mut digest = Sha256::new();
        for line in &self.cookie_lines {
            digest.update(line.len().to_le_bytes());
            digest.update(line.as_bytes());
        }
        if let Some(referer) = &self.referer {
            digest.update(referer.len().to_le_bytes());
            digest.update(referer.as_bytes());
        }
        format!("{:x}", digest.finalize())
    }
}

#[derive(Clone)]
pub struct DownloadRequest {
    pub url: String,
    pub destination: PathBuf,
    pub cancellation: CancellationToken,
    pub cancel_cleanup: CancelCleanup,
    pub context: RequestContext,
    /// A whole-file SHA-256 the person supplied, lowercase hex. When present,
    /// nothing is published unless the staged bytes match it, on every
    /// publication path including recovery. A match shows the bytes are the
    /// ones that checksum describes; it is not publisher authenticity.
    pub expected_sha256: Option<String>,
    /// The most connections this download may open, below the engine-wide
    /// limit. `None` leaves it to the adaptive transfer.
    pub max_connections: Option<usize>,
}

/// Normalizes a SHA-256 as people paste it: surrounding whitespace, an
/// optional `sha256:` or `SHA256=` prefix, and either case. Anything that is
/// not then exactly 64 hex digits is refused rather than guessed at.
pub fn normalize_sha256(text: &str) -> Option<String> {
    let trimmed = text.trim();
    let lower = trimmed.to_ascii_lowercase();
    let digits = ["sha256:", "sha256=", "sha-256:", "sha256 "]
        .iter()
        .find_map(|prefix| lower.strip_prefix(prefix))
        .unwrap_or(&lower)
        .trim();
    (digits.len() == 64 && digits.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .then(|| digits.to_owned())
}

#[derive(Debug, Eq, PartialEq)]
pub struct DownloadedFile {
    pub destination: PathBuf,
    pub bytes: u64,
    /// This is an observed local digest, not publisher-authenticity evidence.
    pub observed_sha256: String,
    /// Present only when the destination is published but its redundant staging
    /// link could not be removed.
    pub staging_cleanup_pending: Option<PathBuf>,
}

#[derive(Debug)]
pub enum DownloadError {
    InvalidUrl,
    InvalidDestination(PathBuf),
    DestinationExists {
        destination: PathBuf,
        staging: Option<PathBuf>,
    },
    Cancelled {
        staging: Option<PathBuf>,
    },
    Transport {
        detail: String,
        staging: Option<PathBuf>,
    },
    Storage {
        path: PathBuf,
        detail: String,
        staging: Option<PathBuf>,
    },
    /// The bytes do not match the checksum the person supplied. When
    /// `published` is `None` nothing was saved and the bytes were discarded.
    /// When it is a path, an earlier run had already published that file
    /// before crashing; it is reported, never deleted.
    ChecksumMismatch {
        expected: String,
        observed: String,
        published: Option<PathBuf>,
    },
}
impl std::fmt::Display for DownloadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidUrl => write!(
                f,
                "input.invalid_url: only http:// and https:// URLs are supported"
            ),
            Self::InvalidDestination(path) => write!(
                f,
                "input.invalid_destination: {} needs a file name",
                path.display()
            ),
            Self::DestinationExists {
                destination,
                staging,
            } => {
                write!(
                    f,
                    "storage.destination_conflict: {} already exists",
                    destination.display()
                )?;
                write_retained(f, staging)
            }
            Self::Cancelled {
                staging: Some(path),
            } => write!(f, "cancelled; retained staging file {}", path.display()),
            Self::Cancelled { staging: None } => write!(f, "cancelled; staging file removed"),
            Self::Transport { detail, staging } => {
                write!(f, "source.transfer_failed: {detail}")?;
                write_retained(f, staging)
            }
            Self::Storage {
                path,
                detail,
                staging,
            } => {
                write!(f, "storage.failed at {}: {detail}", path.display())?;
                write_retained(f, staging)
            }
            Self::ChecksumMismatch {
                expected,
                observed,
                published: None,
            } => write!(
                f,
                "integrity.checksum_mismatch: expected sha256 {expected}, received {observed}; nothing was saved"
            ),
            Self::ChecksumMismatch {
                expected,
                observed,
                published: Some(path),
            } => write!(
                f,
                "integrity.checksum_mismatch: expected sha256 {expected}, but {} holds {observed}",
                path.display()
            ),
        }
    }
}
impl std::error::Error for DownloadError {}

fn write_retained(f: &mut std::fmt::Formatter<'_>, staging: &Option<PathBuf>) -> std::fmt::Result {
    if let Some(path) = staging {
        write!(f, "; retained staging file {}", path.display())?;
    }
    Ok(())
}

#[cfg(test)]
fn cleanup_staging(staging: &std::path::Path) -> Option<PathBuf> {
    match std::fs::remove_file(staging) {
        Ok(()) => None,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(_) => Some(staging.to_path_buf()),
    }
}

#[cfg(test)]
fn cancelled(staging: PathBuf, cleanup: CancelCleanup) -> DownloadError {
    let staging = match cleanup {
        CancelCleanup::RetainStaging => Some(staging),
        CancelCleanup::RemoveStaging => cleanup_staging(&staging),
    };
    DownloadError::Cancelled { staging }
}

/// Streams one HTTP representation through recoverable same-volume checkpoints,
/// then claims the destination with a create-only publication fence.
pub fn download(request: DownloadRequest) -> Result<DownloadedFile, DownloadError> {
    transfer::download(request)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FileJobState {
    Queued,
    Running,
    Cancelling,
    Completed,
    Cancelled,
    Failed,
}
#[derive(Clone, Debug)]
pub struct FileJobSnapshot {
    pub job_id: String,
    pub job_revision: u64,
    pub state: FileJobState,
    pub bytes_received: u64,
    /// Engine-confirmed total size. `None` means the source never stated one.
    pub total_bytes: Option<u64>,
    pub destination: Option<PathBuf>,
    pub observed_sha256: Option<String>,
    pub staging_cleanup_pending: Option<PathBuf>,
    pub error: Option<String>,
    /// Published from the local cache: a copy, not a transfer, so there is no
    /// rate to report.
    pub reused_from_cache: bool,
    /// The fingerprint of the paired device it came from, when it did.
    pub from_peer: Option<[u8; 32]>,
}
#[derive(Clone, Debug)]
pub struct FileJobEvent {
    pub job_id: String,
    pub seq: u64,
    pub job_revision: u64,
    pub state: FileJobState,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CancelResult {
    Accepted,
    TooLate,
    AlreadyTerminal,
}
struct JobInner {
    snapshot: FileJobSnapshot,
    events: Vec<FileJobEvent>,
    request: Option<DownloadRequest>,
    cache: Option<(PathBuf, fetchpath_cache::CacheConfig)>,
    peers: Vec<Arc<dyn PeerSource + Send + Sync>>,
    worker: Option<std::thread::JoinHandle<()>>,
}
#[derive(Clone)]
pub struct FileJob {
    inner: Arc<Mutex<JobInner>>,
    cancellation: CancellationToken,
}

fn transition(inner: &mut JobInner, state: FileJobState) {
    inner.snapshot.job_revision += 1;
    inner.snapshot.state = state;
    let seq = inner.events.last().map_or(1, |event| event.seq + 1);
    inner.events.push(FileJobEvent {
        job_id: inner.snapshot.job_id.clone(),
        seq,
        job_revision: inner.snapshot.job_revision,
        state,
    });
}

impl FileJob {
    pub fn create(url: String, destination: PathBuf) -> Self {
        Self::create_with_cleanup(url, destination, CancelCleanup::RemoveStaging)
    }

    /// Creates a job whose orderly cancellation retains staging/checkpoint data
    /// so a later job for the same source and destination can recover it.
    pub fn create_recoverable(url: String, destination: PathBuf) -> Self {
        Self::create_with_cleanup(url, destination, CancelCleanup::RetainStaging)
    }

    pub fn create_recoverable_with_context(
        url: String,
        destination: PathBuf,
        context: RequestContext,
    ) -> Self {
        Self::create_with_context(url, destination, CancelCleanup::RetainStaging, context)
    }

    fn create_with_cleanup(
        url: String,
        destination: PathBuf,
        cancel_cleanup: CancelCleanup,
    ) -> Self {
        Self::create_with_context(url, destination, cancel_cleanup, RequestContext::default())
    }

    fn create_with_context(
        url: String,
        destination: PathBuf,
        cancel_cleanup: CancelCleanup,
        context: RequestContext,
    ) -> Self {
        let cancellation = CancellationToken::default();
        let id = uuid::Uuid::new_v4().to_string();
        let snapshot = FileJobSnapshot {
            job_id: id.clone(),
            job_revision: 1,
            state: FileJobState::Queued,
            bytes_received: 0,
            total_bytes: None,
            destination: Some(destination.clone()),
            observed_sha256: None,
            staging_cleanup_pending: None,
            error: None,
            reused_from_cache: false,
            from_peer: None,
        };
        Self {
            inner: Arc::new(Mutex::new(JobInner {
                snapshot,
                events: vec![FileJobEvent {
                    job_id: id,
                    seq: 1,
                    job_revision: 1,
                    state: FileJobState::Queued,
                }],
                request: Some(DownloadRequest {
                    url,
                    destination,
                    cancellation: cancellation.clone(),
                    cancel_cleanup,
                    context,
                    expected_sha256: None,
                    max_connections: None,
                }),
                cache: None,
                peers: Vec::new(),
                worker: None,
            })),
            cancellation,
        }
    }
    /// Requires the published file to match `checksum`, as a person would
    /// paste it. Refused once the job has started, and refused for anything
    /// that is not a SHA-256.
    pub fn with_expected_sha256(self, checksum: &str) -> Result<Self, &'static str> {
        let normalized = normalize_sha256(checksum).ok_or("input.invalid_checksum")?;
        {
            let mut inner = self.inner.lock().unwrap();
            let request = inner
                .request
                .as_mut()
                .ok_or("contract.invalid_transition")?;
            request.expected_sha256 = Some(normalized);
        }
        Ok(self)
    }

    /// Lets a checksum-verified download complete from the content cache at
    /// `root` and remember what it fetches there. Only before it starts; a
    /// job without a checksum never touches the cache.
    pub fn use_cache(
        &self,
        root: PathBuf,
        config: fetchpath_cache::CacheConfig,
    ) -> Result<(), &'static str> {
        let mut inner = self.inner.lock().unwrap();
        if inner.request.is_none() {
            return Err("contract.invalid_transition");
        }
        inner.cache = Some((root, config));
        Ok(())
    }

    /// Paired devices to ask, after the cache and before the link, for a
    /// checksum-verified download. Only before it starts.
    pub fn use_peers(
        &self,
        peers: Vec<Arc<dyn PeerSource + Send + Sync>>,
    ) -> Result<(), &'static str> {
        let mut inner = self.inner.lock().unwrap();
        if inner.request.is_none() {
            return Err("contract.invalid_transition");
        }
        inner.peers = peers;
        Ok(())
    }

    /// Caps the connections this download opens. Only before it starts.
    pub fn limit_connections(&self, connections: usize) -> Result<(), &'static str> {
        let mut inner = self.inner.lock().unwrap();
        let request = inner
            .request
            .as_mut()
            .ok_or("contract.invalid_transition")?;
        request.max_connections = Some(connections.max(1));
        Ok(())
    }

    pub fn start(&self) -> Result<(), &'static str> {
        self.start_with_completion(|| {})
    }

    /// Starts with a wakeup after the terminal snapshot is available. The
    /// callback runs outside job locks, before optional cache population;
    /// it must be quick and must not panic. Joining still waits for the worker.
    pub fn start_with_completion(
        &self,
        on_complete: impl FnOnce() + Send + 'static,
    ) -> Result<(), &'static str> {
        let mut inner = self.inner.lock().unwrap();
        if inner.snapshot.state != FileJobState::Queued {
            return Err("contract.invalid_transition");
        }
        let request = inner.request.take().unwrap();
        let cache = inner
            .cache
            .take()
            .filter(|_| request.expected_sha256.is_some());
        let peers = std::mem::take(&mut inner.peers);
        transition(&mut inner, FileJobState::Running);
        let shared = self.inner.clone();
        let token = self.cancellation.clone();
        inner.worker = Some(std::thread::spawn(move || {
            let mut cache = cache
                .and_then(|(root, config)| fetchpath_cache::ContentCache::open(&root, config).ok());
            let reused = cache
                .as_mut()
                .and_then(|cache| verified::reuse_for_file(&request, cache));
            let reused_from_cache = reused.is_some();
            // Never more than the cache would keep, or 64 GiB without one.
            let ceiling = cache
                .as_ref()
                .map_or(64 << 30, |cache| cache.config().max_entry_bytes);
            let mut from_peer = None;
            let result = match reused {
                Some(done) => Ok(done),
                None if request.expected_sha256.is_some() && !peers.is_empty() => {
                    match verified::fetch_file_from_peers(&request, &peers, ceiling) {
                        Some((done, fingerprint)) => {
                            from_peer = Some(fingerprint);
                            Ok(done)
                        }
                        None => download(request.clone()),
                    }
                }
                None => download(request.clone()),
            };
            // Copied into the cache after the job reports completion, so a
            // large file does not sit at 100% while it is copied.
            let remember = match &result {
                Ok(done) if !reused_from_cache && from_peer.is_none() => {
                    Some(done.destination.clone())
                }
                _ => None,
            };
            let mut state = shared.lock().unwrap();
            if !matches!(
                state.snapshot.state,
                FileJobState::Running | FileJobState::Cancelling
            ) {
                return;
            }
            state.snapshot.bytes_received = token.received();
            state.snapshot.total_bytes = token.total().or(state.snapshot.total_bytes);
            match result {
                Ok(done) => {
                    if reused_from_cache || from_peer.is_some() {
                        state.snapshot.bytes_received = done.bytes;
                        state.snapshot.total_bytes = Some(done.bytes);
                        state.snapshot.reused_from_cache = reused_from_cache;
                        state.snapshot.from_peer = from_peer;
                    }
                    state.snapshot.destination = Some(done.destination);
                    state.snapshot.observed_sha256 = Some(done.observed_sha256);
                    state.snapshot.staging_cleanup_pending = done.staging_cleanup_pending;
                    transition(&mut state, FileJobState::Completed);
                }
                Err(DownloadError::Cancelled { staging }) => {
                    state.snapshot.staging_cleanup_pending = staging;
                    transition(&mut state, FileJobState::Cancelled);
                }
                Err(error) => {
                    state.snapshot.staging_cleanup_pending = match &error {
                        DownloadError::DestinationExists { staging, .. }
                        | DownloadError::Transport { staging, .. }
                        | DownloadError::Storage { staging, .. } => staging.clone(),
                        DownloadError::Cancelled { staging } => staging.clone(),
                        DownloadError::InvalidUrl
                        | DownloadError::InvalidDestination(_)
                        | DownloadError::ChecksumMismatch { .. } => None,
                    };
                    state.snapshot.error = Some(error.to_string());
                    transition(&mut state, FileJobState::Failed);
                }
            };
            drop(state);
            on_complete();
            if let (Some(published), Some(cache)) = (remember, cache.as_mut()) {
                verified::remember_file(&request, &published, cache);
            }
        }));
        Ok(())
    }
    pub fn cancel(&self) -> CancelResult {
        let mut state = self.inner.lock().unwrap();
        match state.snapshot.state {
            FileJobState::Completed | FileJobState::Cancelled | FileJobState::Failed => {
                CancelResult::AlreadyTerminal
            }
            FileJobState::Queued => {
                self.cancellation.cancel();
                transition(&mut state, FileJobState::Cancelled);
                CancelResult::Accepted
            }
            FileJobState::Running => {
                drop(state);
                if !self.cancellation.cancel() {
                    return CancelResult::TooLate;
                }
                let mut state = self.inner.lock().unwrap();
                if state.snapshot.state == FileJobState::Running {
                    transition(&mut state, FileJobState::Cancelling);
                }
                CancelResult::Accepted
            }
            FileJobState::Cancelling => CancelResult::Accepted,
        }
    }
    /// The byte ranges this download has in flight right now, received but
    /// not yet written. Empty unless the transfer is segmented.
    pub fn segments(&self) -> Vec<SegmentProgress> {
        self.cancellation.segment_monitor().snapshot()
    }

    pub fn snapshot(&self) -> FileJobSnapshot {
        let mut state = self.inner.lock().unwrap();
        if matches!(
            state.snapshot.state,
            FileJobState::Running | FileJobState::Cancelling
        ) {
            // Received counts bytes held in memory for a range as well as
            // those written; what survives a crash is the checkpoint, which
            // is separate (contract §7).
            // A range already written is not counted again.
            let written = self.cancellation.received();
            let in_flight: u64 = self
                .cancellation
                .segment_monitor()
                .snapshot()
                .iter()
                .filter(|range| range.start >= written)
                .map(|range| range.received)
                .sum();
            state.snapshot.bytes_received = written + in_flight;
        }
        // The total is published as soon as the source states one and stays
        // available after the transfer ends, so a completed row can still show
        // the size that was agreed rather than dropping back to "unknown".
        if let Some(total) = self.cancellation.total() {
            state.snapshot.total_bytes = Some(total);
        }
        state.snapshot.clone()
    }
    pub fn events(&self) -> Vec<FileJobEvent> {
        self.inner.lock().unwrap().events.clone()
    }
    pub fn events_after(&self, after_seq: u64) -> Vec<FileJobEvent> {
        self.inner
            .lock()
            .unwrap()
            .events
            .iter()
            .filter(|event| event.seq > after_seq)
            .cloned()
            .collect()
    }
    pub fn join(&self) {
        let worker = { self.inner.lock().unwrap().worker.take() };
        if let Some(worker) = worker {
            let _ = worker.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};
    use std::fs;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    #[test]
    fn a_default_request_context_is_credential_free_and_a_populated_one_is_not() {
        assert!(RequestContext::default().is_credential_free());

        let with_cookie = RequestContext::new(vec!["a=b".to_owned()], None).expect("valid");
        assert!(!with_cookie.is_credential_free());

        let with_referer =
            RequestContext::new(Vec::new(), Some("https://example.test/".to_owned()))
                .expect("valid");
        assert!(!with_referer.is_credential_free());
    }

    fn temp_dir(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "fetchpath-{label}-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }
    fn server(body: Vec<u8>, slow: bool) -> (String, thread::JoinHandle<()>) {
        server_status(body, slow, 200)
    }
    fn server_status(body: Vec<u8>, slow: bool, status: u16) -> (String, thread::JoinHandle<()>) {
        let declared_length = body.len();
        server_with_declared_length(body, slow, status, declared_length)
    }
    fn server_with_declared_length(
        body: Vec<u8>,
        slow: bool,
        status: u16,
        declared_length: usize,
    ) -> (String, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request);
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        declared_length
                    )
                    .as_bytes(),
                )
                .unwrap();
            for chunk in body.chunks(16 * 1024) {
                if stream.write_all(chunk).is_err() {
                    break;
                }
                if slow {
                    thread::sleep(Duration::from_millis(15));
                }
            }
        });
        (url, handle)
    }
    /// A slow server that signals once its first body chunk is on the wire.
    fn server_signalling_first_chunk(
        body: Vec<u8>,
    ) -> (
        String,
        thread::JoinHandle<()>,
        std::sync::mpsc::Receiver<()>,
    ) {
        let (signal, streaming) = std::sync::mpsc::channel();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request);
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 200 Test
Content-Length: {}
Connection: close

",
                        body.len()
                    )
                    .as_bytes(),
                )
                .unwrap();
            for (index, chunk) in body.chunks(16 * 1024).enumerate() {
                if stream.write_all(chunk).is_err() {
                    break;
                }
                if index == 0 {
                    let _ = signal.send(());
                }
                thread::sleep(Duration::from_millis(15));
            }
        });
        (url, handle, streaming)
    }
    /// A response that states no length at all: the body simply runs to EOF.
    fn server_without_declared_length(body: Vec<u8>) -> (String, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request);
            stream
                .write_all(b"HTTP/1.1 200 Test\r\nConnection: close\r\n\r\n")
                .unwrap();
            let _ = stream.write_all(&body);
        });
        (url, handle)
    }

    #[test]
    fn a_stated_content_length_becomes_the_reported_total() {
        let dir = temp_dir("total-known");
        let body = b"sized fixture".repeat(4096);
        let expected = body.len() as u64;
        let (url, server) = server(body, false);
        let job = FileJob::create(url, dir.join("sized.bin"));
        job.start().unwrap();
        job.join();
        server.join().unwrap();

        let snapshot = job.snapshot();
        assert_eq!(snapshot.state, FileJobState::Completed);
        assert_eq!(snapshot.total_bytes, Some(expected));
        assert_eq!(snapshot.bytes_received, expected);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_response_without_a_length_reports_no_total_rather_than_the_bytes_so_far() {
        let dir = temp_dir("total-unknown");
        let body = b"unsized fixture".repeat(4096);
        let expected = body.len() as u64;
        let (url, server) = server_without_declared_length(body);
        let job = FileJob::create(url, dir.join("unsized.bin"));
        job.start().unwrap();
        job.join();
        server.join().unwrap();

        let snapshot = job.snapshot();
        assert_eq!(snapshot.state, FileJobState::Completed);
        // The bytes are all here, but the source never agreed to a size. The
        // total has to stay absent: reporting `bytes_received` as the total is
        // how a progress bar comes to read 100% from its very first sample.
        assert_eq!(snapshot.total_bytes, None);
        assert_eq!(snapshot.bytes_received, expected);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_queued_job_has_no_total_before_the_source_states_one() {
        let dir = temp_dir("total-queued");
        let job = FileJob::create("http://127.0.0.1:1/never".into(), dir.join("queued.bin"));
        assert_eq!(job.snapshot().total_bytes, None);
        assert_eq!(job.snapshot().bytes_received, 0);
        fs::remove_dir_all(&dir).unwrap();
    }

    fn request(
        url: String,
        destination: PathBuf,
        token: CancellationToken,
        cleanup: CancelCleanup,
    ) -> DownloadRequest {
        DownloadRequest {
            url,
            destination,
            cancellation: token,
            cancel_cleanup: cleanup,
            context: RequestContext::default(),
            expected_sha256: None,
            max_connections: None,
        }
    }

    fn finished(job: &FileJob) -> FileJobSnapshot {
        job.join();
        job.snapshot()
    }

    #[test]
    fn a_checksum_job_completes_from_the_cache_its_first_download_filled() {
        use fetchpath_cache::{CacheConfig, ContentCache, ContentId, Provenance};
        let dir = temp_dir("cache-reuse");
        let root = dir.join("cache");
        let config = CacheConfig::new(1 << 24, 1 << 24);
        let body = b"cached fixture".repeat(4096);
        let expected = format!("{:x}", Sha256::digest(&body));
        let (url, server) = server(body.clone(), false);

        let first = FileJob::create(url.clone(), dir.join("first.bin"))
            .with_expected_sha256(&expected)
            .unwrap();
        first.use_cache(root.clone(), config).unwrap();
        first.start().unwrap();
        let done = finished(&first);
        server.join().unwrap();
        assert_eq!(done.state, FileJobState::Completed);
        assert!(!done.reused_from_cache);
        let id = ContentId::from_expected_sha256(&expected).unwrap();
        let entry = ContentCache::open(&root, config)
            .unwrap()
            .lookup(&id)
            .unwrap();
        // A plain link with no cookies or referrer may be shared later.
        assert_eq!(entry.provenance, Provenance::Public);

        // The server is gone: only the cache can complete this one.
        let second = FileJob::create(url, dir.join("second.bin"))
            .with_expected_sha256(&expected)
            .unwrap();
        second.use_cache(root.clone(), config).unwrap();
        second.start().unwrap();
        let reused = finished(&second);
        assert_eq!(reused.state, FileJobState::Completed, "{:?}", reused.error);
        assert!(reused.reused_from_cache);
        assert_eq!(reused.total_bytes, Some(body.len() as u64));
        assert_eq!(fs::read(dir.join("second.bin")).unwrap(), body);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_signed_link_is_cached_as_private_and_a_job_without_a_checksum_is_not_cached() {
        use fetchpath_cache::{CacheConfig, ContentCache, ContentId, Provenance};
        let dir = temp_dir("cache-private");
        let root = dir.join("cache");
        let config = CacheConfig::new(1 << 24, 1 << 24);
        let body = b"signed fixture".repeat(1024);
        let expected = format!("{:x}", Sha256::digest(&body));

        let (url, first) = server(body.clone(), false);
        let unchecked = FileJob::create(format!("{url}/plain"), dir.join("plain.bin"));
        unchecked.use_cache(root.clone(), config).unwrap();
        unchecked.start().unwrap();
        assert_eq!(finished(&unchecked).state, FileJobState::Completed);
        first.join().unwrap();
        assert!(
            ContentCache::open(&root, config)
                .unwrap()
                .entries()
                .is_empty()
        );

        let (url, second) = server(body, false);
        let signed = FileJob::create(format!("{url}/file?sig=secret"), dir.join("signed.bin"))
            .with_expected_sha256(&expected)
            .unwrap();
        signed.use_cache(root.clone(), config).unwrap();
        signed.start().unwrap();
        assert_eq!(finished(&signed).state, FileJobState::Completed);
        second.join().unwrap();
        let id = ContentId::from_expected_sha256(&expected).unwrap();
        let entry = ContentCache::open(&root, config)
            .unwrap()
            .lookup(&id)
            .unwrap();
        assert_eq!(entry.provenance, Provenance::Credentialed);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn streams_to_unique_staging_hashes_and_publishes_without_leaving_part() {
        let dir = temp_dir("complete");
        let body = b"fixture bytes for fetchpath".repeat(4096);
        let expected = format!("{:x}", Sha256::digest(&body));
        let (url, server) = server(body.clone(), false);
        let destination = dir.join("file.bin");
        let done = download(request(
            url,
            destination.clone(),
            CancellationToken::default(),
            CancelCleanup::RemoveStaging,
        ))
        .unwrap();
        server.join().unwrap();
        assert_eq!(done.bytes, body.len() as u64);
        assert_eq!(done.observed_sha256, expected);
        assert_eq!(fs::read(&destination).unwrap(), body);
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn never_replaces_an_existing_destination() {
        let dir = temp_dir("conflict");
        let destination = dir.join("existing.bin");
        fs::write(&destination, b"keep me").unwrap();
        let result = download(request(
            "http://127.0.0.1:9/nope".into(),
            destination.clone(),
            CancellationToken::default(),
            CancelCleanup::RemoveStaging,
        ));
        assert!(matches!(
            result,
            Err(DownloadError::DestinationExists {
                destination: path,
                staging: None
            }) if path == destination
        ));
        assert_eq!(fs::read(&destination).unwrap(), b"keep me");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn non_200_responses_never_publish_a_body() {
        for status in [404, 500, 206] {
            let dir = temp_dir("status");
            let destination = dir.join("response.bin");
            let (url, server) =
                server_status(b"not a complete representation".to_vec(), false, status);
            let result = download(request(
                url,
                destination.clone(),
                CancellationToken::default(),
                CancelCleanup::RemoveStaging,
            ));
            server.join().unwrap();
            assert!(matches!(
                result,
                Err(DownloadError::Transport {
                    detail,
                    staging: None
                }) if detail == format!("HTTP status {status}")
            ));
            assert!(!destination.exists());
            assert_eq!(fs::read_dir(&dir).unwrap().count(), 0);
            fs::remove_dir_all(dir).unwrap();
        }
    }

    #[test]
    fn truncated_response_never_publishes() {
        let dir = temp_dir("truncated");
        let destination = dir.join("truncated.bin");
        let body = vec![3; 16 * 1024];
        let (url, server) = server_with_declared_length(body.clone(), false, 200, body.len() * 2);
        let result = download(request(
            url,
            destination.clone(),
            CancellationToken::default(),
            CancelCleanup::RemoveStaging,
        ));
        server.join().unwrap();

        assert!(matches!(
            result,
            Err(DownloadError::Transport { staging: None, .. })
        ));
        assert!(!destination.exists());
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 0);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn concurrent_destination_creator_wins_without_overwrite() {
        let dir = temp_dir("destination-race");
        let destination = dir.join("race.bin");
        let (url, server) = server(vec![5; 1024 * 1024], true);
        let competing_path = destination.clone();
        let competitor = thread::spawn(move || {
            thread::sleep(Duration::from_millis(30));
            fs::write(competing_path, b"competitor").unwrap();
        });
        let result = download(request(
            url,
            destination.clone(),
            CancellationToken::default(),
            CancelCleanup::RemoveStaging,
        ));
        competitor.join().unwrap();
        server.join().unwrap();

        assert!(matches!(
            result,
            Err(DownloadError::DestinationExists {
                destination: path,
                staging: None
            }) if path == destination
        ));
        assert_eq!(fs::read(&destination).unwrap(), b"competitor");
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn cancellation_removes_or_retains_unpublished_staging_by_policy() {
        for (label, cleanup, expect_staging) in [
            ("remove", CancelCleanup::RemoveStaging, false),
            ("retain", CancelCleanup::RetainStaging, true),
        ] {
            let dir = temp_dir(label);
            let (url, server, streaming) = server_signalling_first_chunk(vec![7; 512 * 1024]);
            let token = CancellationToken::default();
            let trigger = token.clone();
            // Cancel mid-transfer, once bytes are actually flowing. A fixed
            // delay raced the process-wide request budget: under load the
            // download could still be queued for a permit, see the cancel,
            // never connect, and leave the server blocked in accept forever.
            let canceller = thread::spawn(move || {
                streaming.recv().expect("server began streaming");
                trigger.cancel();
            });
            let result = download(request(url, dir.join("cancel.bin"), token, cleanup));
            canceller.join().unwrap();
            server.join().unwrap();
            match result {
                Err(DownloadError::Cancelled { staging }) => {
                    assert_eq!(staging.is_some(), expect_staging);
                    if let Some(path) = staging {
                        assert!(path.exists());
                        fs::remove_file(path).unwrap();
                    }
                }
                other => panic!("expected cancellation, got {other:?}"),
            }
            assert!(!dir.join("cancel.bin").exists());
            assert_eq!(fs::read_dir(&dir).unwrap().count(), 0);
            fs::remove_dir_all(dir).unwrap();
        }
    }

    #[test]
    fn a_pasted_checksum_is_normalized_or_refused() {
        let digest = "ab".repeat(32);
        for pasted in [
            digest.clone(),
            digest.to_uppercase(),
            format!(
                "  sha256:{digest}
"
            ),
            format!("SHA256={}", digest.to_uppercase()),
        ] {
            assert_eq!(
                normalize_sha256(&pasted),
                Some(digest.clone()),
                "{pasted:?}"
            );
        }
        assert_eq!(normalize_sha256(&digest[..63]), None, "too short");
        assert_eq!(normalize_sha256(&format!("{digest}0")), None, "too long");
        assert_eq!(normalize_sha256(&"zz".repeat(32)), None, "not hex");
        assert_eq!(
            normalize_sha256(&format!("md5:{digest}")),
            None,
            "wrong algorithm"
        );
    }

    #[test]
    fn a_job_takes_a_checksum_only_before_it_starts() {
        let job = FileJob::create(
            "http://127.0.0.1:9/never".into(),
            temp_dir("builder").join("x"),
        );
        assert!(job.clone().with_expected_sha256("nope").is_err());
        let job = job
            .with_expected_sha256(&"cd".repeat(32))
            .expect("accepted before start");
        job.start().expect("starts");
        assert_eq!(
            job.clone().with_expected_sha256(&"cd".repeat(32)).err(),
            Some("contract.invalid_transition")
        );
        job.cancel();
        job.join();
    }

    #[test]
    fn rejects_non_http_input_before_creating_a_file() {
        let dir = temp_dir("input");
        let destination = dir.join("x");
        assert!(matches!(
            download(request(
                "file:///tmp/x".into(),
                destination.clone(),
                CancellationToken::default(),
                CancelCleanup::RemoveStaging
            )),
            Err(DownloadError::InvalidUrl)
        ));
        assert!(!destination.exists());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn failed_cleanup_reports_the_retained_staging_path() {
        let dir = temp_dir("cleanup-reporting");
        let not_a_file = dir.join("staging-directory");
        fs::create_dir(&not_a_file).unwrap();

        assert_eq!(cleanup_staging(&not_a_file), Some(not_a_file.clone()));
        assert!(matches!(
            cancelled(not_a_file.clone(), CancelCleanup::RemoveStaging),
            DownloadError::Cancelled {
                staging: Some(path)
            } if path == not_a_file
        ));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn file_job_completes_with_ordered_events_and_snapshot() {
        let dir = temp_dir("job-complete");
        let body = b"coordinator fixture".repeat(8192);
        let expected = format!("{:x}", Sha256::digest(&body));
        let (url, server) = server(body.clone(), false);
        let destination = dir.join("coordinated.bin");
        let job = FileJob::create(url, destination.clone());

        let created = job.snapshot();
        assert_eq!(created.state, FileJobState::Queued);
        assert_eq!(created.job_revision, 1);
        assert!(uuid::Uuid::parse_str(&created.job_id).is_ok());
        assert_eq!(
            job.events()
                .iter()
                .map(|event| event.seq)
                .collect::<Vec<_>>(),
            [1]
        );

        job.start().unwrap();
        assert_eq!(job.start(), Err("contract.invalid_transition"));
        job.join();
        server.join().unwrap();

        let completed = job.snapshot();
        assert_eq!(completed.state, FileJobState::Completed);
        assert_eq!(completed.job_revision, 3);
        assert_eq!(completed.bytes_received, body.len() as u64);
        assert_eq!(completed.destination, Some(destination.clone()));
        assert_eq!(
            completed.observed_sha256.as_deref(),
            Some(expected.as_str())
        );
        assert_eq!(completed.staging_cleanup_pending, None);
        assert_eq!(
            job.events()
                .iter()
                .map(|event| (event.seq, event.job_revision, event.state))
                .collect::<Vec<_>>(),
            [
                (1, 1, FileJobState::Queued),
                (2, 2, FileJobState::Running),
                (3, 3, FileJobState::Completed),
            ]
        );
        assert!(
            job.events()
                .iter()
                .all(|event| event.job_id == completed.job_id)
        );
        assert_eq!(job.events_after(1).len(), 2);
        assert_eq!(fs::read(destination).unwrap(), body);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn queued_job_cancels_without_starting() {
        let dir = temp_dir("job-queued-cancel");
        let destination = dir.join("never.bin");
        let job = FileJob::create("http://127.0.0.1:9/never".into(), destination.clone());

        assert_eq!(job.cancel(), CancelResult::Accepted);
        assert_eq!(job.cancel(), CancelResult::AlreadyTerminal);
        assert_eq!(job.start(), Err("contract.invalid_transition"));
        assert_eq!(job.snapshot().state, FileJobState::Cancelled);
        assert_eq!(
            job.events()
                .iter()
                .map(|event| event.state)
                .collect::<Vec<_>>(),
            [FileJobState::Queued, FileJobState::Cancelled]
        );
        assert!(!destination.exists());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn running_job_emits_cancelling_then_cancelled() {
        let dir = temp_dir("job-running-cancel");
        let (url, server) = server(vec![9; 1024 * 1024], true);
        let destination = dir.join("cancelled.bin");
        let job = FileJob::create(url, destination.clone());
        job.start().unwrap();

        for _ in 0..100 {
            if job.snapshot().bytes_received > 0 {
                break;
            }
            thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(job.cancel(), CancelResult::Accepted);
        job.join();
        server.join().unwrap();

        assert_eq!(job.snapshot().state, FileJobState::Cancelled);
        assert_eq!(
            job.events()
                .iter()
                .map(|event| event.state)
                .collect::<Vec<_>>(),
            [
                FileJobState::Queued,
                FileJobState::Running,
                FileJobState::Cancelling,
                FileJobState::Cancelled,
            ]
        );
        assert!(!destination.exists());
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 0);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn scoped_browser_cookie_context_reaches_the_authorized_origin() {
        use std::sync::mpsc;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (request_tx, request_rx) = mpsc::channel();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = vec![0_u8; 4096];
            let read = stream.read(&mut request).unwrap();
            request.truncate(read);
            request_tx
                .send(String::from_utf8_lossy(&request).into_owned())
                .unwrap();
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
                .unwrap();
        });
        let dir = temp_dir("browser-cookie");
        let destination = dir.join("authenticated.bin");
        let context = RequestContext::new(
            vec!["127.0.0.1\tFALSE\t/\tFALSE\t0\tsession\tprivate-value".into()],
            None,
        )
        .unwrap();
        let job = FileJob::create_recoverable_with_context(
            format!("http://{address}/authenticated.bin"),
            destination.clone(),
            context,
        );
        job.start().unwrap();
        job.join();
        server.join().unwrap();

        assert_eq!(fs::read(destination).unwrap(), b"ok");
        assert!(
            request_rx
                .recv()
                .unwrap()
                .contains("Cookie: session=private-value")
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn browser_cookie_context_does_not_cross_a_redirected_host_boundary() {
        use std::sync::mpsc;

        let destination_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let destination_address = destination_listener.local_addr().unwrap();
        let (request_tx, request_rx) = mpsc::channel();
        let destination_server = thread::spawn(move || {
            let (mut stream, _) = destination_listener.accept().unwrap();
            let mut request = vec![0_u8; 4096];
            let read = stream.read(&mut request).unwrap();
            request.truncate(read);
            request_tx
                .send(String::from_utf8_lossy(&request).into_owned())
                .unwrap();
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
                .unwrap();
        });

        let redirect_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let redirect_address = redirect_listener.local_addr().unwrap();
        let redirect_server = thread::spawn(move || {
            let (mut stream, _) = redirect_listener.accept().unwrap();
            let mut request = [0_u8; 2048];
            let _ = stream.read(&mut request);
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 302 Found\r\nLocation: http://localhost:{}/final\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                        destination_address.port()
                    )
                    .as_bytes(),
                )
                .unwrap();
        });

        let dir = temp_dir("browser-cookie-redirect");
        let destination = dir.join("redirected.bin");
        let context = RequestContext::new(
            vec!["127.0.0.1\tFALSE\t/\tFALSE\t0\tsession\tprivate-value".into()],
            None,
        )
        .unwrap();
        let job = FileJob::create_recoverable_with_context(
            format!("http://{redirect_address}/start"),
            destination.clone(),
            context,
        );
        job.start().unwrap();
        job.join();
        redirect_server.join().unwrap();
        destination_server.join().unwrap();

        assert_eq!(fs::read(destination).unwrap(), b"ok");
        assert!(!request_rx.recv().unwrap().contains("private-value"));
        fs::remove_dir_all(dir).unwrap();
    }
}
