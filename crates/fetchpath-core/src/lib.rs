//! The first sequential file-download slice. It intentionally has no resume,
//! persistence, redirects across credential boundaries, or H2/H3 promise.

use curl::easy::Easy;
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
use std::time::{SystemTime, UNIX_EPOCH};

pub const BUFFER_BYTES: usize = 16 * 1024;

struct CancelState {
    cancelled: AtomicBool,
    received: AtomicU64,
    published: AtomicBool,
    publication_gate: Mutex<()>,
}
#[derive(Clone)]
pub struct CancellationToken(Arc<CancelState>);
impl Default for CancellationToken {
    fn default() -> Self {
        Self(Arc::new(CancelState {
            cancelled: AtomicBool::new(false),
            received: AtomicU64::new(0),
            published: AtomicBool::new(false),
            publication_gate: Mutex::new(()),
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
    fn received(&self) -> u64 {
        self.0.received.load(Ordering::Acquire)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CancelCleanup {
    RemoveStaging,
    RetainStaging,
}

#[derive(Clone)]
pub struct DownloadRequest {
    pub url: String,
    pub destination: PathBuf,
    pub cancellation: CancellationToken,
    pub cancel_cleanup: CancelCleanup,
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

fn storage(path: &Path, error: std::io::Error) -> DownloadError {
    DownloadError::Storage {
        path: path.to_path_buf(),
        detail: error.to_string(),
        staging: None,
    }
}

fn cleanup_staging(staging: &Path) -> Option<PathBuf> {
    match fs::remove_file(staging) {
        Ok(()) => None,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(_) => Some(staging.to_path_buf()),
    }
}

fn cancelled(staging: PathBuf, cleanup: CancelCleanup) -> DownloadError {
    let staging = match cleanup {
        CancelCleanup::RetainStaging => Some(staging),
        CancelCleanup::RemoveStaging => cleanup_staging(&staging),
    };
    DownloadError::Cancelled { staging }
}

fn transport(detail: impl Into<String>, staging: &Path) -> DownloadError {
    DownloadError::Transport {
        detail: detail.into(),
        staging: cleanup_staging(staging),
    }
}
fn unique_staging(destination: &Path) -> Result<(PathBuf, File), DownloadError> {
    let parent = destination
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let name = destination
        .file_name()
        .and_then(|n| n.to_str())
        .filter(|name| !name.is_empty())
        .ok_or_else(|| DownloadError::InvalidDestination(destination.to_path_buf()))?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    for attempt in 0..128_u32 {
        let path = parent.join(format!(
            ".{name}.fetchpath-{nonce}-{}-{attempt}.part",
            std::process::id()
        ));
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(storage(&path, error)),
        }
    }
    Err(DownloadError::Storage {
        path: destination.to_path_buf(),
        detail: "could not reserve a unique staging name".into(),
        staging: None,
    })
}

/// Streams one HTTP representation to a same-directory staging file, then atomically
/// claims the destination with a hard link. The link is create-only, so a concurrent
/// destination creator wins and is never replaced.
pub fn download(request: DownloadRequest) -> Result<DownloadedFile, DownloadError> {
    if !(request.url.starts_with("http://") || request.url.starts_with("https://")) {
        return Err(DownloadError::InvalidUrl);
    }
    if request.destination.exists() {
        return Err(DownloadError::DestinationExists {
            destination: request.destination,
            staging: None,
        });
    }
    let mut easy = Easy::new();
    easy.url(&request.url)
        .map_err(|error| DownloadError::Transport {
            detail: error.to_string(),
            staging: None,
        })?;
    easy.follow_location(true)
        .map_err(|error| DownloadError::Transport {
            detail: error.to_string(),
            staging: None,
        })?;
    easy.fail_on_error(true)
        .map_err(|error| DownloadError::Transport {
            detail: error.to_string(),
            staging: None,
        })?;
    easy.ssl_verify_peer(true)
        .map_err(|error| DownloadError::Transport {
            detail: error.to_string(),
            staging: None,
        })?;
    easy.ssl_verify_host(true)
        .map_err(|error| DownloadError::Transport {
            detail: error.to_string(),
            staging: None,
        })?;
    easy.buffer_size(BUFFER_BYTES)
        .map_err(|error| DownloadError::Transport {
            detail: error.to_string(),
            staging: None,
        })?;
    easy.progress(true)
        .map_err(|error| DownloadError::Transport {
            detail: error.to_string(),
            staging: None,
        })?;

    let (staging, file) = unique_staging(&request.destination)?;
    if request.cancellation.is_cancelled() {
        return Err(cancelled(staging, request.cancel_cleanup));
    }
    let received = Arc::new(AtomicU64::new(0));
    let mut file = file;
    let mut hasher = Sha256::new();
    let write_error = Arc::new(Mutex::new(None));
    let result = (|| {
        let count = Arc::clone(&received);
        let token = request.cancellation.clone();
        let write_token = token.clone();
        let write_error_slot = Arc::clone(&write_error);
        let mut transfer = easy.transfer();
        transfer.write_function(|data| {
            if let Err(error) = file.write_all(data) {
                *write_error_slot.lock().expect("write error slot poisoned") = Some(error);
                // A short write aborts the transfer; Pause would wait forever
                // without an explicit unpause operation.
                return Ok(0);
            }
            hasher.update(data);
            let total = count.fetch_add(data.len() as u64, Ordering::Relaxed) + data.len() as u64;
            write_token.set_received(total);
            Ok(data.len())
        })?;
        transfer.progress_function(move |_, _, _, _| !token.is_cancelled())?;
        transfer.perform()
    })();
    if let Err(error) = result {
        drop(file);
        if let Some(write_error) = write_error
            .lock()
            .expect("write error slot poisoned")
            .take()
        {
            let retained = cleanup_staging(&staging);
            return Err(DownloadError::Storage {
                path: staging,
                detail: write_error.to_string(),
                staging: retained,
            });
        }
        if request.cancellation.is_cancelled() {
            return Err(cancelled(staging, request.cancel_cleanup));
        }
        if let Ok(response_code) = easy.response_code()
            && response_code >= 400
        {
            return Err(transport(format!("HTTP status {response_code}"), &staging));
        }
        return Err(transport(error.to_string(), &staging));
    }
    let response_code = match easy.response_code() {
        Ok(code) => code,
        Err(error) => {
            drop(file);
            return Err(transport(error.to_string(), &staging));
        }
    };
    if response_code != 200 {
        drop(file);
        return Err(transport(format!("HTTP status {response_code}"), &staging));
    }
    if let Err(error) = file.flush() {
        drop(file);
        let retained = cleanup_staging(&staging);
        return Err(DownloadError::Storage {
            path: staging,
            detail: error.to_string(),
            staging: retained,
        });
    }
    if let Err(error) = file.sync_all() {
        drop(file);
        let retained = cleanup_staging(&staging);
        return Err(DownloadError::Storage {
            path: staging,
            detail: error.to_string(),
            staging: retained,
        });
    }
    drop(file);
    // This serializes the final cancellation check with the hard-link publication
    // fence. Cancellation wins before this point; publication wins once linked.
    let gate = request.cancellation.publication_gate();
    if request.cancellation.is_cancelled() {
        drop(gate);
        return Err(cancelled(staging, request.cancel_cleanup));
    }
    if let Err(error) = fs::hard_link(&staging, &request.destination) {
        drop(gate);
        let retained = cleanup_staging(&staging);
        if error.kind() == std::io::ErrorKind::AlreadyExists {
            return Err(DownloadError::DestinationExists {
                destination: request.destination,
                staging: retained,
            });
        }
        return Err(DownloadError::Storage {
            path: request.destination,
            detail: error.to_string(),
            staging: retained,
        });
    }
    request.cancellation.mark_published();
    let staging_cleanup_pending = cleanup_staging(&staging);
    drop(gate);
    Ok(DownloadedFile {
        destination: request.destination,
        bytes: received.load(Ordering::Relaxed),
        observed_sha256: format!("{:x}", hasher.finalize()),
        staging_cleanup_pending,
    })
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
    pub destination: Option<PathBuf>,
    pub observed_sha256: Option<String>,
    pub staging_cleanup_pending: Option<PathBuf>,
    pub error: Option<String>,
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
        let cancellation = CancellationToken::default();
        let id = uuid::Uuid::new_v4().to_string();
        let snapshot = FileJobSnapshot {
            job_id: id.clone(),
            job_revision: 1,
            state: FileJobState::Queued,
            bytes_received: 0,
            destination: Some(destination.clone()),
            observed_sha256: None,
            staging_cleanup_pending: None,
            error: None,
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
                    cancel_cleanup: CancelCleanup::RemoveStaging,
                }),
                worker: None,
            })),
            cancellation,
        }
    }
    pub fn start(&self) -> Result<(), &'static str> {
        let mut inner = self.inner.lock().unwrap();
        if inner.snapshot.state != FileJobState::Queued {
            return Err("contract.invalid_transition");
        }
        let request = inner.request.take().unwrap();
        transition(&mut inner, FileJobState::Running);
        let shared = self.inner.clone();
        let token = self.cancellation.clone();
        inner.worker = Some(std::thread::spawn(move || {
            let result = download(request);
            let mut state = shared.lock().unwrap();
            if !matches!(
                state.snapshot.state,
                FileJobState::Running | FileJobState::Cancelling
            ) {
                return;
            }
            state.snapshot.bytes_received = token.received();
            match result {
                Ok(done) => {
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
                        DownloadError::InvalidUrl | DownloadError::InvalidDestination(_) => None,
                    };
                    state.snapshot.error = Some(error.to_string());
                    transition(&mut state, FileJobState::Failed);
                }
            };
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
    pub fn snapshot(&self) -> FileJobSnapshot {
        let mut state = self.inner.lock().unwrap();
        if matches!(
            state.snapshot.state,
            FileJobState::Running | FileJobState::Cancelling
        ) {
            state.snapshot.bytes_received = self.cancellation.received();
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
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;
    use std::time::Duration;

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
        }
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
            let (url, server) = server(vec![7; 512 * 1024], true);
            let token = CancellationToken::default();
            let trigger = token.clone();
            let canceller = thread::spawn(move || {
                thread::sleep(Duration::from_millis(30));
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
}
