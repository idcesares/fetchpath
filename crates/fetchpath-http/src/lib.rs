use curl::easy::{Easy, HttpVersion, List};
use std::collections::HashMap;
use std::fmt;
use std::io;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

mod compatibility;
mod controller;
mod frontier;
mod link;
mod scheduler;

pub use link::{LinkFacts, disposition_file_name, fetch_small, inspect_link};

pub use compatibility::{
    Authentication, CompatibilityCapabilities, CompatibilityContext, CompatibilityProtocol,
    CompatibilityTransferReport, CredentialSecret, transfer_compatibility,
};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Protocol {
    Http1,
    Http2,
    Http3,
    #[default]
    Unknown,
}

impl Protocol {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Http1 => "http/1.1",
            Self::Http2 => "h2",
            Self::Http3 => "h3",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProtocolCapabilities {
    pub http2: bool,
    pub http3: bool,
}

impl ProtocolCapabilities {
    pub fn detect() -> Self {
        let version = curl::Version::get();
        Self {
            http2: version.feature_http2(),
            http3: version.feature_http3(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProtocolDecision {
    pub preferred: Protocol,
    pub fallback_reasons: Vec<&'static str>,
}

pub fn decide_protocol(capabilities: ProtocolCapabilities) -> ProtocolDecision {
    if capabilities.http3 {
        ProtocolDecision {
            preferred: Protocol::Http3,
            fallback_reasons: Vec::new(),
        }
    } else if capabilities.http2 {
        ProtocolDecision {
            preferred: Protocol::Http2,
            fallback_reasons: vec!["http3_unavailable"],
        }
    } else {
        ProtocolDecision {
            preferred: Protocol::Http1,
            fallback_reasons: vec!["http3_unavailable", "http2_unavailable"],
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct RequestContext {
    pub cookie_lines: Vec<String>,
    pub referer: Option<String>,
    /// Reserved for controlled cleartext HTTP/2 origins. Public HTTP URLs use
    /// normal negotiation and HTTPS uses ALPN.
    pub http2_prior_knowledge: bool,
}

/// Which scheduler drives a fresh download.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Scheduler {
    Stream,
    /// One plain GET without `Range` on one connection. A download that has
    /// just restarted after an identity change uses it.
    Plain,
}

#[derive(Clone, Copy, Debug)]
pub struct TransferLimits {
    pub max_active_requests: usize,
    /// Memory the download may hold for receive buffers: one buffer per lane.
    pub max_buffered_bytes: usize,
    pub max_concurrency: usize,
    /// The smallest claim, except near the end of the file.
    pub min_segment_bytes: usize,
    /// Files smaller than this run on one lane.
    pub min_adaptive_bytes: u64,
    /// libcurl receive buffer per lane.
    pub receive_buffer_bytes: usize,
    /// A lane that delivers no bytes for this long is replaced.
    pub stall_timeout: Duration,
    /// How long the first request may take to deliver a byte.
    pub open_timeout: Duration,
    pub scheduler: Scheduler,
}

/// How much time one range should take on its lane.
pub const SEGMENT_SECONDS: f64 = 1.5;

/// The default libcurl receive buffer. FP-085 measured 64, 128 and 256 KiB.
pub const RECEIVE_BUFFER_BYTES: usize = 16 * 1024;

impl Default for TransferLimits {
    fn default() -> Self {
        Self {
            max_active_requests: 8,
            max_buffered_bytes: 32 * 1024 * 1024,
            max_concurrency: 4,
            min_segment_bytes: 1024 * 1024,
            min_adaptive_bytes: 4 * 1024 * 1024,
            receive_buffer_bytes: RECEIVE_BUFFER_BYTES,
            stall_timeout: Duration::from_secs(3),
            open_timeout: Duration::from_secs(30),
            scheduler: Scheduler::Stream,
        }
    }
}

impl TransferLimits {
    fn validate(self) -> Result<Self, TransferError> {
        if self.max_active_requests == 0
            || self.max_buffered_bytes == 0
            || self.max_concurrency == 0
            || self.min_segment_bytes == 0
            || self.receive_buffer_bytes == 0
            || self.receive_buffer_bytes > self.max_buffered_bytes
            || self.stall_timeout.is_zero()
            || self.open_timeout.is_zero()
        {
            return Err(TransferError::InvalidLimits);
        }
        Ok(self)
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct BudgetSnapshot {
    pub active_requests: usize,
    pub buffered_bytes: usize,
    pub peak_active_requests: usize,
    pub peak_buffered_bytes: usize,
}

#[derive(Default)]
struct BudgetState {
    active_requests: usize,
    buffered_bytes: usize,
    peak_active_requests: usize,
    peak_buffered_bytes: usize,
    downloads: HashMap<u64, DownloadState>,
    next_download: u64,
}

struct DownloadState {
    lanes: usize,
    growing: bool,
}

pub struct GlobalBudget {
    max_active_requests: usize,
    max_buffered_bytes: usize,
    state: Mutex<BudgetState>,
    changed: Condvar,
}

impl GlobalBudget {
    pub fn new(
        max_active_requests: usize,
        max_buffered_bytes: usize,
    ) -> Result<Self, TransferError> {
        if max_active_requests == 0 || max_buffered_bytes == 0 {
            return Err(TransferError::InvalidLimits);
        }
        Ok(Self {
            max_active_requests,
            max_buffered_bytes,
            state: Mutex::new(BudgetState::default()),
            changed: Condvar::new(),
        })
    }

    pub fn reserve<C>(
        &self,
        buffered_bytes: usize,
        cancelled: &C,
    ) -> Result<BudgetPermit<'_>, TransferError>
    where
        C: Fn() -> bool + Sync,
    {
        self.reserve_for(None, buffered_bytes, cancelled)
    }

    fn reserve_for(
        &self,
        download: Option<u64>,
        buffered_bytes: usize,
        cancelled: &(dyn Fn() -> bool + Sync),
    ) -> Result<BudgetPermit<'_>, TransferError> {
        if buffered_bytes > self.max_buffered_bytes {
            return Err(TransferError::InvalidLimits);
        }
        let mut state = self.state.lock().unwrap();
        while state.active_requests == self.max_active_requests
            || state.buffered_bytes + buffered_bytes > self.max_buffered_bytes
        {
            if cancelled() {
                return Err(TransferError::Cancelled);
            }
            state = self
                .changed
                .wait_timeout(state, Duration::from_millis(25))
                .unwrap()
                .0;
        }
        Ok(self.grant(&mut state, download, buffered_bytes))
    }

    fn grant(
        &self,
        state: &mut BudgetState,
        download: Option<u64>,
        buffered_bytes: usize,
    ) -> BudgetPermit<'_> {
        state.active_requests += 1;
        state.buffered_bytes += buffered_bytes;
        state.peak_active_requests = state.peak_active_requests.max(state.active_requests);
        state.peak_buffered_bytes = state.peak_buffered_bytes.max(state.buffered_bytes);
        if let Some(slot) = download.and_then(|id| state.downloads.get_mut(&id)) {
            slot.lanes += 1;
        }
        BudgetPermit {
            budget: self,
            buffered_bytes,
            download,
        }
    }

    /// Registers one download so lanes are shared fairly between downloads.
    /// Each registered download can always get its first lane; extra lanes
    /// are split evenly among the downloads still growing.
    pub fn register_download(&self) -> DownloadSlot<'_> {
        let mut state = self.state.lock().unwrap();
        let id = state.next_download;
        state.next_download += 1;
        state.downloads.insert(
            id,
            DownloadState {
                lanes: 0,
                growing: true,
            },
        );
        DownloadSlot { budget: self, id }
    }

    pub fn snapshot(&self) -> BudgetSnapshot {
        let state = self.state.lock().unwrap();
        BudgetSnapshot {
            active_requests: state.active_requests,
            buffered_bytes: state.buffered_bytes,
            peak_active_requests: state.peak_active_requests,
            peak_buffered_bytes: state.peak_buffered_bytes,
        }
    }
}

/// One registered download's view of the [`GlobalBudget`].
pub struct DownloadSlot<'a> {
    budget: &'a GlobalBudget,
    id: u64,
}

impl<'a> DownloadSlot<'a> {
    /// Waits for a lane. A download uses this for its first lane, so a
    /// download beyond the request limit waits in the queue.
    pub fn reserve(
        &self,
        buffered_bytes: usize,
        cancelled: &(dyn Fn() -> bool + Sync),
    ) -> Result<BudgetPermit<'a>, TransferError> {
        self.budget
            .reserve_for(Some(self.id), buffered_bytes, cancelled)
    }

    /// An extra lane, or `None` at once when the budget or this download's
    /// fair share is used up. It never waits, so an event loop can call it.
    pub fn try_reserve(&self, buffered_bytes: usize) -> Option<BudgetPermit<'a>> {
        let budget = self.budget;
        let mut state = budget.state.lock().unwrap();
        if state.active_requests >= budget.max_active_requests
            || state.buffered_bytes + buffered_bytes > budget.max_buffered_bytes
        {
            return None;
        }
        let mine = state.downloads.get(&self.id)?;
        if !mine.growing {
            return None;
        }
        let my_lanes = mine.lanes;
        // A download that has no lane yet must still find room for its first.
        let waiting = state
            .downloads
            .values()
            .filter(|download| download.lanes == 0)
            .count();
        if state.active_requests + waiting >= budget.max_active_requests {
            return None;
        }
        let settled: usize = state
            .downloads
            .iter()
            .filter(|(id, download)| **id != self.id && !download.growing)
            .map(|(_, download)| download.lanes)
            .sum();
        let growing = state
            .downloads
            .values()
            .filter(|download| download.growing)
            .count()
            .max(1);
        let share = budget
            .max_active_requests
            .saturating_sub(settled)
            .div_ceil(growing)
            .max(1);
        if my_lanes >= share {
            return None;
        }
        Some(budget.grant(&mut state, Some(self.id), buffered_bytes))
    }

    /// Says whether this download still wants more lanes. A download that
    /// has stopped probing keeps what it has and leaves the rest to others.
    pub fn set_growing(&self, growing: bool) {
        if let Some(download) = self
            .budget
            .state
            .lock()
            .unwrap()
            .downloads
            .get_mut(&self.id)
        {
            download.growing = growing;
        }
    }
}

impl Drop for DownloadSlot<'_> {
    fn drop(&mut self) {
        self.budget.state.lock().unwrap().downloads.remove(&self.id);
        self.budget.changed.notify_all();
    }
}

pub struct BudgetPermit<'a> {
    budget: &'a GlobalBudget,
    buffered_bytes: usize,
    download: Option<u64>,
}

impl Drop for BudgetPermit<'_> {
    fn drop(&mut self) {
        let mut state = self.budget.state.lock().unwrap();
        state.active_requests -= 1;
        state.buffered_bytes -= self.buffered_bytes;
        if let Some(slot) = self.download.and_then(|id| state.downloads.get_mut(&id)) {
            slot.lanes = slot.lanes.saturating_sub(1);
        }
        self.budget.changed.notify_all();
    }
}

#[derive(Clone, Debug, Default)]
struct ResponseHeaders {
    status: Option<u32>,
    protocol: Protocol,
    etag: Option<String>,
    content_length: Option<u64>,
    /// A total of `*` is allowed and stays unknown.
    open_range: Option<OpenRange>,
    retry_after: Option<Duration>,
}

/// A `Content-Range` whose total may be unknown (`bytes 0-9/*`).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct OpenRange {
    pub start: u64,
    pub end: u64,
    pub total: Option<u64>,
}

impl ResponseHeaders {
    fn ingest(&mut self, line: &[u8]) {
        let Ok(line) = std::str::from_utf8(line) else {
            return;
        };
        let line = line.trim();
        if line.starts_with("HTTP/") {
            self.status = line
                .split_whitespace()
                .nth(1)
                .and_then(|value| value.parse().ok());
            self.protocol = if line.starts_with("HTTP/3") {
                Protocol::Http3
            } else if line.starts_with("HTTP/2") {
                Protocol::Http2
            } else {
                Protocol::Http1
            };
            self.etag = None;
            self.content_length = None;
            self.open_range = None;
            self.retry_after = None;
            return;
        }
        let Some((name, value)) = line.split_once(':') else {
            return;
        };
        let value = value.trim();
        if name.eq_ignore_ascii_case("etag") {
            self.etag = Some(value.to_owned());
        } else if name.eq_ignore_ascii_case("content-length") {
            self.content_length = value.parse().ok();
        } else if name.eq_ignore_ascii_case("content-range") {
            self.open_range = parse_open_range(value);
        } else if name.eq_ignore_ascii_case("retry-after") {
            // A date form is ignored: the lanes back off on the status alone.
            self.retry_after = value.parse().ok().map(Duration::from_secs);
        }
    }
}

fn parse_open_range(value: &str) -> Option<OpenRange> {
    let value = value.strip_prefix("bytes ")?;
    let (range, total) = value.split_once('/')?;
    let (start, end) = range.split_once('-')?;
    let start: u64 = start.parse().ok()?;
    let end: u64 = end.parse().ok()?;
    let total = if total == "*" {
        None
    } else {
        Some(total.parse::<u64>().ok()?)
    };
    (start <= end && total.is_none_or(|total| end < total)).then_some(OpenRange {
        start,
        end,
        total,
    })
}

fn strong_etag(value: Option<&str>) -> Option<String> {
    value
        .filter(|value| value.starts_with('"') && value.ends_with('"') && !value.starts_with("W/"))
        .map(str::to_owned)
}

#[derive(Clone, Debug)]
pub struct Chunk<'a> {
    pub offset: u64,
    pub bytes: &'a [u8],
    pub strong_etag: Option<&'a str>,
    pub total_bytes: Option<u64>,
}

/// One range a segmented transfer has in flight, as last observed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SegmentProgress {
    /// First byte of the range.
    pub start: u64,
    /// Last byte of the range, inclusive. It shrinks when another lane takes
    /// the tail of the range.
    pub end: u64,
    /// Bytes of this range received and written so far.
    pub received: u64,
}

pub(crate) struct LiveSegment {
    pub(crate) start: u64,
    /// One past the last byte the lane may still write.
    pub(crate) end: AtomicU64,
    pub(crate) received: AtomicU64,
}

impl LiveSegment {
    pub(crate) fn new(start: u64, end: u64) -> Arc<Self> {
        Arc::new(Self {
            start,
            end: AtomicU64::new(end),
            received: AtomicU64::new(0),
        })
    }
}

/// A read-only view of the ranges a segmented transfer has in flight.
///
/// Observation only: it never changes what is requested or written. The
/// transfer takes the lock when a range starts and when it ends; each write
/// adds to one atomic counter.
#[derive(Default)]
pub struct SegmentMonitor {
    live: Mutex<Vec<Arc<LiveSegment>>>,
}

impl SegmentMonitor {
    /// The ranges in flight now, in file order. Empty between transfers and
    /// for a transfer that is not segmented.
    pub fn snapshot(&self) -> Vec<SegmentProgress> {
        let mut ranges: Vec<_> = self
            .live
            .lock()
            .expect("segment monitor poisoned")
            .iter()
            .map(|segment| SegmentProgress {
                start: segment.start,
                end: segment.end.load(Ordering::Relaxed).saturating_sub(1),
                received: segment.received.load(Ordering::Relaxed),
            })
            .collect();
        ranges.sort_by_key(|range| range.start);
        ranges
    }

    pub(crate) fn add(&self, start: u64, end: u64) -> Arc<LiveSegment> {
        let segment = LiveSegment::new(start, end);
        self.live
            .lock()
            .expect("segment monitor poisoned")
            .push(Arc::clone(&segment));
        segment
    }

    pub(crate) fn remove(&self, segment: &Arc<LiveSegment>) {
        self.live
            .lock()
            .expect("segment monitor poisoned")
            .retain(|live| !Arc::ptr_eq(live, segment));
    }

    pub(crate) fn clear(&self) {
        self.live.lock().expect("segment monitor poisoned").clear();
    }
}

/// Empties the monitor however the transfer ends, so a failed or cancelled
/// download never shows ranges that are no longer moving.
struct ClearOnDrop<'a>(&'a SegmentMonitor);

impl Drop for ClearOnDrop<'_> {
    fn drop(&mut self) {
        self.0.clear();
    }
}

/// One measurement window of the concurrency controller.
#[derive(Clone, Debug)]
pub struct BatchObservation {
    pub concurrency: usize,
    pub bytes: u64,
    pub elapsed_ms: f64,
}

#[derive(Clone, Debug, Default)]
pub struct TransferReport {
    /// The length of the file after the transfer, resumed prefix included.
    pub bytes: u64,
    pub total_bytes: Option<u64>,
    pub strong_etag: Option<String>,
    pub preferred_protocol: Protocol,
    pub negotiated_protocol: Protocol,
    pub fallback_reasons: Vec<&'static str>,
    pub used_ranges: bool,
    pub adaptive: bool,
    pub peak_concurrency: usize,
    /// Valid measurement windows of the concurrency controller.
    pub observations: Vec<BatchObservation>,
    pub budget: BudgetSnapshot,
    /// Claims split to feed an idle lane.
    pub splits: u32,
    /// Lanes stopped for stalling or for holding back the prefix.
    pub replacements: u32,
    /// Requests re-issued after a failure or a replacement.
    pub retries: u32,
    /// Answers of 429 or 503, each of which halves the lanes.
    pub throttles: u32,
    /// Lanes replaced because they held the prefix while every other lane
    /// waited behind the `max_ahead` limit.
    pub block_replacements: u32,
    /// Lanes the controller wanted when the transfer ended.
    pub final_concurrency: usize,
    /// Requests sent, and how many of them opened a new connection.
    pub requests: u32,
    pub connections_opened: u32,
}

/// Where a resumed transfer continues.
#[derive(Clone, Debug)]
pub struct ResumePoint {
    /// First byte to fetch: the length of the verified prefix.
    pub offset: u64,
    /// The strong ETag the prefix came from; sent as `If-Range`.
    pub strong_etag: String,
    /// The total the checkpoint recorded, when it recorded one.
    pub expected_total: Option<u64>,
}

#[derive(Debug)]
pub enum TransferError {
    Cancelled,
    InvalidLimits,
    InvalidUrl(String),
    Authentication(String),
    Certificate(String),
    HostKey(String),
    ResumeRejected(String),
    /// The source is not the representation the bytes so far came from.
    /// The caller discards them and starts again from byte zero.
    IdentityChanged(String),
    /// The source answered in a way retrying will not fix: an error status,
    /// or a response that breaks the range rules.
    Rejected(String),
    Transport(String),
    Sink(io::Error),
}

impl fmt::Display for TransferError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cancelled => formatter.write_str("transfer cancelled"),
            Self::InvalidLimits => formatter.write_str("invalid HTTP transfer limits"),
            Self::InvalidUrl(detail) => write!(formatter, "invalid transfer URL: {detail}"),
            Self::Authentication(detail) => write!(formatter, "authentication failed: {detail}"),
            Self::Certificate(detail) => {
                write!(formatter, "certificate verification failed: {detail}")
            }
            Self::HostKey(detail) => write!(formatter, "host-key verification failed: {detail}"),
            Self::ResumeRejected(detail) => write!(formatter, "resume rejected: {detail}"),
            Self::IdentityChanged(detail) => write!(formatter, "source changed: {detail}"),
            Self::Rejected(detail) => formatter.write_str(detail),
            Self::Transport(detail) => formatter.write_str(detail),
            Self::Sink(error) => write!(formatter, "destination write failed: {error}"),
        }
    }
}

impl std::error::Error for TransferError {}

pub fn transfer_adaptive<F, C>(
    url: &str,
    context: &RequestContext,
    limits: TransferLimits,
    budget: &GlobalBudget,
    cancelled: C,
    sink: F,
) -> Result<TransferReport, TransferError>
where
    F: FnMut(Chunk<'_>) -> io::Result<()>,
    C: Fn() -> bool + Sync + Send,
{
    let monitor = SegmentMonitor::default();
    transfer_adaptive_observed(url, context, limits, budget, &monitor, cancelled, sink)
}

/// [`transfer_adaptive`], also reporting the ranges in flight to `monitor`.
pub fn transfer_adaptive_observed<F, C>(
    url: &str,
    context: &RequestContext,
    limits: TransferLimits,
    budget: &GlobalBudget,
    monitor: &SegmentMonitor,
    cancelled: C,
    sink: F,
) -> Result<TransferReport, TransferError>
where
    F: FnMut(Chunk<'_>) -> io::Result<()>,
    C: Fn() -> bool + Sync + Send,
{
    transfer_resumable(url, context, limits, budget, monitor, None, cancelled, sink)
}

/// Downloads `url`, or the rest of it after `resume`.
///
/// The sink gets each piece at its file offset. With the stream-first
/// scheduler, pieces of different lanes arrive in any order, never overlap,
/// and cover every byte from the start offset to the end once. The sink must
/// write at `offset` (positionally) and keep its own record of which bytes it
/// has. Each lane delivers its own bytes in order.
#[allow(clippy::too_many_arguments)]
pub fn transfer_resumable<F, C>(
    url: &str,
    context: &RequestContext,
    limits: TransferLimits,
    budget: &GlobalBudget,
    monitor: &SegmentMonitor,
    resume: Option<&ResumePoint>,
    cancelled: C,
    mut sink: F,
) -> Result<TransferReport, TransferError>
where
    F: FnMut(Chunk<'_>) -> io::Result<()>,
    C: Fn() -> bool + Sync + Send,
{
    let _clear = ClearOnDrop(monitor);
    let limits = limits.validate()?;
    scheduler::run(
        url, context, limits, budget, monitor, resume, &cancelled, &mut sink,
    )
}

pub(crate) fn configure(
    easy: &mut Easy,
    url: &str,
    context: &RequestContext,
    decision: &ProtocolDecision,
) -> Result<(), TransferError> {
    easy.url(url).map_err(curl_error)?;
    http_only(easy.raw())?;
    easy.follow_location(true).map_err(curl_error)?;
    easy.fail_on_error(true).map_err(curl_error)?;
    easy.ssl_verify_peer(true).map_err(curl_error)?;
    easy.ssl_verify_host(true).map_err(curl_error)?;
    easy.buffer_size(16 * 1024).map_err(curl_error)?;
    easy.progress(true).map_err(curl_error)?;
    easy.http_version(http_version_for(url, context, decision))
        .map_err(curl_error)?;
    for cookie in &context.cookie_lines {
        easy.cookie_list(cookie).map_err(curl_error)?;
    }
    if let Some(referer) = &context.referer {
        easy.referer(referer).map_err(curl_error)?;
    }
    Ok(())
}

pub(crate) fn http_version_for(
    url: &str,
    context: &RequestContext,
    decision: &ProtocolDecision,
) -> HttpVersion {
    match decision.preferred {
        Protocol::Http2 if context.http2_prior_knowledge => HttpVersion::V2PriorKnowledge,
        Protocol::Http3 => HttpVersion::V3,
        Protocol::Http2 if url.starts_with("https://") => HttpVersion::V2TLS,
        Protocol::Http2 | Protocol::Http1 | Protocol::Unknown => HttpVersion::Any,
    }
}

/// Limits the request and every redirect it follows to HTTP and HTTPS.
/// libcurl otherwise follows a redirect to FTP, which this path neither
/// expects nor ranges correctly. The curl crate has no safe setter for these.
pub(crate) fn http_only(handle: *mut curl_sys::CURL) -> Result<(), TransferError> {
    const HTTP_AND_HTTPS: std::os::raw::c_long =
        (curl_sys::CURLPROTO_HTTP | curl_sys::CURLPROTO_HTTPS) as std::os::raw::c_long;
    for option in [
        curl_sys::CURLOPT_PROTOCOLS,
        curl_sys::CURLOPT_REDIR_PROTOCOLS,
    ] {
        // SAFETY: `handle` is a live easy handle owned by the caller, and both
        // options take a long bitmask by value.
        let code = unsafe { curl_sys::curl_easy_setopt(handle, option, HTTP_AND_HTTPS) };
        if code != curl_sys::CURLE_OK {
            return Err(curl_error(curl::Error::new(code)));
        }
    }
    Ok(())
}

pub(crate) fn request_headers(if_range: Option<&str>) -> Result<List, TransferError> {
    let mut headers = List::new();
    headers
        .append("Accept-Encoding: identity")
        .map_err(curl_error)?;
    if let Some(etag) = if_range {
        headers
            .append(&format!("If-Range: {etag}"))
            .map_err(curl_error)?;
    }
    Ok(headers)
}

pub(crate) fn curl_error(error: curl::Error) -> TransferError {
    TransferError::Transport(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn protocol_decision_reports_capability_fallbacks() {
        assert_eq!(
            decide_protocol(ProtocolCapabilities {
                http2: true,
                http3: false,
            }),
            ProtocolDecision {
                preferred: Protocol::Http2,
                fallback_reasons: vec!["http3_unavailable"],
            }
        );
        assert_eq!(
            decide_protocol(ProtocolCapabilities {
                http2: false,
                http3: false,
            })
            .preferred,
            Protocol::Http1
        );
    }

    #[test]
    fn global_budget_rejects_impossible_reservations() {
        let budget = GlobalBudget::new(2, 1024).unwrap();
        assert!(matches!(
            budget.reserve(2048, &|| false),
            Err(TransferError::InvalidLimits)
        ));
        assert_eq!(budget.snapshot(), BudgetSnapshot::default());
    }

    #[test]
    fn global_budget_caps_concurrent_jobs_and_buffered_bytes() {
        let budget = Arc::new(GlobalBudget::new(2, 1024).unwrap());
        let workers: Vec<_> = (0..6)
            .map(|_| {
                let budget = Arc::clone(&budget);
                std::thread::spawn(move || {
                    let _permit = budget.reserve(512, &|| false).unwrap();
                    std::thread::sleep(Duration::from_millis(20));
                })
            })
            .collect();
        for worker in workers {
            worker.join().unwrap();
        }
        assert_eq!(
            budget.snapshot(),
            BudgetSnapshot {
                active_requests: 0,
                buffered_bytes: 0,
                peak_active_requests: 2,
                peak_buffered_bytes: 1024,
            }
        );
    }

    const LANE: usize = 128 * 1024;

    fn take_all<'a>(slot: &DownloadSlot<'a>) -> Vec<BudgetPermit<'a>> {
        let mut permits = Vec::new();
        while let Some(permit) = slot.try_reserve(LANE) {
            permits.push(permit);
        }
        permits
    }

    #[test]
    fn extra_lanes_never_wait_for_the_budget() {
        let budget = GlobalBudget::new(2, 2 * LANE).unwrap();
        let slot = budget.register_download();
        let first = slot.reserve(LANE, &|| false).unwrap();
        let started = std::time::Instant::now();
        let second = slot.try_reserve(LANE);
        assert!(second.is_some());
        assert!(slot.try_reserve(LANE).is_none(), "the budget is used up");
        assert!(started.elapsed() < Duration::from_millis(500));
        drop((first, second));
        assert_eq!(budget.snapshot().active_requests, 0);
    }

    #[test]
    fn downloads_get_at_least_one_lane_and_split_the_extra_ones_evenly() {
        let budget = GlobalBudget::new(8, 8 * LANE).unwrap();
        let one = budget.register_download();
        let two = budget.register_download();
        let mut lanes = vec![one.reserve(LANE, &|| false).unwrap()];
        lanes.push(two.reserve(LANE, &|| false).unwrap());
        let first = take_all(&one);
        let second = take_all(&two);
        // Each keeps its first lane and gains extras up to an even share of 8.
        assert_eq!(1 + first.len(), 4);
        assert_eq!(1 + second.len(), 4);
        assert_eq!(budget.snapshot().active_requests, 8);
        drop((lanes, first, second));
    }

    #[test]
    fn a_download_that_stopped_growing_leaves_the_rest_to_the_others() {
        let budget = GlobalBudget::new(8, 8 * LANE).unwrap();
        let settled = budget.register_download();
        let growing = budget.register_download();
        let mut held = vec![settled.reserve(LANE, &|| false).unwrap()];
        held.push(growing.reserve(LANE, &|| false).unwrap());
        // The settled download keeps 2 lanes and stops asking.
        held.extend(settled.try_reserve(LANE));
        settled.set_growing(false);
        assert!(settled.try_reserve(LANE).is_none());
        let extra = take_all(&growing);
        assert_eq!(1 + extra.len(), 6, "8 lanes minus the 2 that are settled");
        drop((held, extra));
    }

    #[test]
    fn extra_lanes_leave_room_for_a_download_still_waiting_for_its_first() {
        let budget = GlobalBudget::new(4, 4 * LANE).unwrap();
        let busy = budget.register_download();
        let waiting = budget.register_download();
        // Not growing, so it does not count in the even share; only the
        // guard for its first lane is left.
        waiting.set_growing(false);
        let mut held = vec![busy.reserve(LANE, &|| false).unwrap()];
        held.extend(take_all(&busy));
        assert_eq!(held.len(), 3, "one lane stays free for the newcomer");
        assert!(budget.reserve(LANE, &|| false).is_ok());
    }
}
