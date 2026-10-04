//! The stream-first scheduler against real sockets: first-response rules,
//! range splitting, stalls, retries, throttling and cancellation.

use fetchpath_http::{
    GlobalBudget, RequestContext, ResumePoint, Scheduler, SegmentMonitor, TransferError,
    TransferLimits, TransferReport, transfer_resumable,
};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

/// Real h2c, with either one token bucket per TCP session or one shared
/// bucket for the whole origin. Equal stream pacing cannot reveal which
/// limit is in force; the production scheduler has to measure another socket.
const H2_POOL_FIXTURE: &str = r#"
const http2 = require('node:http2');
const server = http2.createServer();
const sessions = new Set();
const perConnection = process.argv[1] === 'connection';
// Keep the connection-limited transfer alive for baseline and two gain
// windows even when the shared CI runner delays a measurement boundary.
const total = (perConnection ? 16 : 8) * 1024 * 1024;
const chunk = Buffer.alloc(16 * 1024, 7);
server.on('session', session => {
  const bucket = { streams: [], cursor: 0, tokens: 0, last: performance.now() };
  session.bucket = bucket;
  sessions.add(bucket);
  session.on('close', () => sessions.delete(bucket));
  session.on('error', () => {});
});
server.on('stream', (stream, headers) => {
  stream.on('error', () => {});
  const range = /^bytes=(\d+)-(\d*)$/.exec(headers.range || '');
  const start = range ? Number(range[1]) : 0;
  const end = range && range[2] ? Number(range[2]) : total - 1;
  const entry = {stream, left: end - start + 1};
  stream.respond({':status': range ? 206 : 200,
    'content-length': entry.left, 'etag': '"pool-fixture"',
    ...(range ? {'content-range': `bytes ${start}-${end}/${total}`} : {})});
  stream.session.bucket.streams.push(entry);
});
function send(bucket) {
  const now = performance.now();
  bucket.tokens = Math.min(65536, bucket.tokens + (now - bucket.last) * 1048576 / 1000);
  bucket.last = now;
  bucket.streams = bucket.streams.filter(x => !x.stream.destroyed && x.left > 0);
  if (!bucket.streams.length) return;
  while (bucket.tokens >= chunk.length && bucket.streams.length) {
    let sent = false;
    for (let n = 0; n < bucket.streams.length; n++) {
    const entry = bucket.streams[bucket.cursor++ % bucket.streams.length];
    if (entry.stream.writableLength > 65536) continue;
    const count = Math.min(chunk.length, entry.left);
    entry.stream.write(chunk.subarray(0, count));
    entry.left -= count;
    bucket.tokens -= count;
    if (!entry.left) entry.stream.end();
    sent = true;
    break;
    }
    if (!sent) break;
    bucket.streams = bucket.streams.filter(x => !x.stream.destroyed && x.left > 0);
  }
}
const globalBucket = {streams: [], cursor: 0, tokens: 0, last: performance.now()};
setInterval(() => {
  if (perConnection) for (const bucket of sessions) send(bucket);
  else {
    globalBucket.streams = [...sessions].flatMap(x => x.streams);
    send(globalBucket);
  }
}, 8);
server.listen(0, '127.0.0.1', () => console.log(`http://127.0.0.1:${server.address().port}/file`));
"#;

fn h2_pool_transfer(limit: &str) -> (TransferReport, Duration, Image) {
    use std::io::{BufRead, BufReader};
    use std::process::{Command, Stdio};
    struct Fixture(std::process::Child);
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let child = Command::new("node")
        .args(["-e", H2_POOL_FIXTURE, limit])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("Node is required for the controlled HTTP/2 fixture");
    let mut fixture = Fixture(child);
    let mut url = String::new();
    BufReader::new(fixture.0.stdout.take().unwrap())
        .read_line(&mut url)
        .unwrap();
    let budget = GlobalBudget::new(2, 2 * MIB).unwrap();
    let context = RequestContext {
        http2_prior_knowledge: true,
        ..RequestContext::default()
    };
    let image = RefCell::new(Image::default());
    let started = Instant::now();
    let report = transfer_resumable(
        url.trim(),
        &context,
        TransferLimits {
            max_concurrency: 2,
            min_segment_bytes: MIB,
            ..limits()
        },
        &budget,
        &SegmentMonitor::default(),
        None,
        || started.elapsed() > Duration::from_secs(25),
        |chunk| image.borrow_mut().record(chunk.offset, chunk.bytes),
    )
    .unwrap();
    (report, started.elapsed(), image.into_inner())
}

#[test]
fn h2_per_connection_limit_gains_throughput_from_the_bounded_socket_trial() {
    let (report, elapsed, image) = h2_pool_transfer("connection");
    assert_eq!(image.bytes, vec![7; 16 * MIB]);
    assert_eq!(image.overlaps, 0);
    assert_eq!(report.budget.peak_active_requests, 2);
    let rates: Vec<f64> = report
        .observations
        .iter()
        .map(|window| window.bytes as f64 / window.elapsed_ms)
        .collect();
    assert!(
        rates.len() >= 3,
        "baseline plus two measured gains: {rates:?}"
    );
    assert!(
        rates[1] >= rates[0] * 1.15 && rates[2] >= rates[0] * 1.15,
        "two gains: {rates:?}"
    );
    assert!(
        elapsed < Duration::from_secs(14),
        "independent sockets must beat the roughly 16-second single-session cap: {elapsed:?}"
    );
    assert_eq!(
        report.connections_opened, 2,
        "accepted trial reuses its two sockets"
    );
    assert_eq!(
        report.replacements, 0,
        "a pool experiment is not an unhealthy-lane replacement"
    );
}

#[test]
fn h2_shared_origin_limit_rejects_the_socket_trial_and_never_reprobes() {
    let (report, _, image) = h2_pool_transfer("origin");
    assert_eq!(image.bytes, vec![7; 8 * MIB]);
    assert_eq!(image.overlaps, 0);
    assert_eq!(report.budget.peak_active_requests, 2);
    assert_eq!(
        report.connections_opened, 3,
        "one shared socket, one rejected trial, one shared rollback; no re-probe"
    );
    assert_eq!(report.replacements, 0);
}

const MIB: usize = 1024 * 1024;

// -- a test server -----------------------------------------------------------

struct Req {
    /// Index of the request across all connections, from zero.
    index: usize,
    range: Option<(usize, Option<usize>)>,
}

struct Reply {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    pace: Duration,
    chunk: usize,
    /// Send this many body bytes, then close.
    cut_after: Option<usize>,
    /// Send this many body bytes, then wait until the server is released.
    hold_after: Option<usize>,
    /// Read the request and never answer it.
    silent: bool,
    initial_delay: Duration,
}

impl Reply {
    fn new(status: u16, body: Vec<u8>) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body,
            pace: Duration::ZERO,
            chunk: 16 * 1024,
            cut_after: None,
            hold_after: None,
            silent: false,
            initial_delay: Duration::ZERO,
        }
    }

    fn header(mut self, name: &str, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    fn paced(mut self) -> Self {
        // About 30 MB/s per lane.
        self.pace = Duration::from_millis(2);
        self.chunk = 64 * 1024;
        self
    }
}

/// A 206 for `start..=end` of `body`.
fn partial(body: &[u8], etag: Option<&str>, start: usize, end: usize) -> Reply {
    let mut reply = Reply::new(206, body[start..=end].to_vec()).header(
        "Content-Range",
        format!("bytes {start}-{end}/{}", body.len()),
    );
    if let Some(etag) = etag {
        reply = reply.header("ETag", etag);
    }
    reply
}

/// The honest answer to any request for `body`.
fn honest(body: &[u8], etag: &str, req: &Req) -> Reply {
    match req.range {
        Some((start, end)) => {
            let end = end.unwrap_or(body.len() - 1).min(body.len() - 1);
            partial(body, Some(etag), start, end)
        }
        None => whole(body, etag),
    }
}

fn whole(body: &[u8], etag: &str) -> Reply {
    Reply::new(200, body.to_vec()).header("ETag", etag)
}

struct Server {
    url: String,
    requests: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    release: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}

impl Server {
    fn requests(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }

    fn count(&self) -> usize {
        self.requests.lock().unwrap().len()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.release.store(true, Ordering::SeqCst);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn read_request(stream: &mut TcpStream) -> Option<String> {
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
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

fn server(behaviour: impl Fn(&Req) -> Reply + Send + Sync + 'static) -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let url = format!("http://{}/fixture", listener.local_addr().unwrap());
    let requests = Arc::new(Mutex::new(Vec::new()));
    let stop = Arc::new(AtomicBool::new(false));
    let release = Arc::new(AtomicBool::new(false));
    let counter = Arc::new(AtomicUsize::new(0));
    let behaviour = Arc::new(behaviour);
    let (log, flag, released) = (
        Arc::clone(&requests),
        Arc::clone(&stop),
        Arc::clone(&release),
    );
    let worker = thread::spawn(move || {
        let mut handlers = Vec::new();
        while !flag.load(Ordering::SeqCst) {
            let Ok((mut stream, _)) = listener.accept() else {
                thread::sleep(Duration::from_millis(1));
                continue;
            };
            let (behaviour, log, counter, flag, released) = (
                Arc::clone(&behaviour),
                Arc::clone(&log),
                Arc::clone(&counter),
                Arc::clone(&flag),
                Arc::clone(&released),
            );
            handlers.push(thread::spawn(move || {
                stream.set_nonblocking(false).unwrap();
                // One request per connection: every reply says Connection: close.
                {
                    let Some(text) = read_request(&mut stream) else {
                        return;
                    };
                    let req = Req {
                        index: counter.fetch_add(1, Ordering::SeqCst),
                        range: parse_range(&text),
                    };
                    log.lock().unwrap().push(text);
                    let reply = behaviour(&req);
                    if reply.silent {
                        while !flag.load(Ordering::SeqCst) && !released.load(Ordering::SeqCst) {
                            thread::sleep(Duration::from_millis(5));
                        }
                        return;
                    }
                    thread::sleep(reply.initial_delay);
                    let reason = match reply.status {
                        206 => "Partial Content",
                        429 => "Too Many Requests",
                        _ => "Test",
                    };
                    let mut head = format!(
                        "HTTP/1.1 {} {reason}\r\nContent-Length: {}\r\nConnection: close\r\n",
                        reply.status,
                        reply.body.len()
                    );
                    for (name, value) in &reply.headers {
                        head.push_str(&format!("{name}: {value}\r\n"));
                    }
                    head.push_str("\r\n");
                    if stream.write_all(head.as_bytes()).is_err() {
                        return;
                    }
                    let limit = reply.cut_after.unwrap_or(reply.body.len());
                    let mut sent = 0;
                    for chunk in reply.body[..limit.min(reply.body.len())].chunks(reply.chunk) {
                        if flag.load(Ordering::SeqCst) {
                            return;
                        }
                        if reply.hold_after.is_some_and(|hold| sent >= hold) {
                            while !flag.load(Ordering::SeqCst) && !released.load(Ordering::SeqCst) {
                                thread::sleep(Duration::from_millis(5));
                            }
                        }
                        if stream.write_all(chunk).is_err() {
                            return;
                        }
                        sent += chunk.len();
                        if !reply.pace.is_zero() {
                            thread::sleep(reply.pace);
                        }
                    }
                    // `Connection: close` was sent, so this ends the connection.
                }
            }));
        }
        for handler in handlers {
            let _ = handler.join();
        }
    });
    Server {
        url,
        requests,
        stop,
        release,
        worker: Some(worker),
    }
}

// -- the sink: an image of the file that refuses overlapping writes ------------

#[derive(Default)]
struct Image {
    bytes: Vec<u8>,
    extents: BTreeMap<u64, u64>,
    overlaps: usize,
}

impl Image {
    fn record(&mut self, offset: u64, data: &[u8]) -> io::Result<()> {
        let end = offset + data.len() as u64;
        if let Some((_, &before_end)) = self.extents.range(..end).next_back()
            && before_end > offset
        {
            self.overlaps += 1;
            return Err(io::Error::other("a byte was delivered twice"));
        }
        self.extents.insert(offset, end);
        if self.bytes.len() < end as usize {
            self.bytes.resize(end as usize, 0);
        }
        self.bytes[offset as usize..end as usize].copy_from_slice(data);
        Ok(())
    }

    /// Bytes in the image, whatever order they came in.
    fn written(&self) -> u64 {
        self.extents.iter().map(|(start, end)| end - start).sum()
    }
}

fn limits() -> TransferLimits {
    TransferLimits {
        scheduler: Scheduler::Stream,
        ..TransferLimits::default()
    }
}

fn fetch_with(
    server: &Server,
    limits: TransferLimits,
    resume: Option<&ResumePoint>,
    image: &RefCell<Image>,
) -> Result<TransferReport, TransferError> {
    let budget = GlobalBudget::new(8, 32 * MIB).unwrap();
    let monitor = SegmentMonitor::default();
    transfer_resumable(
        &server.url,
        &RequestContext::default(),
        limits,
        &budget,
        &monitor,
        resume,
        || false,
        |chunk| image.borrow_mut().record(chunk.offset, chunk.bytes),
    )
}

fn fetch(server: &Server) -> (Result<TransferReport, TransferError>, Image) {
    let image = RefCell::new(Image::default());
    let result = fetch_with(server, limits(), None, &image);
    (result, image.into_inner())
}

fn patterned(len: usize) -> Vec<u8> {
    (0..len).map(|index| (index % 251) as u8).collect()
}

// -- ranges ------------------------------------------------------------------

#[test]
fn redirects_are_resolved_once_and_ranges_use_the_pinned_target() {
    let body = patterned(12 * MIB);
    let served = body.clone();
    let target = server(move |req| honest(&served, "\"v1\"", req).paced());
    let location = target.url.clone();
    let origin = server(move |_| {
        Reply::new(302, b"redirect body is not file data".to_vec()).header("Location", &location)
    });
    let (result, image) = fetch(&origin);
    assert!(result.unwrap().peak_concurrency >= 2);
    assert_eq!(origin.count(), 1);
    assert!(target.count() >= 2);
    assert_eq!(image.bytes, body);
    assert_eq!(image.overlaps, 0);
}

#[test]
fn rejected_pins_re_resolve_and_check_the_new_identity() {
    for status in [403, 404, 410] {
        for changed in [0, 1, 2] {
            let body = patterned(12 * MIB);
            let mut served = body.clone();
            if changed == 2 {
                served.push(1);
            }
            let renewed = server(move |req| {
                honest(&served, if changed == 1 { "\"v2\"" } else { "\"v1\"" }, req).paced()
            });
            let old_body = body.clone();
            let expired = server(move |req| {
                if req.index == 0 {
                    honest(&old_body, "\"v1\"", req).paced()
                } else {
                    Reply::new(status, Vec::new())
                }
            });
            let (first, next) = (expired.url.clone(), renewed.url.clone());
            let origin = server(move |req| {
                Reply::new(302, Vec::new())
                    .header("Location", if req.index == 0 { &first } else { &next })
            });
            let (result, image) = fetch(&origin);
            if changed != 0 {
                assert!(
                    matches!(result, Err(TransferError::IdentityChanged(_))),
                    "{result:?}"
                );
            } else {
                result.unwrap();
                assert_eq!(image.bytes, body);
                assert_eq!(image.overlaps, 0);
            }
            assert_eq!(
                origin.count(),
                2,
                "one refresh resolver for status {status}"
            );
            assert!(renewed.count() > 0);
        }
    }
}

#[test]
fn expired_signed_url_is_refreshed_before_starting_another_range() {
    let body = patterned(12 * MIB);
    let served = body.clone();
    let renewed = server(move |req| honest(&served, "\"v1\"", req).paced());
    let old_body = body.clone();
    let expired = server(move |req| honest(&old_body, "\"v1\"", req).paced());
    let (first, next) = (format!("{}?Expires=1", expired.url), renewed.url.clone());
    let origin = server(move |req| {
        Reply::new(302, Vec::new()).header("Location", if req.index == 0 { &first } else { &next })
    });
    let (result, image) = fetch(&origin);
    result.unwrap();
    assert_eq!(origin.count(), 2);
    assert_eq!(
        expired.count(),
        1,
        "the expired URL only carried the opening stream"
    );
    assert_eq!(image.bytes, body);
}

#[test]
fn redirect_cache_expiry_is_honoured_and_repeated_rejected_targets_are_bounded() {
    let body = patterned(12 * MIB);
    let served = body.clone();
    let target = server(move |req| honest(&served, "\"v1\"", req).paced());
    let location = target.url.clone();
    let origin = server(move |_| {
        Reply::new(302, Vec::new())
            .header("Location", &location)
            .header("Cache-Control", "private, max-age=0")
    });
    let (result, image) = fetch(&origin);
    result.unwrap();
    assert!(origin.count() >= 2, "the redirect itself expires the pin");
    assert_eq!(image.bytes, body);

    let rejected = server(move |_| Reply::new(403, Vec::new()));
    let location = rejected.url.clone();
    let origin = server(move |_| Reply::new(302, Vec::new()).header("Location", &location));
    assert!(matches!(fetch(&origin).0, Err(TransferError::Rejected(_))));
    assert_eq!(
        origin.count(),
        4,
        "opening refreshes have a finite retry budget"
    );
    assert_eq!(rejected.count(), 4);
}

#[test]
fn cross_origin_redirect_and_pinned_ranges_strip_credentials_cookies_and_referer() {
    let served = patterned(12 * MIB);
    let target = server(move |req| honest(&served, "\"v1\"", req).paced());
    let mut location = url::Url::parse(&target.url).unwrap();
    location.set_username("injected").unwrap();
    location.set_password(Some("injected")).unwrap();
    let origin = server(move |_| {
        Reply::new(302, Vec::new())
            .header("Location", location.as_str())
            .header("Set-Cookie", "response=private; Path=/")
    });
    let mut initial = url::Url::parse(&origin.url).unwrap();
    initial.set_username("fixture").unwrap();
    initial.set_password(Some("private")).unwrap();
    let context = RequestContext {
        cookie_lines: vec!["127.0.0.1\tFALSE\t/\tFALSE\t0\tfixture\tprivate".into()],
        referer: Some("http://example.invalid/private".into()),
        ..RequestContext::default()
    };
    let budget = GlobalBudget::new(8, 32 * MIB).unwrap();
    transfer_resumable(
        initial.as_str(),
        &context,
        limits(),
        &budget,
        &SegmentMonitor::default(),
        None,
        || false,
        |_| Ok(()),
    )
    .unwrap();
    let sent = origin.requests()[0].to_ascii_lowercase();
    assert!(sent.contains("authorization:"));
    assert!(sent.contains("cookie:"));
    assert!(sent.contains("referer:"));
    assert!(target.count() >= 2);
    for sent in target.requests() {
        let sent = sent.to_ascii_lowercase();
        assert!(
            !sent.contains("authorization:"),
            "credentials crossed ports"
        );
        assert!(!sent.contains("cookie:"), "cookies crossed ports");
        assert!(!sent.contains("referer:"));
    }
}

#[test]
fn lanes_split_a_file_with_no_gap_and_no_overlap() {
    let body = patterned(12 * MIB);
    let served = body.clone();
    let server = server(move |req| honest(&served, "\"v1\"", req).paced());

    let (result, image) = fetch(&server);
    let report = result.unwrap();

    assert_eq!(image.bytes, body, "every byte, in the right place");
    assert_eq!(image.overlaps, 0);
    assert_eq!(image.written(), body.len() as u64);
    assert!(report.used_ranges);
    assert_eq!(report.bytes, body.len() as u64);
    assert!(report.peak_concurrency >= 2);
    let requests = server.requests();
    assert!(requests[0].contains("Range: bytes=0-\r\n"), "no probe byte");
    assert!(
        requests[1..]
            .iter()
            .all(|request| request.contains("If-Range: \"v1\"")),
        "later ranges carry the validator"
    );
}

#[test]
fn returning_to_the_original_origin_does_not_restore_secrets() {
    let return_url = Arc::new(Mutex::new(String::new()));
    let shared_url = Arc::clone(&return_url);
    let intermediate = server(move |_| {
        Reply::new(302, Vec::new()).header("Location", shared_url.lock().unwrap().clone())
    });
    let other_url = intermediate.url.clone();
    let body = patterned(12 * MIB);
    let origin = server(move |req| {
        if req.index == 0 {
            Reply::new(302, Vec::new()).header("Location", &other_url)
        } else {
            honest(&body, "\"v1\"", req).paced()
        }
    });
    *return_url.lock().unwrap() = origin.url.clone();
    let mut initial = url::Url::parse(&origin.url).unwrap();
    initial.set_username("fixture").unwrap();
    initial.set_password(Some("private")).unwrap();
    let context = RequestContext {
        cookie_lines: vec!["127.0.0.1\tFALSE\t/\tFALSE\t0\tfixture\tprivate".into()],
        referer: Some("http://example.invalid/private".into()),
        ..RequestContext::default()
    };
    let budget = GlobalBudget::new(8, 32 * MIB).unwrap();
    transfer_resumable(
        initial.as_str(),
        &context,
        limits(),
        &budget,
        &SegmentMonitor::default(),
        None,
        || false,
        |_| Ok(()),
    )
    .unwrap();
    assert!(
        origin.requests()[0]
            .to_ascii_lowercase()
            .contains("authorization:")
    );
    for sent in origin
        .requests()
        .into_iter()
        .skip(1)
        .chain(intermediate.requests())
    {
        let sent = sent.to_ascii_lowercase();
        assert!(!sent.contains("authorization:"));
        assert!(!sent.contains("cookie:"));
        assert!(!sent.contains("referer:"));
    }
}

#[test]
fn same_origin_redirect_keeps_the_cookie_engine_for_the_next_hop() {
    let origin = server(move |req| {
        if req.index == 0 {
            Reply::new(302, Vec::new())
                .header("Location", "/next")
                .header("Set-Cookie", "session=fixture; Path=/")
        } else {
            whole(b"file", "\"v1\"")
        }
    });
    let budget = GlobalBudget::new(8, 32 * MIB).unwrap();
    let context = RequestContext {
        cookie_lines: vec!["127.0.0.1\tFALSE\t/\tFALSE\t0\tinitial\tfixture".into()],
        ..RequestContext::default()
    };
    transfer_resumable(
        &origin.url,
        &context,
        limits(),
        &budget,
        &SegmentMonitor::default(),
        None,
        || false,
        |_| Ok(()),
    )
    .unwrap();
    let request = origin.requests()[1].to_ascii_lowercase();
    assert!(request.contains("session=fixture"));
    assert!(request.contains("initial=fixture"));
}

#[test]
fn a_remainder_under_the_adaptive_size_finishes_on_the_first_request() {
    let body = patterned(3 * MIB);
    let served = body.clone();
    let server = server(move |req| honest(&served, "\"v1\"", req));
    let point = ResumePoint {
        offset: MIB as u64,
        strong_etag: "\"v1\"".into(),
        expected_total: Some(body.len() as u64),
    };
    let image = RefCell::new(Image::default());

    let report = fetch_with(&server, limits(), Some(&point), &image).unwrap();

    assert_eq!(server.count(), 1);
    assert_eq!(report.bytes, body.len() as u64);
    assert_eq!(&image.borrow().bytes[MIB..], &body[MIB..]);
    assert!(server.requests()[0].contains(&format!("Range: bytes={MIB}-\r\n")));
    assert!(server.requests()[0].contains("If-Range: \"v1\""));
}

#[test]
fn a_resume_of_a_large_remainder_uses_ranges() {
    let body = patterned(12 * MIB);
    let served = body.clone();
    let server = server(move |req| honest(&served, "\"v1\"", req).paced());
    let point = ResumePoint {
        offset: 2 * MIB as u64,
        strong_etag: "\"v1\"".into(),
        expected_total: Some(body.len() as u64),
    };
    let image = RefCell::new(Image::default());

    let report = fetch_with(&server, limits(), Some(&point), &image).unwrap();

    assert!(report.used_ranges);
    assert!(server.count() > 1);
    assert_eq!(&image.borrow().bytes[2 * MIB..], &body[2 * MIB..]);
    assert_eq!(image.borrow().overlaps, 0);
}

// -- the first response (spec 4.1) ---------------------------------------------

#[test]
fn a_200_at_zero_is_streamed_in_one_request() {
    let body = patterned(2 * MIB);
    let served = body.clone();
    let server = server(move |_| whole(&served, "\"v1\""));

    let (result, image) = fetch(&server);
    let report = result.unwrap();

    assert_eq!(image.bytes, body);
    assert!(!report.used_ranges);
    assert_eq!(server.count(), 1);
    assert_eq!(report.strong_etag.as_deref(), Some("\"v1\""));
}

#[test]
fn a_capped_206_with_a_strong_etag_gives_the_rest_to_other_lanes() {
    let body = patterned(12 * MIB);
    let served = body.clone();
    let server = server(move |req| {
        if req.index == 0 {
            // Asked for the whole file, the server answers with 5 MiB.
            partial(&served, Some("\"v1\""), 0, 5 * MIB - 1).paced()
        } else {
            honest(&served, "\"v1\"", req).paced()
        }
    });

    let (result, image) = fetch(&server);

    result.unwrap();
    assert_eq!(image.bytes, body);
    assert_eq!(image.overlaps, 0);
    assert!(server.count() > 1);
}

#[test]
fn a_capped_206_without_a_strong_etag_asks_again_without_a_range() {
    let body = patterned(6 * MIB);
    let served = body.clone();
    let server = server(move |req| {
        if req.index == 0 {
            partial(&served, Some("W/\"v1\""), 0, 2 * MIB - 1)
        } else {
            Reply::new(200, served.clone())
        }
    });

    let (result, image) = fetch(&server);

    let report = result.unwrap();
    assert_eq!(image.bytes, body);
    assert!(!report.used_ranges);
    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    assert!(requests[0].contains("Range: bytes=0-"));
    assert!(
        !requests[1].contains("Range:"),
        "the second request is plain"
    );
}

#[test]
fn a_206_with_an_unknown_total_is_read_on_one_request() {
    let body = patterned(MIB);
    let served = body.clone();
    let server = server(move |_| {
        Reply::new(206, served.clone())
            .header("Content-Range", format!("bytes 0-{}/*", served.len() - 1))
            .header("ETag", "\"v1\"")
    });

    let (result, image) = fetch(&server);

    let report = result.unwrap();
    assert_eq!(image.bytes, body);
    assert!(!report.used_ranges);
    assert_eq!(report.total_bytes, None);
    assert_eq!(server.count(), 1);
}

#[test]
fn a_206_at_zero_without_a_strong_etag_is_read_sequentially() {
    let body = patterned(6 * MIB);
    let served = body.clone();
    let server = server(move |_| partial(&served, Some("W/\"v1\""), 0, served.len() - 1));

    let (result, image) = fetch(&server);

    let report = result.unwrap();
    assert_eq!(image.bytes, body);
    assert!(!report.used_ranges);
    assert_eq!(report.strong_etag, None);
    assert_eq!(server.count(), 1);
}

#[test]
fn a_416_at_zero_on_an_empty_resource_is_retried_once_plain() {
    let server = server(|req| {
        if req.index == 0 {
            Reply::new(416, Vec::new())
        } else {
            Reply::new(200, Vec::new())
        }
    });

    let (result, image) = fetch(&server);

    let report = result.unwrap();
    assert_eq!(report.bytes, 0);
    assert!(image.bytes.is_empty());
    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    assert!(!requests[1].contains("Range:"));
}

#[test]
fn a_416_that_persists_after_the_plain_retry_is_an_error() {
    let server = server(|_| Reply::new(416, Vec::new()));
    let (result, _) = fetch(&server);
    assert!(matches!(result, Err(TransferError::Rejected(_))));
    assert_eq!(server.count(), 2, "one retry, no more");
}

fn resume_at(offset: usize, total: Option<usize>) -> ResumePoint {
    ResumePoint {
        offset: offset as u64,
        strong_etag: "\"v1\"".into(),
        expected_total: total.map(|total| total as u64),
    }
}

#[test]
fn a_416_on_a_resume_is_an_identity_restart() {
    let server = server(|_| Reply::new(416, Vec::new()));
    let image = RefCell::new(Image::default());
    let point = resume_at(MIB, Some(2 * MIB));
    let result = fetch_with(&server, limits(), Some(&point), &image);
    assert!(matches!(result, Err(TransferError::IdentityChanged(_))));
    assert_eq!(image.borrow().written(), 0);
}

#[test]
fn a_200_on_a_resume_is_an_identity_restart_before_any_write() {
    let body = patterned(2 * MIB);
    let served = body.clone();
    let server = server(move |_| whole(&served, "\"v1\""));
    let image = RefCell::new(Image::default());
    let point = resume_at(MIB, Some(2 * MIB));
    let result = fetch_with(&server, limits(), Some(&point), &image);
    assert!(matches!(result, Err(TransferError::IdentityChanged(_))));
    assert_eq!(image.borrow().written(), 0, "the stream was not reused");
}

#[test]
fn a_resume_whose_total_or_validator_changed_is_an_identity_restart() {
    let body = patterned(2 * MIB);
    for (etag, recorded_total) in [
        ("\"v1\"", Some(2 * MIB + 1)), // the total differs from the checkpoint
        ("\"v2\"", Some(2 * MIB)),     // the validator differs
    ] {
        let served = body.clone();
        let server = server(move |req| honest(&served, etag, req));
        let image = RefCell::new(Image::default());
        let point = resume_at(MIB, recorded_total);
        let result = fetch_with(&server, limits(), Some(&point), &image);
        assert!(
            matches!(result, Err(TransferError::IdentityChanged(_))),
            "{etag} {recorded_total:?}"
        );
        assert_eq!(image.borrow().written(), 0);
    }
}

#[test]
fn a_later_lane_that_sees_another_representation_stops_the_transfer() {
    let body = patterned(12 * MIB);
    let served = body.clone();
    let server = server(move |req| {
        if req.index == 0 {
            honest(&served, "\"v1\"", req).paced()
        } else {
            honest(&served, "\"v2\"", req).paced()
        }
    });
    let (result, _) = fetch(&server);
    assert!(matches!(result, Err(TransferError::IdentityChanged(_))));
}

#[test]
fn a_later_200_without_a_validator_is_an_identity_change() {
    let body = patterned(12 * MIB);
    let served = body.clone();
    let server = server(move |req| {
        if req.index == 0 {
            honest(&served, "\"v1\"", req).paced()
        } else {
            Reply::new(200, served.clone())
        }
    });
    let (result, _) = fetch(&server);
    assert!(matches!(result, Err(TransferError::IdentityChanged(_))));
}

#[test]
fn a_node_that_ignores_ranges_is_retried_and_the_file_still_completes() {
    let body = patterned(12 * MIB);
    let served = body.clone();
    let server = server(move |req| {
        if req.index == 1 || req.index == 2 {
            // A second and third request answered by a node that ignores ranges.
            whole(&served, "\"v1\"").paced()
        } else {
            honest(&served, "\"v1\"", req).paced()
        }
    });

    let (result, image) = fetch(&server);

    let report = result.unwrap();
    assert_eq!(image.bytes, body);
    assert_eq!(image.overlaps, 0);
    assert!(report.retries >= 2);
}

// -- stalls and retries ---------------------------------------------------------

#[test]
fn a_stalled_lane_is_replaced_and_no_byte_arrives_twice() {
    let body = patterned(12 * MIB);
    let served = body.clone();
    let server = server(move |req| {
        let mut reply = honest(&served, "\"v1\"", req).paced();
        if req.index == 2 {
            // Half its body, then silence for as long as the test runs.
            reply.hold_after = Some(reply.body.len() / 2);
        }
        reply
    });
    let mut short_stall = limits();
    short_stall.stall_timeout = Duration::from_millis(300);
    let image = RefCell::new(Image::default());

    let report = fetch_with(&server, short_stall, None, &image).unwrap();

    assert_eq!(image.borrow().bytes, body);
    assert_eq!(image.borrow().overlaps, 0, "no byte was written twice");
    assert!(report.replacements >= 1, "{report:?}");
}

#[test]
fn a_server_that_never_sends_a_body_ends_the_attempt() {
    let body = patterned(12 * MIB);
    let served = body.clone();
    let server = server(move |req| {
        if req.index == 0 {
            honest(&served, "\"v1\"", req).paced()
        } else {
            Reply {
                silent: true,
                ..Reply::new(206, Vec::new())
            }
        }
    });
    let mut short_stall = limits();
    short_stall.stall_timeout = Duration::from_millis(150);
    let image = RefCell::new(Image::default());
    let started = Instant::now();

    let result = fetch_with(&server, short_stall, None, &image);

    assert!(
        matches!(result, Err(TransferError::Transport(_))),
        "{result:?}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(20),
        "the attempt ended instead of cycling"
    );
}

#[test]
fn a_429_is_retried_after_the_pause_and_counted() {
    let body = patterned(12 * MIB);
    let served = body.clone();
    let server = server(move |req| {
        if req.index == 2 {
            Reply::new(429, Vec::new()).header("Retry-After", "0")
        } else {
            honest(&served, "\"v1\"", req).paced()
        }
    });

    let (result, image) = fetch(&server);

    let report = result.unwrap();
    assert_eq!(image.bytes, body);
    assert!(report.throttles >= 1);
    assert!(report.retries >= 1);
}

#[test]
fn a_503_before_the_first_byte_waits_for_retry_after() {
    let body = patterned(MIB);
    let served = body.clone();
    let server = server(move |req| {
        if req.index == 0 {
            Reply::new(503, Vec::new()).header("Retry-After", "1")
        } else {
            honest(&served, "\"v1\"", req)
        }
    });
    let started = Instant::now();

    let (result, image) = fetch(&server);

    result.unwrap();
    assert_eq!(image.bytes, body);
    assert!(started.elapsed() >= Duration::from_millis(900));
}

#[test]
fn an_error_status_on_the_first_request_is_reported_at_once() {
    for status in [403, 404, 500] {
        let server = server(move |_| Reply::new(status, b"no".to_vec()));
        let (result, _) = fetch(&server);
        match result {
            Err(TransferError::Rejected(detail)) => {
                assert_eq!(detail, format!("HTTP status {status}"));
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(server.count(), 1);
    }
}

#[test]
fn a_connection_that_cannot_be_made_is_reported_without_retrying() {
    // A port nothing listens on.
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let budget = GlobalBudget::new(8, 32 * MIB).unwrap();
    let monitor = SegmentMonitor::default();
    let result = transfer_resumable(
        &format!("http://127.0.0.1:{port}/x"),
        &RequestContext::default(),
        limits(),
        &budget,
        &monitor,
        None,
        || false,
        |_| Ok(()),
    );
    assert!(matches!(result, Err(TransferError::Rejected(_))));
}

// -- cancellation --------------------------------------------------------------

#[test]
fn cancelling_at_any_moment_stops_every_lane_and_never_double_writes() {
    let body = patterned(12 * MIB);
    let served = body.clone();
    let server = server(move |req| honest(&served, "\"v1\"", req).paced());
    for delay_ms in [0_u64, 5, 15, 40, 70, 110, 160, 220] {
        let cancelled = Arc::new(AtomicBool::new(false));
        let trigger = Arc::clone(&cancelled);
        let canceller = thread::spawn(move || {
            thread::sleep(Duration::from_millis(delay_ms));
            trigger.store(true, Ordering::SeqCst);
        });
        let image = RefCell::new(Image::default());
        let budget = GlobalBudget::new(8, 32 * MIB).unwrap();
        let monitor = SegmentMonitor::default();
        let started = Instant::now();
        let result = transfer_resumable(
            &server.url,
            &RequestContext::default(),
            limits(),
            &budget,
            &monitor,
            None,
            || cancelled.load(Ordering::SeqCst),
            |chunk| image.borrow_mut().record(chunk.offset, chunk.bytes),
        );
        canceller.join().unwrap();
        // A very early cancel can also land before the first byte; a late one
        // may find the file already complete.
        match result {
            Err(TransferError::Cancelled) => {}
            Ok(_) => assert_eq!(image.borrow().bytes, body),
            other => panic!("cancel at {delay_ms} ms: {other:?}"),
        }
        assert!(started.elapsed() < Duration::from_secs(5));
        assert_eq!(image.borrow().overlaps, 0);
        assert!(monitor.snapshot().is_empty(), "no lane is left in view");
        // Every lane is gone: the budget has all its permits back.
        assert_eq!(budget.snapshot().active_requests, 0);
    }
}

#[test]
fn measured_first_ttfb_allows_later_ranges_to_wait_beyond_the_stall_timeout() {
    let body = patterned(8 * MIB);
    let served = body.clone();
    let server = server(move |req| {
        let mut reply = honest(&served, "\"v1\"", req).paced();
        reply.initial_delay = Duration::from_millis(300);
        reply
    });
    let mut configured = limits();
    configured.stall_timeout = Duration::from_millis(150);
    let image = RefCell::new(Image::default());
    let report = fetch_with(&server, configured, None, &image).unwrap();
    assert_eq!(image.borrow().bytes, body);
    assert!(server.count() > 1, "later lanes were exercised");
    assert_eq!(report.replacements, 0, "TTFB is not a body stall");
}

#[test]
fn silence_exactly_at_the_first_claim_boundary_releases_the_owner() {
    let body = patterned(16 * MIB);
    let served = body.clone();
    let server = server(move |req| {
        let mut reply = honest(&served, "\"v1\"", req).paced();
        if req.index == 0 {
            // Initial claim is min(total/16, 2*min), here exactly 1 MiB.
            reply.hold_after = Some(MIB);
        }
        reply
    });
    let image = RefCell::new(Image::default());
    let started = Instant::now();
    let mut configured = limits();
    configured.stall_timeout = Duration::from_secs(30);
    let report = fetch_with(&server, configured, None, &image).unwrap();
    assert_eq!(image.borrow().bytes, body);
    assert!(started.elapsed() < Duration::from_secs(10));
    assert_eq!(
        report.replacements, 0,
        "complete owners need no stall timeout"
    );
}

#[test]
fn a_retry_after_over_sixty_seconds_is_not_silently_capped() {
    let server = server(|_| Reply::new(503, Vec::new()).header("Retry-After", "120"));
    let started = Instant::now();
    let budget = GlobalBudget::new(2, 4 * MIB).unwrap();
    let result = transfer_resumable(
        &server.url,
        &RequestContext::default(),
        limits(),
        &budget,
        &SegmentMonitor::default(),
        None,
        || {
            (server.count() > 0 && started.elapsed() >= Duration::from_millis(250))
                || started.elapsed() >= Duration::from_secs(5)
        },
        |_| Ok(()),
    );
    assert!(matches!(result, Err(TransferError::Cancelled)));
    assert_eq!(server.count(), 1);
}

#[test]
fn an_extreme_retry_after_is_an_error_without_a_clock_panic() {
    let server =
        server(|_| Reply::new(503, Vec::new()).header("Retry-After", u64::MAX.to_string()));
    let (result, _) = fetch(&server);
    assert!(
        matches!(result, Err(TransferError::Transport(detail)) if detail.contains("clock range"))
    );
    assert_eq!(server.count(), 1);
}
