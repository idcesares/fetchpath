use crate::checkpoint::{
    ResponseHeaders, resume_headers_match, source_key_with_context, strong_etag,
};
use crate::{BUFFER_BYTES, CancelCleanup, DownloadError, DownloadRequest, DownloadedFile};
use curl::easy::{Easy, List};
use fetchpath_http::{
    Chunk, GlobalBudget, RequestContext as HttpRequestContext, TransferError, TransferLimits,
    transfer_adaptive,
};
use fetchpath_storage::{
    CheckpointPhase, CheckpointRecord, CheckpointStore, FaultInjector, NoFaults,
    PublicationRecovery, sha256_file,
};
use std::cell::{Cell, RefCell};
use std::fs::File;
use std::io;
use std::sync::OnceLock;

const CHECKPOINT_BYTES: u64 = 64 * 1024;
static HTTP_BUDGET: OnceLock<GlobalBudget> = OnceLock::new();

fn http_budget() -> &'static GlobalBudget {
    let limits = TransferLimits::default();
    HTTP_BUDGET.get_or_init(|| {
        GlobalBudget::new(limits.max_active_requests, limits.max_buffered_bytes)
            .expect("default HTTP transfer budget must be valid")
    })
}

enum CallbackFailure {
    RestartRequired,
    Cancelled,
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
    request.cancellation.set_received(offset);
    if request.cancellation.is_cancelled() {
        return Err(cancelled(&request, &store));
    }

    let first = if offset == 0 {
        match perform_adaptive_attempt(&request, &store, &mut file, faults) {
            Err(CallbackFailure::RestartRequired) => {
                file = store
                    .reset()
                    .map_err(|error| storage_error(&store, error))?;
                request.cancellation.set_received(0);
                perform_attempt(&request, &store, &mut file, 0, None, faults)
            }
            result => result,
        }
    } else {
        perform_attempt(
            &request,
            &store,
            &mut file,
            offset,
            validator.as_deref(),
            faults,
        )
    };
    let attempt = match first {
        Err(CallbackFailure::RestartRequired) => {
            file = store
                .reset()
                .map_err(|error| storage_error(&store, error))?;
            offset = 0;
            validator = None;
            request.cancellation.set_received(0);
            match perform_attempt(
                &request,
                &store,
                &mut file,
                offset,
                validator.as_deref(),
                faults,
            ) {
                Ok(attempt) => attempt,
                Err(failure) => return Err(map_attempt_failure(&request, &store, failure)),
            }
        }
        Ok(attempt) => attempt,
        Err(failure) => return Err(map_attempt_failure(&request, &store, failure)),
    };

    store
        .sync_payload(&mut file, faults)
        .map_err(|error| storage_detail(&store, error))?;
    drop(file);
    let digest = sha256_file(store.staging()).map_err(|error| storage_detail(&store, error))?;
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

fn perform_adaptive_attempt(
    request: &DownloadRequest,
    store: &CheckpointStore,
    file: &mut File,
    faults: &dyn FaultInjector,
) -> Result<AttemptResult, CallbackFailure> {
    let limits = TransferLimits::default();
    let budget = http_budget();
    let context = HttpRequestContext {
        cookie_lines: request.context.cookie_lines.clone(),
        referer: request.context.referer.clone(),
        http2_prior_knowledge: false,
    };
    let current_offset = Cell::new(0_u64);
    let last_checkpoint = Cell::new(0_u64);
    let result = transfer_adaptive(
        &request.url,
        &context,
        limits,
        budget,
        || request.cancellation.is_cancelled(),
        |chunk: Chunk<'_>| {
            if chunk.offset != current_offset.get() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "adaptive range arrived at {} while {} was required",
                        chunk.offset,
                        current_offset.get()
                    ),
                ));
            }
            store.write_payload(file, chunk.offset, chunk.bytes, faults)?;
            let next = chunk.offset + chunk.bytes.len() as u64;
            current_offset.set(next);
            request.cancellation.set_received(next);
            if chunk.strong_etag.is_some() && next - last_checkpoint.get() >= CHECKPOINT_BYTES {
                store.sync_payload(file, faults)?;
                let digest = sha256_file(store.staging())?;
                let record = CheckpointRecord::downloading(
                    source_key_with_context(&request.url, &request.context.fingerprint()),
                    next,
                    digest,
                    chunk.strong_etag.map(str::to_owned),
                    chunk.total_bytes,
                );
                store.commit(record, faults)?;
                last_checkpoint.set(next);
            }
            Ok(())
        },
    );
    match result {
        Ok(report) => Ok(AttemptResult {
            headers: ResponseHeaders {
                status: Some(200),
                etag: report.strong_etag,
                content_length: report.total_bytes,
                content_range: None,
            },
            final_len: report.bytes,
        }),
        Err(TransferError::Cancelled) => Err(CallbackFailure::Cancelled),
        Err(TransferError::Sink(error)) => Err(CallbackFailure::Storage(error)),
        Err(TransferError::RestartSequential(_)) => Err(CallbackFailure::RestartRequired),
        Err(error) => Err(CallbackFailure::Transport(error.to_string())),
    }
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

fn perform_attempt(
    request: &DownloadRequest,
    store: &CheckpointStore,
    file: &mut File,
    start_offset: u64,
    expected_etag: Option<&str>,
    faults: &dyn FaultInjector,
) -> Result<AttemptResult, CallbackFailure> {
    let _request_permit = http_budget()
        .reserve(0, &|| request.cancellation.is_cancelled())
        .map_err(|error| match error {
            TransferError::Cancelled => CallbackFailure::Cancelled,
            other => CallbackFailure::Transport(other.to_string()),
        })?;
    let mut easy = Easy::new();
    configure(&mut easy, &request.url, &request.context)
        .map_err(|error| CallbackFailure::Transport(error.to_string()))?;
    if start_offset > 0 {
        easy.range(&format!("{start_offset}-"))
            .map_err(|error| CallbackFailure::Transport(error.to_string()))?;
        let mut headers = List::new();
        headers
            .append(&format!(
                "If-Range: {}",
                expected_etag.expect("resume offset requires a validator")
            ))
            .map_err(|error| CallbackFailure::Transport(error.to_string()))?;
        easy.http_headers(headers)
            .map_err(|error| CallbackFailure::Transport(error.to_string()))?;
    }

    let headers = RefCell::new(ResponseHeaders::default());
    let callback_failure = RefCell::new(None);
    let current_offset = Cell::new(start_offset);
    let received_this_attempt = Cell::new(0_u64);
    let last_checkpoint = Cell::new(start_offset);
    let validated = Cell::new(false);
    let transfer_result = {
        let mut transfer = easy.transfer();
        transfer
            .header_function(|line| {
                headers.borrow_mut().ingest(line);
                true
            })
            .map_err(|error| CallbackFailure::Transport(error.to_string()))?;
        transfer
            .write_function(|data| {
                if !validated.get() {
                    let current = headers.borrow();
                    let valid = if start_offset == 0 {
                        current.status == Some(200)
                    } else {
                        resume_headers_match(
                            &current,
                            start_offset,
                            expected_etag.expect("resume requires validator"),
                        )
                    };
                    if !valid {
                        *callback_failure.borrow_mut() = Some(if start_offset > 0 {
                            CallbackFailure::RestartRequired
                        } else {
                            CallbackFailure::Transport(format!(
                                "HTTP status {}",
                                current.status.unwrap_or_default()
                            ))
                        });
                        return Ok(0);
                    }
                    validated.set(true);
                }
                let offset = current_offset.get();
                if let Err(error) = store.write_payload(file, offset, data, faults) {
                    *callback_failure.borrow_mut() = Some(CallbackFailure::Storage(error));
                    return Ok(0);
                }
                let next = offset + data.len() as u64;
                current_offset.set(next);
                received_this_attempt.set(received_this_attempt.get() + data.len() as u64);
                request.cancellation.set_received(next);

                let validator = strong_etag(headers.borrow().etag.as_deref());
                if validator.is_some() && next - last_checkpoint.get() >= CHECKPOINT_BYTES {
                    let checkpoint_result = (|| -> io::Result<()> {
                        store.sync_payload(file, faults)?;
                        let digest = sha256_file(store.staging())?;
                        let record = CheckpointRecord::downloading(
                            source_key_with_context(&request.url, &request.context.fingerprint()),
                            next,
                            digest,
                            validator,
                            response_total(&headers.borrow(), next),
                        );
                        store.commit(record, faults)?;
                        Ok(())
                    })();
                    if let Err(error) = checkpoint_result {
                        *callback_failure.borrow_mut() = Some(CallbackFailure::Storage(error));
                        return Ok(0);
                    }
                    last_checkpoint.set(next);
                }
                Ok(data.len())
            })
            .map_err(|error| CallbackFailure::Transport(error.to_string()))?;
        transfer
            .progress_function(|_, _, _, _| !request.cancellation.is_cancelled())
            .map_err(|error| CallbackFailure::Transport(error.to_string()))?;
        transfer.perform()
    };

    if let Some(failure) = callback_failure.into_inner() {
        return Err(failure);
    }
    if request.cancellation.is_cancelled() {
        return Err(CallbackFailure::Cancelled);
    }
    if let Err(error) = transfer_result {
        if let Ok(status) = easy.response_code()
            && status >= 400
        {
            return Err(CallbackFailure::Transport(format!("HTTP status {status}")));
        }
        return Err(CallbackFailure::Transport(error.to_string()));
    }
    let headers = headers.into_inner();
    let valid = if start_offset == 0 {
        headers.status == Some(200)
    } else {
        resume_headers_match(
            &headers,
            start_offset,
            expected_etag.expect("resume requires validator"),
        )
    };
    if !valid {
        return Err(if start_offset > 0 {
            CallbackFailure::RestartRequired
        } else {
            CallbackFailure::Transport(format!(
                "HTTP status {}",
                headers.status.unwrap_or_default()
            ))
        });
    }
    let received = received_this_attempt.get();
    if let Some(expected) = response_body_len(&headers)
        && received != expected
    {
        return Err(CallbackFailure::Storage(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            format!("response declared {expected} bytes but delivered {received}"),
        )));
    }
    Ok(AttemptResult {
        headers,
        final_len: current_offset.get(),
    })
}

fn configure(
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

fn response_body_len(headers: &ResponseHeaders) -> Option<u64> {
    if headers.status == Some(206) {
        headers
            .content_range
            .map(|range| range.end - range.start + 1)
    } else {
        headers.content_length
    }
}

fn response_total(headers: &ResponseHeaders, fallback: u64) -> Option<u64> {
    if headers.status == Some(206) {
        headers.content_range.and_then(|range| range.total)
    } else {
        headers.content_length.or(Some(fallback))
    }
}

fn publish(
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

fn cancelled(request: &DownloadRequest, store: &CheckpointStore) -> DownloadError {
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

fn storage_detail(store: &CheckpointStore, error: io::Error) -> DownloadError {
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
        CallbackFailure::Transport(detail) => {
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

    fn exact_range(request: &str) -> Option<(usize, usize)> {
        request.lines().find_map(|line| {
            let (name, value) = line.split_once(':')?;
            if !name.eq_ignore_ascii_case("range") {
                return None;
            }
            let (start, end) = value.trim().strip_prefix("bytes=")?.split_once('-')?;
            Some((start.parse().ok()?, end.parse().ok()?))
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

    fn adaptive_server(body: Vec<u8>) -> (String, Arc<Mutex<Vec<String>>>, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = Arc::clone(&requests);
        let request_count = 1 + (body.len() - 1).div_ceil(1024 * 1024);
        let worker = thread::spawn(move || {
            for _ in 0..request_count {
                let (mut stream, _) = listener.accept().unwrap();
                let request = read_request(&mut stream);
                let (start, end) = exact_range(&request).expect("exact range request expected");
                captured.lock().unwrap().push(request);
                let selected = &body[start..=end];
                let headers = format!(
                    "HTTP/1.1 206 Partial Content\r\nETag: \"adaptive-v1\"\r\nContent-Length: {}\r\nContent-Range: bytes {start}-{end}/{}\r\nConnection: close\r\n\r\n",
                    selected.len(),
                    body.len()
                );
                stream.write_all(headers.as_bytes()).unwrap();
                stream.write_all(selected).unwrap();
            }
        });
        (format!("http://{address}/fixture"), requests, worker)
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

    #[test]
    fn adaptive_ranges_reassemble_in_order_under_the_core_publication_contract() {
        let dir = temp_dir("adaptive-ranges");
        let destination = dir.join("file.bin");
        let body: Vec<u8> = (0..5 * 1024 * 1024)
            .map(|index| (index % 251) as u8)
            .collect();
        let (url, requests, worker) = adaptive_server(body.clone());

        let done = download(request(url, destination.clone())).unwrap();
        worker.join().unwrap();

        assert_eq!(done.bytes, body.len() as u64);
        assert_eq!(fs::read(&destination).unwrap(), body);
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 6);
        assert!(requests[0].contains("Range: bytes=0-0"));
        assert!(
            requests[1..]
                .iter()
                .all(|request| request.contains("If-Range: \"adaptive-v1\""))
        );
        fs::remove_dir_all(dir).unwrap();
    }
}
