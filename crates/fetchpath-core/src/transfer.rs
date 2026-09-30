use crate::checkpoint::{
    CommitQueue, PrefixTracker, ResponseHeaders, source_key_with_context, strong_etag,
};
use crate::{BUFFER_BYTES, CancelCleanup, DownloadError, DownloadRequest, DownloadedFile};
use curl::easy::Easy;
use fetchpath_http::{
    Chunk, GlobalBudget, RequestContext as HttpRequestContext, ResumePoint, Scheduler,
    TransferError, TransferLimits, transfer_resumable,
};
use fetchpath_storage::{
    CheckpointPhase, CheckpointRecord, CheckpointStore, FaultInjector, NoFaults,
    PublicationRecovery, sha256_file,
};
use std::cell::RefCell;
use std::fs::File;
use std::io;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// A checkpoint needs at least this many more contiguous bytes...
const CHECKPOINT_BYTES: u64 = 64 * 1024;
/// ...and at least this much time since the last one.
const CHECKPOINT_INTERVAL: Duration = Duration::from_secs(1);
static HTTP_BUDGET: OnceLock<GlobalBudget> = OnceLock::new();

pub(crate) fn http_budget() -> &'static GlobalBudget {
    let limits = TransferLimits::default();
    // Unit tests run many downloads in one process. The real budget would
    // split its lanes among them and hide what each test is about.
    let requests = if cfg!(test) {
        limits.max_active_requests * 8
    } else {
        limits.max_active_requests
    };
    HTTP_BUDGET.get_or_init(|| {
        GlobalBudget::new(
            requests,
            limits.max_buffered_bytes * (requests / limits.max_active_requests),
        )
        .expect("default HTTP transfer budget must be valid")
    })
}

enum CallbackFailure {
    RestartRequired,
    Cancelled,
    /// The source answered in a way a second attempt will not change.
    Rejected(String),
    Transport(String),
    Storage(io::Error),
}

struct AttemptResult {
    headers: ResponseHeaders,
    final_len: u64,
}

pub(crate) fn download(request: DownloadRequest) -> Result<DownloadedFile, DownloadError> {
    download_with_faults(request, &NoFaults)
}

pub fn download_with_faults(
    request: DownloadRequest,
    faults: &dyn FaultInjector,
) -> Result<DownloadedFile, DownloadError> {
    if !(request.url.starts_with("http://") || request.url.starts_with("https://")) {
        return Err(DownloadError::InvalidUrl);
    }
    let context_fingerprint = request.context.fingerprint();
    let key = source_key_with_context(&request.url, &context_fingerprint);
    let store = CheckpointStore::new(&request.destination, &key).map_err(|error| {
        if error.kind() == io::ErrorKind::InvalidInput {
            DownloadError::InvalidDestination(request.destination.clone())
        } else {
            DownloadError::Storage {
                path: request.destination.clone(),
                detail: error.to_string(),
                staging: None,
            }
        }
    })?;
    let latest = store
        .latest()
        .map_err(|error| storage_error(&store, error))?;

    if let Some(record) = latest.as_ref()
        && record.phase == CheckpointPhase::PublicationIntent
    {
        match store
            .reconcile_publication(record)
            .map_err(|error| storage_error(&store, error))?
        {
            PublicationRecovery::Completed => {
                request.cancellation.mark_published();
                if let Some(expected) = mismatch(&request, &record.local_sha256) {
                    return Err(DownloadError::ChecksumMismatch {
                        expected,
                        observed: record.local_sha256.clone(),
                        published: Some(request.destination.clone()),
                    });
                }
                return Ok(completed(&request, record, None));
            }
            PublicationRecovery::Conflict => {
                return Err(DownloadError::DestinationExists {
                    destination: request.destination,
                    staging: Some(store.staging().to_path_buf()),
                });
            }
            PublicationRecovery::Pending => {
                if !store
                    .validate_staging(record)
                    .map_err(|error| storage_error(&store, error))?
                {
                    return Err(storage_detail(
                        &store,
                        io::Error::new(
                            io::ErrorKind::InvalidData,
                            "publication intent does not match retained staging bytes",
                        ),
                    ));
                }
                if let Some(expected) = mismatch(&request, &record.local_sha256) {
                    let _ = store.remove_all();
                    return Err(DownloadError::ChecksumMismatch {
                        expected,
                        observed: record.local_sha256.clone(),
                        published: None,
                    });
                }
                return publish(&request, &store, record, faults);
            }
            PublicationRecovery::NotPending => unreachable!(),
        }
    }

    if request.destination.exists() {
        return Err(DownloadError::DestinationExists {
            destination: request.destination,
            staging: latest.map(|_| store.staging().to_path_buf()),
        });
    }

    let (mut file, mut offset, mut validator) = recover_download(&store, latest.as_ref())?;
    let mut expected_total = latest
        .as_ref()
        .filter(|_| offset > 0)
        .and_then(|record| record.expected_total);
    request.cancellation.set_received(offset);
    // A recovered checkpoint already carries the length the source stated on the
    // first attempt, so a resumed transfer shows its size before the first byte
    // of this attempt arrives instead of blanking out and filling in later.
    request
        .cancellation
        .set_total(latest.as_ref().and_then(|record| record.expected_total));
    if request.cancellation.is_cancelled() {
        return Err(cancelled(&request, &store));
    }

    // At most two attempts, and one restart from byte zero when the source is
    // not the representation the retained bytes came from. The stream-first
    // scheduler runs every attempt, resumed ones too.
    let mut identity_restarted = false;
    let mut attempts = 0;
    let attempt = loop {
        // A complete retained prefix still gets the final disk digest and
        // publication checks, without asking the origin for an empty range.
        if expected_total == Some(offset) {
            break AttemptResult {
                headers: ResponseHeaders {
                    status: Some(200),
                    etag: validator.clone(),
                    content_length: expected_total,
                    content_range: None,
                },
                final_len: offset,
            };
        }
        attempts += 1;
        let failure = match perform_scheduled_attempt(
            &request,
            &store,
            &file,
            offset,
            validator.as_deref(),
            expected_total,
            identity_restarted,
            faults,
        ) {
            Ok(attempt) => break attempt,
            Err(failure) => failure,
        };
        match failure {
            CallbackFailure::RestartRequired if !identity_restarted && attempts < 2 => {
                identity_restarted = true;
                file = store
                    .reset()
                    .map_err(|error| storage_error(&store, error))?;
                offset = 0;
                validator = None;
                expected_total = None;
                request.cancellation.set_received(0);
            }
            CallbackFailure::Transport(_) if attempts < 2 => {
                // Resume from the committed prefix, or from zero when there is none.
                let latest = store
                    .latest()
                    .map_err(|error| storage_error(&store, error))?;
                (file, offset, validator) = recover_download(&store, latest.as_ref())?;
                expected_total = latest
                    .as_ref()
                    .filter(|_| offset > 0)
                    .and_then(|record| record.expected_total);
                request.cancellation.set_received(offset);
            }
            failure => return Err(map_attempt_failure(&request, &store, failure)),
        }
    };

    store
        .sync_payload(&mut file, faults)
        .map_err(|error| storage_detail(&store, error))?;
    drop(file);
    let digest = sha256_file(store.staging()).map_err(|error| storage_detail(&store, error))?;
    if let Some(expected) = mismatch(&request, &digest) {
        // Known-bad bytes are discarded, not retained for a resume to build on.
        let _ = store.remove_all();
        return Err(DownloadError::ChecksumMismatch {
            expected,
            observed: digest,
            published: None,
        });
    }
    let response_validator = strong_etag(attempt.headers.etag.as_deref());
    let expected_total = response_total(&attempt.headers, attempt.final_len);
    let mut intent = CheckpointRecord::downloading(
        key,
        attempt.final_len,
        digest,
        response_validator,
        expected_total,
    );
    intent.phase = CheckpointPhase::PublicationIntent;
    let intent = store
        .commit(intent, faults)
        .map_err(|error| storage_detail(&store, error))?;
    publish(&request, &store, &intent, faults)
}

fn recover_download(
    store: &CheckpointStore,
    latest: Option<&CheckpointRecord>,
) -> Result<(File, u64, Option<String>), DownloadError> {
    let Some(record) = latest else {
        return store
            .reset()
            .map(|file| (file, 0, None))
            .map_err(|error| storage_error(store, error));
    };
    let reusable = record.phase == CheckpointPhase::Downloading
        && record.strong_etag.is_some()
        && store
            .validate_staging(record)
            .map_err(|error| storage_error(store, error))?;
    if !reusable {
        return store
            .reset()
            .map(|file| (file, 0, None))
            .map_err(|error| storage_error(store, error));
    }
    store
        .open_staging()
        .map(|file| (file, record.committed_len, record.strong_etag.clone()))
        .map_err(|error| storage_error(store, error))
}

/// Downloads the file, or the rest of it after `offset`, with the
/// stream-first scheduler. Lanes deliver bytes out of order; they are written
/// where they belong and folded into the prefix digest in order.
#[allow(clippy::too_many_arguments)]
fn perform_scheduled_attempt(
    request: &DownloadRequest,
    store: &CheckpointStore,
    file: &File,
    offset: u64,
    validator: Option<&str>,
    expected_total: Option<u64>,
    plain_restart: bool,
    faults: &dyn FaultInjector,
) -> Result<AttemptResult, CallbackFailure> {
    let mut limits = TransferLimits::default();
    if let Some(connections) = request.max_connections {
        limits.max_concurrency = connections.clamp(1, limits.max_active_requests);
    }
    if plain_restart && offset == 0 {
        // Restarting after an identity change: one plain GET, no ranges. A
        // later attempt that resumes from a checkpoint asks for a range again.
        limits.scheduler = Scheduler::Plain;
    }
    let context = HttpRequestContext {
        cookie_lines: request.context.cookie_lines.clone(),
        referer: request.context.referer.clone(),
        http2_prior_knowledge: false,
    };
    let resume = match (offset > 0, validator) {
        (true, Some(strong_etag)) => Some(ResumePoint {
            offset,
            strong_etag: strong_etag.to_owned(),
            expected_total,
        }),
        _ => None,
    };
    // The digest of the bytes kept so far, extended as bytes arrive rather
    // than recomputed from the whole staging file at every checkpoint.
    let tracker = RefCell::new(PrefixTracker::new(
        offset,
        prefix_digest(store.staging(), offset).map_err(CallbackFailure::Storage)?,
    ));
    let key = source_key_with_context(&request.url, &request.context.fingerprint());
    let queue = CommitQueue::new(offset);
    // The validator and total the newest chunk carried, for the final commit.
    let identity = RefCell::new((validator.map(str::to_owned), expected_total));
    let mut last_submit = (Instant::now(), offset);

    let result = std::thread::scope(|scope| {
        let worker = scope.spawn(|| queue.run(store, faults));
        let result = transfer_resumable(
            &request.url,
            &context,
            limits,
            http_budget(),
            request.cancellation.segment_monitor(),
            resume.as_ref(),
            || request.cancellation.is_cancelled(),
            |chunk: Chunk<'_>| {
                if queue.failed() {
                    return Err(queue
                        .take_error()
                        .unwrap_or_else(|| io::Error::other("checkpoint thread stopped")));
                }
                let mut tracker = tracker.borrow_mut();
                tracker.check(chunk.offset, chunk.bytes.len())?;
                store.write_payload_at(file, chunk.offset, chunk.bytes, faults)?;
                tracker.record(file, chunk.offset, chunk.bytes)?;
                let prefix = tracker.prefix();
                request.cancellation.set_received(prefix);
                request.cancellation.set_total(chunk.total_bytes);
                if let Some(etag) = chunk.strong_etag {
                    let mut identity = identity.borrow_mut();
                    if identity.0.as_deref() != Some(etag) {
                        identity.0 = Some(etag.to_owned());
                    }
                    identity.1 = chunk.total_bytes.or(identity.1);
                    // Checkpoint when a second has passed and 64 KiB more
                    // are contiguous. The commit thread fsyncs and commits.
                    if prefix - last_submit.1 >= CHECKPOINT_BYTES
                        && last_submit.0.elapsed() >= CHECKPOINT_INTERVAL
                    {
                        queue.submit(CheckpointRecord::downloading(
                            key.clone(),
                            prefix,
                            tracker.digest(),
                            identity.0.clone(),
                            identity.1,
                        ));
                        last_submit = (Instant::now(), prefix);
                    }
                }
                Ok(())
            },
        );
        // Every lane has stopped by now. Let the commit thread finish.
        queue.close();
        let _ = worker.join();
        result
    });

    // All handles and the commit worker are quiescent. Removal takes
    // precedence over a concurrent checkpoint error when cancel was requested.
    if request.cancellation.is_cancelled() && request.cancel_cleanup == CancelCleanup::RemoveStaging
    {
        return Err(CallbackFailure::Cancelled);
    }
    if let Some(error) = queue.take_error() {
        return Err(CallbackFailure::Storage(error));
    }
    match result {
        Ok(report) => {
            let tracker = tracker.borrow();
            if tracker.prefix() != report.bytes || tracker.written() != report.bytes {
                return Err(CallbackFailure::Storage(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "the transfer ended at {} bytes but {} are contiguous and {} written",
                        report.bytes,
                        tracker.prefix(),
                        tracker.written()
                    ),
                )));
            }
            Ok(AttemptResult {
                headers: ResponseHeaders {
                    status: Some(200),
                    etag: report.strong_etag,
                    content_length: report.total_bytes,
                    content_range: None,
                },
                final_len: report.bytes,
            })
        }
        Err(TransferError::Cancelled) => {
            // Stop lanes (done), sync, commit the prefix. A pause keeps the
            // staging file, so what it holds is worth committing; a cancel
            // that removes it is not.
            if request.cancel_cleanup == CancelCleanup::RetainStaging {
                let tracker = tracker.borrow();
                let (etag, total) = identity.borrow().clone();
                if let Some(etag) = etag
                    && tracker.prefix() > queue.committed()
                {
                    store
                        .sync_payload_shared(file, faults)
                        .map_err(CallbackFailure::Storage)?;
                    store
                        .commit(
                            CheckpointRecord::downloading(
                                key,
                                tracker.prefix(),
                                tracker.digest(),
                                Some(etag),
                                total,
                            ),
                            faults,
                        )
                        .map_err(CallbackFailure::Storage)?;
                }
            }
            Err(CallbackFailure::Cancelled)
        }
        Err(TransferError::Sink(error)) => Err(CallbackFailure::Storage(error)),
        Err(TransferError::RestartSequential(_) | TransferError::IdentityChanged(_)) => {
            Err(CallbackFailure::RestartRequired)
        }
        Err(TransferError::Rejected(detail)) => Err(CallbackFailure::Rejected(detail)),
        Err(error) => Err(CallbackFailure::Transport(error.to_string())),
    }
}

/// SHA-256 state over the first `len` bytes of `path`.
fn prefix_digest(path: &std::path::Path, len: u64) -> io::Result<sha2::Sha256> {
    use std::io::Read;
    let mut digest = <sha2::Sha256 as sha2::Digest>::new();
    if len == 0 {
        return Ok(digest);
    }
    let mut reader = File::open(path)?.take(len);
    let mut buffer = vec![0_u8; 1024 * 1024];
    let mut read = 0_u64;
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        sha2::Digest::update(&mut digest, &buffer[..count]);
        read += count as u64;
    }
    if read != len {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "staging is shorter than its checkpoint",
        ));
    }
    Ok(digest)
}

pub(crate) fn configure(
    easy: &mut Easy,
    url: &str,
    context: &crate::RequestContext,
) -> Result<(), curl::Error> {
    easy.url(url)?;
    easy.follow_location(true)?;
    easy.fail_on_error(true)?;
    easy.ssl_verify_peer(true)?;
    easy.ssl_verify_host(true)?;
    for cookie in &context.cookie_lines {
        easy.cookie_list(cookie)?;
    }
    if let Some(referer) = &context.referer {
        easy.referer(referer)?;
    }
    easy.buffer_size(BUFFER_BYTES)?;
    easy.progress(true)
}

fn response_total(headers: &ResponseHeaders, fallback: u64) -> Option<u64> {
    if headers.status == Some(206) {
        headers.content_range.and_then(|range| range.total)
    } else {
        headers.content_length.or(Some(fallback))
    }
}

/// The expected checksum, when one was supplied and `observed` differs from it.
fn mismatch(request: &DownloadRequest, observed: &str) -> Option<String> {
    request
        .expected_sha256
        .as_deref()
        .filter(|expected| !expected.eq_ignore_ascii_case(observed))
        .map(str::to_owned)
}

pub(crate) fn publish(
    request: &DownloadRequest,
    store: &CheckpointStore,
    intent: &CheckpointRecord,
    faults: &dyn FaultInjector,
) -> Result<DownloadedFile, DownloadError> {
    let gate = request.cancellation.publication_gate();
    if request.cancellation.is_cancelled() {
        drop(gate);
        return Err(cancelled(request, store));
    }
    match store.publish(intent, faults) {
        Ok(staging_pending) => {
            request.cancellation.mark_published();
            drop(gate);
            Ok(completed(request, intent, staging_pending))
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            drop(gate);
            let _ = store.remove_all();
            Err(DownloadError::DestinationExists {
                destination: request.destination.clone(),
                staging: store
                    .staging()
                    .exists()
                    .then(|| store.staging().to_path_buf()),
            })
        }
        Err(error) => {
            if request.destination.exists() {
                request.cancellation.mark_published();
            }
            drop(gate);
            Err(storage_detail(store, error))
        }
    }
}

fn completed(
    request: &DownloadRequest,
    record: &CheckpointRecord,
    staging_cleanup_pending: Option<std::path::PathBuf>,
) -> DownloadedFile {
    DownloadedFile {
        destination: request.destination.clone(),
        bytes: record.committed_len,
        observed_sha256: record.local_sha256.clone(),
        staging_cleanup_pending,
    }
}

pub(crate) fn cancelled(request: &DownloadRequest, store: &CheckpointStore) -> DownloadError {
    let staging = match request.cancel_cleanup {
        CancelCleanup::RetainStaging => Some(store.staging().to_path_buf()),
        CancelCleanup::RemoveStaging => {
            let _ = store.remove_all();
            store
                .staging()
                .exists()
                .then(|| store.staging().to_path_buf())
        }
    };
    DownloadError::Cancelled { staging }
}

fn storage_error(store: &CheckpointStore, error: io::Error) -> DownloadError {
    storage_detail(store, error)
}

pub(crate) fn storage_detail(store: &CheckpointStore, error: io::Error) -> DownloadError {
    DownloadError::Storage {
        path: store.staging().to_path_buf(),
        detail: error.to_string(),
        staging: store
            .staging()
            .exists()
            .then(|| store.staging().to_path_buf()),
    }
}

fn map_attempt_failure(
    request: &DownloadRequest,
    store: &CheckpointStore,
    failure: CallbackFailure,
) -> DownloadError {
    match failure {
        CallbackFailure::Cancelled => cancelled(request, store),
        CallbackFailure::RestartRequired => {
            let _ = store.remove_all();
            DownloadError::Transport {
                detail: "source refused a safe byte-zero restart".into(),
                staging: store
                    .staging()
                    .exists()
                    .then(|| store.staging().to_path_buf()),
            }
        }
        CallbackFailure::Transport(detail) | CallbackFailure::Rejected(detail) => {
            let reusable = store.latest().ok().flatten().is_some();
            if !reusable {
                let _ = store.remove_all();
            }
            DownloadError::Transport {
                detail,
                staging: store
                    .staging()
                    .exists()
                    .then(|| store.staging().to_path_buf()),
            }
        }
        CallbackFailure::Storage(error) => storage_detail(store, error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CancelCleanup, CancellationToken};
    use fetchpath_storage::{FaultPoint, NoFaults};
    use sha2::{Digest, Sha256};
    use std::fs;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    #[derive(Clone)]
    enum Reply {
        Full {
            body: Vec<u8>,
            etag: &'static str,
        },
        Range {
            body: Vec<u8>,
            etag: &'static str,
            truncate: bool,
        },
    }

    struct FailOnce(Mutex<Option<FaultPoint>>);

    impl FaultInjector for FailOnce {
        fn check(&self, point: FaultPoint) -> io::Result<()> {
            let mut selected = self.0.lock().unwrap();
            if selected.as_ref() == Some(&point) {
                *selected = None;
                return Err(if point == FaultPoint::PayloadWrite {
                    io::Error::new(io::ErrorKind::StorageFull, "injected disk full")
                } else {
                    io::Error::other("injected fault")
                });
            }
            Ok(())
        }
    }

    fn temp_dir(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "fetchpath-resume-{label}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn request(url: String, destination: PathBuf) -> DownloadRequest {
        DownloadRequest {
            url,
            destination,
            cancellation: CancellationToken::default(),
            cancel_cleanup: CancelCleanup::RemoveStaging,
            context: crate::RequestContext::default(),
            expected_sha256: None,
            max_connections: None,
        }
    }

    fn read_request(stream: &mut TcpStream) -> String {
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut request = Vec::new();
        let mut buffer = [0_u8; 1024];
        while !request.windows(4).any(|window| window == b"\r\n\r\n") {
            let read = stream.read(&mut buffer).unwrap();
            if read == 0 {
                break;
            }
            request.extend_from_slice(&buffer[..read]);
        }
        String::from_utf8(request).unwrap()
    }

    fn range_start(request: &str) -> Option<usize> {
        request.lines().find_map(|line| {
            let (name, value) = line.split_once(':')?;
            if !name.eq_ignore_ascii_case("range") {
                return None;
            }
            value
                .trim()
                .strip_prefix("bytes=")?
                .strip_suffix('-')?
                .parse()
                .ok()
        })
    }

    fn server(replies: Vec<Reply>) -> (String, Arc<Mutex<Vec<String>>>, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = Arc::clone(&requests);
        let worker = thread::spawn(move || {
            for reply in replies {
                let (mut stream, _) = listener.accept().unwrap();
                let request = read_request(&mut stream);
                captured.lock().unwrap().push(request.clone());
                let (status, body, etag, content_range, declared_len) = match reply {
                    Reply::Full { body, etag } => {
                        let len = body.len();
                        (200, body, etag, None, len)
                    }
                    Reply::Range {
                        body,
                        etag,
                        truncate,
                    } => {
                        let start = range_start(&request).expect("range request expected");
                        let full_len = body.len();
                        let suffix = body[start..].to_vec();
                        let declared = suffix.len();
                        let sent = if truncate {
                            suffix[..suffix.len() / 2].to_vec()
                        } else {
                            suffix
                        };
                        (
                            206,
                            sent,
                            etag,
                            Some(format!("bytes {start}-{}/{full_len}", full_len - 1)),
                            declared,
                        )
                    }
                };
                let mut headers = format!(
                    "HTTP/1.1 {status} Test\r\nETag: {etag}\r\nContent-Length: {declared_len}\r\nConnection: close\r\n"
                );
                if let Some(content_range) = content_range {
                    headers.push_str(&format!("Content-Range: {content_range}\r\n"));
                }
                headers.push_str("\r\n");
                let _ = stream.write_all(headers.as_bytes());
                for chunk in body.chunks(4096) {
                    if stream.write_all(chunk).is_err() {
                        break;
                    }
                }
            }
        });
        (format!("http://{address}/fixture"), requests, worker)
    }

    /// The first range size the adaptive path uses.
    fn segment() -> usize {
        fetchpath_http::TransferLimits::default().min_segment_bytes
    }

    /// One request as a `range_server` behaviour sees it.
    struct Req {
        /// Index of the request, from zero, across all connections.
        index: usize,
        range: Option<(usize, Option<usize>)>,
    }

    /// What to send for one request. Fields default to a healthy 206.
    struct Answer {
        status: u16,
        etag: Option<&'static str>,
        /// Content-Range as text, overriding the honest one.
        content_range: Option<String>,
        body: Vec<u8>,
        /// Pause between writes of `chunk` bytes.
        pace: Duration,
        chunk: usize,
        /// Stop sending after this many body bytes, then close.
        cut_after: Option<usize>,
        /// Send this many body bytes, then wait for `hold`'s release.
        hold_after: Option<usize>,
    }

    fn honest(body: &[u8], etag: &'static str, req: &Req) -> Answer {
        match req.range {
            Some((start, end)) => {
                let end = end.unwrap_or(body.len() - 1).min(body.len() - 1);
                Answer {
                    status: 206,
                    etag: Some(etag),
                    content_range: Some(format!("bytes {start}-{end}/{}", body.len())),
                    body: body[start..=end].to_vec(),
                    pace: Duration::ZERO,
                    chunk: 4096,
                    cut_after: None,
                    hold_after: None,
                }
            }
            None => whole(body, etag),
        }
    }

    /// A 200 with the whole body, as a source that ignores ranges sends.
    fn whole(body: &[u8], etag: &'static str) -> Answer {
        Answer {
            status: 200,
            etag: Some(etag),
            content_range: None,
            body: body.to_vec(),
            pace: Duration::ZERO,
            chunk: 4096,
            cut_after: None,
            hold_after: None,
        }
    }

    struct RangeServer {
        url: String,
        requests: Arc<Mutex<Vec<String>>>,
        stop: Arc<std::sync::atomic::AtomicBool>,
        /// Lets answers that `hold_after` continue.
        release: Arc<std::sync::atomic::AtomicBool>,
        worker: Option<thread::JoinHandle<()>>,
    }

    impl RangeServer {
        fn requests(&self) -> Vec<String> {
            self.requests.lock().unwrap().clone()
        }
    }

    impl Drop for RangeServer {
        fn drop(&mut self) {
            self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
            if let Some(worker) = self.worker.take() {
                let _ = worker.join();
            }
        }
    }

    /// A server that answers every connection on its own thread, so lanes
    /// can run at once.
    fn range_server(behaviour: impl Fn(&Req) -> Answer + Send + Sync + 'static) -> RangeServer {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}/fixture", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let behaviour = Arc::new(behaviour);
        let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let release = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (log, flag, released) = (
            Arc::clone(&requests),
            Arc::clone(&stop),
            Arc::clone(&release),
        );
        let worker = thread::spawn(move || {
            let mut handlers = Vec::new();
            while !flag.load(std::sync::atomic::Ordering::SeqCst) {
                let Ok((mut stream, _)) = listener.accept() else {
                    thread::sleep(Duration::from_millis(2));
                    continue;
                };
                let (behaviour, log, count, flag, released) = (
                    Arc::clone(&behaviour),
                    Arc::clone(&log),
                    Arc::clone(&count),
                    Arc::clone(&flag),
                    Arc::clone(&released),
                );
                handlers.push(thread::spawn(move || {
                    stream.set_nonblocking(false).unwrap();
                    let Some(text) = try_read_request(&mut stream) else {
                        return;
                    };
                    let req = Req {
                        index: count.fetch_add(1, std::sync::atomic::Ordering::SeqCst),
                        range: parse_range(&text),
                    };
                    log.lock().unwrap().push(text);
                    let answer = behaviour(&req);
                    let reason = if answer.status == 206 {
                        "Partial Content"
                    } else {
                        "Test"
                    };
                    let mut head = format!(
                        "HTTP/1.1 {} {reason}\r\nContent-Length: {}\r\nConnection: close\r\n",
                        answer.status,
                        answer.body.len()
                    );
                    if let Some(etag) = answer.etag {
                        head.push_str(&format!("ETag: {etag}\r\n"));
                    }
                    if let Some(range) = &answer.content_range {
                        head.push_str(&format!("Content-Range: {range}\r\n"));
                    }
                    head.push_str("\r\n");
                    if stream.write_all(head.as_bytes()).is_err() {
                        return;
                    }
                    let limit = answer.cut_after.unwrap_or(answer.body.len());
                    for (index, chunk) in answer.body[..limit.min(answer.body.len())]
                        .chunks(answer.chunk)
                        .enumerate()
                    {
                        if flag.load(std::sync::atomic::Ordering::SeqCst) {
                            return;
                        }
                        if let Some(hold) = answer.hold_after
                            && index * answer.chunk >= hold
                        {
                            while !flag.load(std::sync::atomic::Ordering::SeqCst)
                                && !released.load(std::sync::atomic::Ordering::SeqCst)
                            {
                                thread::sleep(Duration::from_millis(2));
                            }
                        }
                        if stream.write_all(chunk).is_err() {
                            return;
                        }
                        if !answer.pace.is_zero() {
                            thread::sleep(answer.pace);
                        }
                    }
                }));
            }
            for handler in handlers {
                let _ = handler.join();
            }
        });
        RangeServer {
            url,
            requests,
            stop,
            release,
            worker: Some(worker),
        }
    }

    fn try_read_request(stream: &mut TcpStream) -> Option<String> {
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut request = Vec::new();
        let mut buffer = [0_u8; 1024];
        while !request.windows(4).any(|window| window == b"\r\n\r\n") {
            match stream.read(&mut buffer) {
                Ok(0) | Err(_) => return None,
                Ok(read) => request.extend_from_slice(&buffer[..read]),
            }
        }
        String::from_utf8(request).ok()
    }

    fn parse_range(request: &str) -> Option<(usize, Option<usize>)> {
        request.lines().find_map(|line| {
            let (name, value) = line.split_once(':')?;
            if !name.eq_ignore_ascii_case("range") {
                return None;
            }
            let (start, end) = value.trim().strip_prefix("bytes=")?.split_once('-')?;
            Some((start.parse().ok()?, end.parse().ok()))
        })
    }

    fn seed_checkpoint(url: &str, destination: &std::path::Path, body: &[u8], len: usize) {
        let key = crate::checkpoint::source_key(url);
        let store = CheckpointStore::new(destination, &key).unwrap();
        fs::write(store.staging(), &body[..len]).unwrap();
        let record = CheckpointRecord::downloading(
            key,
            len as u64,
            format!("{:x}", Sha256::digest(&body[..len])),
            Some("\"v1\"".into()),
            Some(body.len() as u64),
        );
        store.commit(record, &NoFaults).unwrap();
    }

    #[test]
    fn restart_reuses_only_a_matching_strong_validator_range() {
        let dir = temp_dir("matching-range");
        let destination = dir.join("file.bin");
        let body = vec![42; 192 * 1024];
        let (url, requests, worker) = server(vec![Reply::Range {
            body: body.clone(),
            etag: "\"v1\"",
            truncate: false,
        }]);
        seed_checkpoint(&url, &destination, &body, 96 * 1024);

        let done = download(request(url, destination.clone())).unwrap();
        worker.join().unwrap();
        assert_eq!(done.bytes, body.len() as u64);
        assert_eq!(fs::read(&destination).unwrap(), body);
        assert!(requests.lock().unwrap()[0].contains("Range: bytes=98304-"));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn changed_etag_forces_a_clean_full_restart() {
        let dir = temp_dir("changed-etag");
        let destination = dir.join("file.bin");
        let old_body = vec![1; 128 * 1024];
        let new_body = vec![2; 160 * 1024];
        let (url, requests, worker) = server(vec![
            Reply::Full {
                body: new_body.clone(),
                etag: "\"v2\"",
            },
            Reply::Full {
                body: new_body.clone(),
                etag: "\"v2\"",
            },
        ]);
        seed_checkpoint(&url, &destination, &old_body, 96 * 1024);

        let done = download(request(url, destination.clone())).unwrap();
        worker.join().unwrap();
        assert_eq!(done.bytes, new_body.len() as u64);
        assert_eq!(fs::read(&destination).unwrap(), new_body);
        let requests = requests.lock().unwrap();
        assert!(requests[0].contains("Range: bytes=98304-"));
        assert!(!requests[1].contains("Range:"));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn range_refusal_forces_a_clean_full_restart() {
        let dir = temp_dir("range-refused");
        let destination = dir.join("file.bin");
        let body = vec![3; 160 * 1024];
        let (url, requests, worker) = server(vec![
            Reply::Full {
                body: body.clone(),
                etag: "\"v1\"",
            },
            Reply::Full {
                body: body.clone(),
                etag: "\"v1\"",
            },
        ]);
        seed_checkpoint(&url, &destination, &body, 96 * 1024);

        download(request(url, destination.clone())).unwrap();
        worker.join().unwrap();
        let requests = requests.lock().unwrap();
        assert!(requests[0].contains("Range: bytes=98304-"));
        assert!(!requests[1].contains("Range:"));
        assert_eq!(fs::read(&destination).unwrap(), body);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn truncated_resume_never_publishes_and_keeps_the_last_checkpoint() {
        let dir = temp_dir("truncated-range");
        let destination = dir.join("file.bin");
        let body = vec![4; 160 * 1024];
        let (url, _, worker) = server(vec![Reply::Range {
            body: body.clone(),
            etag: "\"v1\"",
            truncate: true,
        }]);
        seed_checkpoint(&url, &destination, &body, 96 * 1024);
        let key = crate::checkpoint::source_key(&url);
        let store = CheckpointStore::new(&destination, &key).unwrap();

        assert!(matches!(
            download(request(url, destination.clone())),
            Err(DownloadError::Transport {
                staging: Some(_),
                ..
            })
        ));
        worker.join().unwrap();
        assert!(!destination.exists());
        let checkpoint = store.latest().unwrap().unwrap();
        assert_eq!(checkpoint.committed_len, 96 * 1024);
        assert!(store.validate_staging(&checkpoint).unwrap());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn injected_disk_full_flush_and_commit_faults_never_publish() {
        for point in [
            FaultPoint::PayloadWrite,
            FaultPoint::PayloadFlush,
            FaultPoint::MetadataCommit,
        ] {
            let dir = temp_dir("io-fault");
            let destination = dir.join("file.bin");
            let body = vec![5; 8 * 1024];
            let (url, _, worker) = server(vec![Reply::Full {
                body,
                etag: "\"v1\"",
            }]);
            let faults = FailOnce(Mutex::new(Some(point)));
            assert!(download_with_faults(request(url, destination.clone()), &faults).is_err());
            worker.join().unwrap();
            assert!(!destination.exists());
            fs::remove_dir_all(dir).unwrap();
        }
    }

    fn sha256_hex(bytes: &[u8]) -> String {
        use sha2::{Digest, Sha256};
        format!("{:x}", Sha256::digest(bytes))
    }

    fn checked(url: String, destination: PathBuf, expected: &str) -> DownloadRequest {
        DownloadRequest {
            expected_sha256: Some(expected.to_owned()),
            ..request(url, destination)
        }
    }

    #[test]
    fn a_matching_checksum_publishes() {
        let dir = temp_dir("checksum-match");
        let destination = dir.join("file.bin");
        let body = vec![3; 20 * 1024];
        let (url, _, worker) = server(vec![Reply::Full {
            body: body.clone(),
            etag: "\"v1\"",
        }]);
        let done = download(checked(url, destination.clone(), &sha256_hex(&body))).unwrap();
        worker.join().unwrap();
        assert_eq!(done.observed_sha256, sha256_hex(&body));
        assert_eq!(fs::read(&destination).unwrap(), body);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_mismatched_checksum_publishes_nothing_and_discards_the_bad_bytes() {
        let dir = temp_dir("checksum-mismatch");
        let destination = dir.join("file.bin");
        let body = vec![4; 20 * 1024];
        let (url, _, worker) = server(vec![Reply::Full {
            body: body.clone(),
            etag: "\"v1\"",
        }]);
        let wrong = sha256_hex(b"something else");
        let error = download(checked(url, destination.clone(), &wrong)).unwrap_err();
        worker.join().unwrap();
        match error {
            DownloadError::ChecksumMismatch {
                expected,
                observed,
                published,
            } => {
                assert_eq!(expected, wrong);
                assert_eq!(observed, sha256_hex(&body));
                assert_eq!(published, None);
            }
            other => panic!("expected a checksum mismatch, got {other:?}"),
        }
        assert!(!destination.exists());
        // Known-bad bytes are not kept for a resume to build on.
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 0);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_pending_publication_is_checked_before_it_is_completed_on_recovery() {
        let dir = temp_dir("checksum-pending");
        let destination = dir.join("file.bin");
        let body = vec![5; 8 * 1024];
        let (url, _, worker) = server(vec![Reply::Full {
            body: body.clone(),
            etag: "\"v1\"",
        }]);
        let faults = FailOnce(Mutex::new(Some(FaultPoint::PublicationFence)));
        assert!(download_with_faults(request(url.clone(), destination.clone()), &faults).is_err());
        worker.join().unwrap();
        assert!(
            !destination.exists(),
            "the fence failed, so nothing is published yet"
        );

        let wrong = sha256_hex(b"not these bytes");
        let error = download(checked(url.clone(), destination.clone(), &wrong)).unwrap_err();
        assert!(matches!(
            error,
            DownloadError::ChecksumMismatch {
                published: None,
                ..
            }
        ));
        assert!(!destination.exists());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn an_already_published_file_that_does_not_match_is_reported_not_deleted() {
        let dir = temp_dir("checksum-completed");
        let destination = dir.join("file.bin");
        let body = vec![6; 8 * 1024];
        let (url, _, worker) = server(vec![Reply::Full {
            body: body.clone(),
            etag: "\"v1\"",
        }]);
        let faults = FailOnce(Mutex::new(Some(FaultPoint::PublicationReconcile)));
        assert!(download_with_faults(request(url.clone(), destination.clone()), &faults).is_err());
        worker.join().unwrap();

        let wrong = sha256_hex(b"different");
        let error = download(checked(url, destination.clone(), &wrong)).unwrap_err();
        assert!(matches!(
            error,
            DownloadError::ChecksumMismatch { published: Some(ref path), .. } if path == &destination
        ));
        assert_eq!(
            fs::read(&destination).unwrap(),
            body,
            "a published file is never deleted"
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn restart_reconciles_a_publication_that_crossed_the_fence() {
        let dir = temp_dir("publication-restart");
        let destination = dir.join("file.bin");
        let body = vec![6; 8 * 1024];
        let (url, _, worker) = server(vec![Reply::Full {
            body: body.clone(),
            etag: "\"v1\"",
        }]);
        let faults = FailOnce(Mutex::new(Some(FaultPoint::PublicationReconcile)));
        assert!(download_with_faults(request(url.clone(), destination.clone()), &faults).is_err());
        worker.join().unwrap();
        assert_eq!(fs::read(&destination).unwrap(), body);

        let recovered = download(request(url, destination.clone())).unwrap();
        assert_eq!(recovered.bytes, body.len() as u64);
        assert_eq!(fs::read(&destination).unwrap(), body);
        fs::remove_dir_all(dir).unwrap();
    }

    fn patterned(len: usize) -> Vec<u8> {
        (0..len).map(|index| (index % 251) as u8).collect()
    }

    #[test]
    fn parallel_ranges_reassemble_under_the_core_publication_contract() {
        let dir = temp_dir("parallel-ranges");
        let destination = dir.join("file.bin");
        let body = patterned(6 * segment());
        let served = body.clone();
        let server = range_server(move |req| {
            let mut answer = honest(&served, "\"adaptive-v1\"", req);
            // About 30 MB/s per lane, so lanes overlap.
            answer.pace = Duration::from_millis(2);
            answer.chunk = 64 * 1024;
            answer
        });

        let done = download(request(server.url.clone(), destination.clone())).unwrap();

        assert_eq!(done.bytes, body.len() as u64);
        assert_eq!(fs::read(&destination).unwrap(), body);
        let requests = server.requests();
        assert!(
            requests[0].contains("Range: bytes=0-\r\n"),
            "no probe: the first request already asks for the file"
        );
        assert!(requests.len() > 1, "the file came on several lanes");
        assert!(
            requests[1..]
                .iter()
                .all(|request| request.contains("If-Range: \"adaptive-v1\"")),
            "every later range carries the validator"
        );
        let mut ranges: Vec<(usize, usize)> = requests[1..]
            .iter()
            .map(|request| {
                let (start, end) = parse_range(request).unwrap();
                (start, end.expect("later ranges are exact"))
            })
            .collect();
        ranges.sort_unstable();
        for pair in ranges.windows(2) {
            assert!(pair[0].1 < pair[1].0, "ranges do not overlap: {pair:?}");
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn segments_in_flight_are_observable_and_cleared_when_written() {
        let dir = temp_dir("adaptive-segments");
        let destination = dir.join("file.bin");
        let body = patterned(5 * segment());
        let served = body.clone();
        let server = range_server(move |req| {
            let mut answer = honest(&served, "\"segments-v1\"", req);
            answer.pace = Duration::from_millis(2);
            answer.chunk = 64 * 1024;
            if req.range.is_some_and(|(start, _)| start > 0) {
                // A later range stops half way until the test has looked.
                answer.hold_after = Some(answer.body.len() / 2);
            }
            answer
        });

        let download_request = request(server.url.clone(), destination.clone());
        let token = download_request.cancellation.clone();
        let worker = thread::spawn(move || download(download_request));
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let observed = loop {
            let segments = token.segment_monitor().snapshot();
            if segments
                .iter()
                .any(|range| range.start > 0 && range.received > 0)
            {
                break segments;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "segment progress never observed"
            );
            thread::sleep(Duration::from_millis(5));
        };
        let held = observed
            .iter()
            .find(|range| range.start > 0 && range.received > 0)
            .unwrap();
        assert!(held.end < body.len() as u64);
        // Written bytes count as received; the prefix is the contiguous part.
        assert!(token.received() <= body.len() as u64);
        server
            .release
            .store(true, std::sync::atomic::Ordering::SeqCst);

        let done = worker.join().unwrap().unwrap();
        assert_eq!(done.bytes, body.len() as u64);
        assert_eq!(fs::read(&destination).unwrap(), body);
        assert!(token.segment_monitor().snapshot().is_empty());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_resume_of_a_large_remainder_uses_ranges() {
        let dir = temp_dir("resume-ranges");
        let destination = dir.join("file.bin");
        let body = patterned(12 * segment());
        let served = body.clone();
        let server = range_server(move |req| honest(&served, "\"v1\"", req));
        seed_checkpoint(&server.url, &destination, &body, 2 * segment());

        let done = download(request(server.url.clone(), destination.clone())).unwrap();

        assert_eq!(done.bytes, body.len() as u64);
        assert_eq!(fs::read(&destination).unwrap(), body);
        let requests = server.requests();
        assert!(requests[0].contains(&format!("Range: bytes={}-\r\n", 2 * segment())));
        assert!(requests[0].contains("If-Range: \"v1\""));
        assert!(requests.len() > 1, "the remainder was split over lanes");
        fs::remove_dir_all(dir).unwrap();
    }

    fn seed_with_total(
        url: &str,
        destination: &std::path::Path,
        body: &[u8],
        len: usize,
        total: Option<u64>,
    ) {
        let key = crate::checkpoint::source_key(url);
        let store = CheckpointStore::new(destination, &key).unwrap();
        fs::write(store.staging(), &body[..len]).unwrap();
        let record = CheckpointRecord::downloading(
            key,
            len as u64,
            format!("{:x}", Sha256::digest(&body[..len])),
            Some("\"v1\"".into()),
            total,
        );
        store.commit(record, &NoFaults).unwrap();
    }

    #[test]
    fn a_total_that_differs_from_the_checkpoint_restarts_from_zero() {
        let dir = temp_dir("resume-total");
        let destination = dir.join("file.bin");
        let body = patterned(200 * 1024);
        let served = body.clone();
        let server = range_server(move |req| honest(&served, "\"v1\"", req));
        // The checkpoint remembers a different length, same validator.
        seed_with_total(
            &server.url,
            &destination,
            &body,
            96 * 1024,
            Some(body.len() as u64 + 1),
        );

        let done = download(request(server.url.clone(), destination.clone())).unwrap();

        assert_eq!(done.bytes, body.len() as u64);
        assert_eq!(fs::read(&destination).unwrap(), body);
        let requests = server.requests();
        assert_eq!(requests.len(), 2);
        assert!(requests[0].contains("Range: bytes=98304-"));
        assert!(
            !requests[1].contains("Range:"),
            "the restart is a plain request from zero"
        );
        fs::remove_dir_all(dir).unwrap();
    }

    /// Notes whether the checkpoint metadata still existed when the first
    /// payload byte was about to be written.
    struct ProbeFirstWrite {
        store: CheckpointStore,
        metadata_at_first_write: Mutex<Option<bool>>,
    }

    impl FaultInjector for ProbeFirstWrite {
        fn check(&self, point: FaultPoint) -> io::Result<()> {
            if point == FaultPoint::PayloadWrite {
                self.metadata_at_first_write
                    .lock()
                    .unwrap()
                    .get_or_insert_with(|| self.store.latest().unwrap().is_some());
            }
            Ok(())
        }
    }

    #[test]
    fn a_200_on_resume_resets_before_any_byte_is_written() {
        let dir = temp_dir("resume-200");
        let destination = dir.join("file.bin");
        let old = vec![1_u8; 160 * 1024];
        let new = vec![2_u8; 160 * 1024];
        let served = new.clone();
        // The source ignores the range and sends the whole (new) file.
        let server = range_server(move |_| whole(&served, "\"v1\""));
        seed_checkpoint(&server.url, &destination, &old, 96 * 1024);
        let key = crate::checkpoint::source_key(&server.url);
        let probe = ProbeFirstWrite {
            store: CheckpointStore::new(&destination, &key).unwrap(),
            metadata_at_first_write: Mutex::new(None),
        };

        download_with_faults(request(server.url.clone(), destination.clone()), &probe).unwrap();

        assert_eq!(fs::read(&destination).unwrap(), new);
        assert_eq!(
            *probe.metadata_at_first_write.lock().unwrap(),
            Some(false),
            "the retained prefix was discarded before the first new byte"
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_dropped_connection_resumes_from_the_committed_prefix() {
        let dir = temp_dir("dropped-connection");
        let destination = dir.join("file.bin");
        let body = patterned(3 * segment());
        let served = body.clone();
        let server = range_server(move |req| {
            if req.index == 0 {
                // A slow 200 that dies after two seconds of data.
                Answer {
                    pace: Duration::from_millis(15),
                    chunk: 16 * 1024,
                    cut_after: Some(segment() * 2 + segment() / 2),
                    ..whole(&served, "\"v1\"")
                }
            } else {
                honest(&served, "\"v1\"", req)
            }
        });

        let done = download(request(server.url.clone(), destination.clone())).unwrap();

        assert_eq!(fs::read(&destination).unwrap(), body);
        assert_eq!(done.bytes, body.len() as u64);
        let requests = server.requests();
        assert_eq!(requests.len(), 2, "one retry");
        let (resumed_at, _) = parse_range(&requests[1]).unwrap();
        assert!(
            resumed_at > 0,
            "the second attempt resumed from a committed prefix"
        );
        assert!(requests[1].contains("If-Range: \"v1\""));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_checkpoint_cadence_bounds_what_a_crash_discards() {
        let dir = temp_dir("cadence");
        let destination = dir.join("file.bin");
        // About 1 MiB per second for 4 seconds.
        let body = patterned(4 * segment());
        let served = body.clone();
        let server = range_server(move |_| Answer {
            pace: Duration::from_millis(15),
            chunk: 16 * 1024,
            ..whole(&served, "\"v1\"")
        });
        // A crash: writes start failing 2.6 seconds in, with no orderly stop.
        struct Crash(std::time::Instant);
        impl FaultInjector for Crash {
            fn check(&self, point: FaultPoint) -> io::Result<()> {
                if point == FaultPoint::PayloadWrite
                    && self.0.elapsed() > Duration::from_millis(2600)
                {
                    return Err(io::Error::other("crash"));
                }
                Ok(())
            }
        }
        let download_request = request(server.url.clone(), destination.clone());
        let token = download_request.cancellation.clone();
        let key = crate::checkpoint::source_key(&server.url);
        let result = download_with_faults(download_request, &Crash(std::time::Instant::now()));
        assert!(result.is_err());

        let store = CheckpointStore::new(&destination, &key).unwrap();
        let checkpoint = store.latest().unwrap().expect("a checkpoint was committed");
        let prefix = token.received();
        let lost = prefix - checkpoint.committed_len;
        // Roughly one second of data plus the commit in flight, at 1 MiB/s.
        assert!(
            lost <= 2 * segment() as u64,
            "a crash lost {lost} bytes, more than about two seconds of transfer"
        );
        assert!(checkpoint.committed_len >= segment() as u64 / 2);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_fault_snapshot_counts_completed_out_of_order_writes_before_shutdown() {
        let dir = temp_dir("fault-lanes");
        let destination = dir.join("file.bin");
        // Nonzero bytes distinguish all actual writes from sparse zero-filled holes.
        let body = vec![1; 128 * segment()];
        let server = range_server(move |req| {
            let mut answer = honest(&body, "\"v1\"", req);
            answer.pace = Duration::from_millis(4);
            answer.chunk = 16 * 1024;
            answer
        });
        let download_request = request(server.url.clone(), destination.clone());
        let key = crate::checkpoint::source_key(&server.url);
        struct FaultSnapshot {
            started: std::time::Instant,
            store: CheckpointStore,
            seen: Mutex<Option<(u64, u64, f64)>>,
        }
        impl FaultInjector for FaultSnapshot {
            fn check(&self, point: FaultPoint) -> io::Result<()> {
                let elapsed = self.started.elapsed();
                if point == FaultPoint::PayloadWrite && elapsed > Duration::from_secs(6) {
                    let mut seen = self.seen.lock().unwrap();
                    if seen.is_none() {
                        // Read metadata before graceful thread draining can commit
                        // more. Count every on-disk byte, including retired lanes.
                        let committed = self.store.latest()?.map_or(0, |r| r.committed_len);
                        let written = fs::read(self.store.staging())?
                            .into_iter()
                            .filter(|byte| *byte == 1)
                            .count() as u64;
                        *seen = Some((written, committed, elapsed.as_secs_f64()));
                    }
                    return Err(io::Error::other("injected write failure"));
                }
                Ok(())
            }
        }
        let fault = FaultSnapshot {
            started: std::time::Instant::now(),
            store: CheckpointStore::new(&destination, &key).unwrap(),
            seen: Mutex::new(None),
        };
        assert!(download_with_faults(download_request, &fault).is_err());
        let (written, committed, seconds) = fault.seen.lock().unwrap().unwrap();
        assert!(committed > 0, "a pre-fault checkpoint was durably recorded");
        let lost = written.saturating_sub(committed);
        let goodput = written as f64 / seconds;
        assert!(
            lost as f64 <= (4.0 * goodput).max(64.0 * 1024.0),
            "lost {lost} bytes at {goodput:.0} B/s"
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn pause_commits_the_prefix_and_resume_continues_from_it() {
        let dir = temp_dir("pause-resume");
        let destination = dir.join("file.bin");
        let body = patterned(8 * segment());
        let served = body.clone();
        let server = range_server(move |req| {
            let mut answer = honest(&served, "\"v1\"", req);
            answer.pace = Duration::from_millis(4);
            answer.chunk = 4096;
            answer
        });
        let mut first = request(server.url.clone(), destination.clone());
        first.cancel_cleanup = CancelCleanup::RetainStaging;
        let token = first.cancellation.clone();
        let worker = thread::spawn(move || download(first));
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        while token.received() < segment() as u64 {
            assert!(std::time::Instant::now() < deadline, "no progress");
            thread::sleep(Duration::from_millis(5));
        }
        let beyond: u64 = {
            let prefix = token.received();
            token
                .segment_monitor()
                .snapshot()
                .iter()
                .filter(|range| range.start >= prefix)
                .map(|range| range.received)
                .sum()
        };
        token.cancel();
        assert!(matches!(
            worker.join().unwrap(),
            Err(DownloadError::Cancelled { staging: Some(_) })
        ));
        // About four seconds of transfer at most sit beyond the prefix.
        assert!(
            beyond <= 6 * segment() as u64,
            "{beyond} bytes beyond the prefix"
        );

        let key = crate::checkpoint::source_key(&server.url);
        let store = CheckpointStore::new(&destination, &key).unwrap();
        let checkpoint = store.latest().unwrap().expect("pause committed the prefix");
        assert!(store.validate_staging(&checkpoint).unwrap());
        assert!(checkpoint.committed_len > 0);
        assert!(!destination.exists());

        let before = server.requests().len();
        let done = download(request(server.url.clone(), destination.clone())).unwrap();
        assert_eq!(done.bytes, body.len() as u64);
        assert_eq!(fs::read(&destination).unwrap(), body);
        let requests = server.requests();
        assert!(requests[before].contains(&format!("Range: bytes={}-", checkpoint.committed_len)));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_cancel_that_removes_staging_leaves_nothing_behind() {
        for _ in 0..3 {
            let dir = temp_dir("cancel-remove");
            let destination = dir.join("file.bin");
            let body = patterned(8 * segment());
            let served = body.clone();
            let server = range_server(move |req| {
                let mut answer = honest(&served, "\"v1\"", req);
                answer.pace = Duration::from_millis(4);
                answer
            });
            let download_request = request(server.url.clone(), destination.clone());
            let token = download_request.cancellation.clone();
            let worker = thread::spawn(move || download(download_request));
            while token.received() < segment() as u64 / 2 {
                thread::sleep(Duration::from_millis(2));
            }
            token.cancel();
            assert!(matches!(
                worker.join().unwrap(),
                Err(DownloadError::Cancelled { staging: None })
            ));
            // The commit thread and the lanes are done: nothing can recreate a
            // checkpoint after the removal.
            assert_eq!(fs::read_dir(&dir).unwrap().count(), 0);
            drop(server);
            fs::remove_dir_all(dir).unwrap();
        }
    }
    #[test]
    fn a_complete_retained_prefix_publishes_without_network_and_checks_its_digest() {
        for wrong_checksum in [false, true] {
            let dir = temp_dir("complete-prefix");
            let destination = dir.join("file.bin");
            let body = patterned(128 * 1024);
            let url = "http://127.0.0.1:1/unreachable";
            seed_with_total(
                url,
                &destination,
                &body,
                body.len(),
                Some(body.len() as u64),
            );
            let expected = if wrong_checksum {
                "0".repeat(64)
            } else {
                sha256_hex(&body)
            };
            let result = download(checked(url.into(), destination.clone(), &expected));
            if wrong_checksum {
                assert!(matches!(
                    result,
                    Err(DownloadError::ChecksumMismatch { .. })
                ));
                assert!(!destination.exists());
            } else {
                assert_eq!(result.unwrap().bytes, body.len() as u64);
                assert_eq!(fs::read(&destination).unwrap(), body);
            }
            fs::remove_dir_all(dir).unwrap();
        }
    }

    #[test]
    fn removal_cancel_wins_over_a_concurrent_commit_flush_error() {
        struct CancelOnFlush(CancellationToken);
        impl FaultInjector for CancelOnFlush {
            fn check(&self, point: FaultPoint) -> io::Result<()> {
                if point == FaultPoint::PayloadFlush {
                    self.0.cancel();
                    return Err(io::Error::other("commit flush failed during cancel"));
                }
                Ok(())
            }
        }
        let dir = temp_dir("cancel-commit-error");
        let destination = dir.join("file.bin");
        let body = patterned(8 * segment());
        let server = range_server(move |req| {
            let mut answer = honest(&body, "\"v1\"", req);
            answer.pace = Duration::from_millis(4);
            answer.chunk = 4096;
            answer
        });
        let download_request = request(server.url.clone(), destination);
        let fault = CancelOnFlush(download_request.cancellation.clone());
        assert!(matches!(
            download_with_faults(download_request, &fault),
            Err(DownloadError::Cancelled { staging: None })
        ));
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 0);
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn a_transport_retry_followed_by_an_identity_change_does_not_make_a_third_attempt() {
        let dir = temp_dir("two-attempts");
        let destination = dir.join("file.bin");
        let body = patterned(128 * 1024);
        let served = body.clone();
        let server = range_server(move |req| {
            if req.index < 4 {
                let mut answer = honest(&served, "\"v1\"", req);
                answer.cut_after = Some(0);
                answer
            } else {
                whole(&served, "\"v2\"")
            }
        });
        seed_with_total(
            &server.url,
            &destination,
            &body,
            32 * 1024,
            Some(body.len() as u64),
        );
        assert!(download(request(server.url.clone(), destination.clone())).is_err());
        assert_eq!(
            server.requests().len(),
            5,
            "transport and identity share the attempt cap"
        );
        assert!(!destination.exists());
        fs::remove_dir_all(dir).unwrap();
    }
}
