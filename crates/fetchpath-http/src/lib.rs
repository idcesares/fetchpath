use curl::easy::{Easy, HttpVersion, List};
use std::cell::{Cell, RefCell};
use std::fmt;
use std::io;
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

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

#[derive(Clone, Copy, Debug)]
pub struct TransferLimits {
    pub max_active_requests: usize,
    pub max_buffered_bytes: usize,
    pub max_concurrency: usize,
    pub segment_bytes: usize,
    pub min_adaptive_bytes: u64,
}

impl Default for TransferLimits {
    fn default() -> Self {
        Self {
            max_active_requests: 8,
            max_buffered_bytes: 8 * 1024 * 1024,
            max_concurrency: 4,
            segment_bytes: 1024 * 1024,
            min_adaptive_bytes: 4 * 1024 * 1024,
        }
    }
}

impl TransferLimits {
    fn validate(self) -> Result<Self, TransferError> {
        if self.max_active_requests == 0
            || self.max_buffered_bytes == 0
            || self.max_concurrency == 0
            || self.segment_bytes == 0
            || self.segment_bytes > self.max_buffered_bytes
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
        state.active_requests += 1;
        state.buffered_bytes += buffered_bytes;
        state.peak_active_requests = state.peak_active_requests.max(state.active_requests);
        state.peak_buffered_bytes = state.peak_buffered_bytes.max(state.buffered_bytes);
        Ok(BudgetPermit {
            budget: self,
            buffered_bytes,
        })
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

pub struct BudgetPermit<'a> {
    budget: &'a GlobalBudget,
    buffered_bytes: usize,
}

impl Drop for BudgetPermit<'_> {
    fn drop(&mut self) {
        let mut state = self.budget.state.lock().unwrap();
        state.active_requests -= 1;
        state.buffered_bytes -= self.buffered_bytes;
        self.budget.changed.notify_all();
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ContentRange {
    pub start: u64,
    pub end: u64,
    pub total: u64,
}

#[derive(Clone, Debug, Default)]
struct ResponseHeaders {
    status: Option<u32>,
    protocol: Protocol,
    etag: Option<String>,
    content_length: Option<u64>,
    content_range: Option<ContentRange>,
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
            self.content_range = None;
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
            self.content_range = parse_content_range(value);
        }
    }
}

fn parse_content_range(value: &str) -> Option<ContentRange> {
    let value = value.strip_prefix("bytes ")?;
    let (range, total) = value.split_once('/')?;
    let (start, end) = range.split_once('-')?;
    let start = start.parse().ok()?;
    let end = end.parse().ok()?;
    let total = total.parse().ok()?;
    (start <= end && end < total).then_some(ContentRange { start, end, total })
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

#[derive(Clone, Debug)]
pub struct BatchObservation {
    pub concurrency: usize,
    pub bytes: u64,
    pub elapsed_ms: f64,
}

#[derive(Clone, Debug)]
pub struct TransferReport {
    pub bytes: u64,
    pub total_bytes: Option<u64>,
    pub strong_etag: Option<String>,
    pub preferred_protocol: Protocol,
    pub negotiated_protocol: Protocol,
    pub fallback_reasons: Vec<&'static str>,
    pub used_ranges: bool,
    pub adaptive: bool,
    pub peak_concurrency: usize,
    pub observations: Vec<BatchObservation>,
    pub budget: BudgetSnapshot,
}

#[derive(Debug)]
pub enum TransferError {
    Cancelled,
    InvalidLimits,
    RestartSequential(String),
    Transport(String),
    Sink(io::Error),
}

impl fmt::Display for TransferError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cancelled => formatter.write_str("transfer cancelled"),
            Self::InvalidLimits => formatter.write_str("invalid HTTP transfer limits"),
            Self::RestartSequential(detail) => formatter.write_str(detail),
            Self::Transport(detail) => formatter.write_str(detail),
            Self::Sink(error) => write!(formatter, "destination write failed: {error}"),
        }
    }
}

impl std::error::Error for TransferError {}

struct AdaptiveController {
    current: usize,
    maximum: usize,
    previous_rate: Option<f64>,
    healthy_streak: usize,
    cooldown: usize,
}

impl AdaptiveController {
    fn new(maximum: usize) -> Self {
        Self {
            current: 1,
            maximum,
            previous_rate: None,
            healthy_streak: 0,
            cooldown: 0,
        }
    }

    fn concurrency(&self) -> usize {
        self.current
    }

    fn observe(&mut self, bytes: u64, elapsed: Duration, backpressure: bool) {
        let rate = bytes as f64 / elapsed.as_secs_f64().max(0.000_001);
        if backpressure
            || self
                .previous_rate
                .is_some_and(|previous| rate < previous * 0.80)
        {
            self.current = self.current.saturating_sub(1).max(1);
            self.healthy_streak = 0;
            self.cooldown = 2;
        } else if self.cooldown > 0 {
            self.cooldown -= 1;
        } else {
            self.healthy_streak += 1;
            if self.healthy_streak >= 2 && self.current < self.maximum {
                self.current += 1;
                self.healthy_streak = 0;
            }
        }
        self.previous_rate = Some(rate);
    }
}

struct ProbeResult {
    headers: ResponseHeaders,
    buffered: Vec<u8>,
    streamed: u64,
}

pub fn transfer_adaptive<F, C>(
    url: &str,
    context: &RequestContext,
    limits: TransferLimits,
    budget: &GlobalBudget,
    cancelled: C,
    mut sink: F,
) -> Result<TransferReport, TransferError>
where
    F: FnMut(Chunk<'_>) -> io::Result<()>,
    C: Fn() -> bool + Sync + Send,
{
    let limits = limits.validate()?;
    let capabilities = ProtocolCapabilities::detect();
    let decision = decide_protocol(capabilities);
    let probe = probe(url, context, &decision, budget, &cancelled, &mut sink)?;
    if probe.headers.status == Some(200) {
        if let Some(expected) = probe.headers.content_length
            && expected != probe.streamed
        {
            return Err(TransferError::Transport(format!(
                "response declared {expected} bytes but delivered {}",
                probe.streamed
            )));
        }
        let mut fallback_reasons = decision.fallback_reasons;
        if decision.preferred != probe.headers.protocol {
            fallback_reasons.push("negotiated_lower_protocol");
        }
        return Ok(TransferReport {
            bytes: probe.streamed,
            total_bytes: probe.headers.content_length,
            strong_etag: strong_etag(probe.headers.etag.as_deref()),
            preferred_protocol: decision.preferred,
            negotiated_protocol: probe.headers.protocol,
            fallback_reasons,
            used_ranges: false,
            adaptive: false,
            peak_concurrency: 1,
            observations: Vec::new(),
            budget: budget.snapshot(),
        });
    }

    let range = probe
        .headers
        .content_range
        .filter(|range| range.start == 0 && range.end == 0)
        .ok_or_else(|| {
            TransferError::RestartSequential("probe returned an invalid Content-Range".into())
        })?;
    let etag = strong_etag(probe.headers.etag.as_deref()).ok_or_else(|| {
        TransferError::RestartSequential("segmentation requires a strong ETag".into())
    })?;
    if probe.buffered.len() != 1 {
        return Err(TransferError::Transport(format!(
            "one-byte probe delivered {} bytes",
            probe.buffered.len()
        )));
    }
    sink(Chunk {
        offset: 0,
        bytes: &probe.buffered,
        strong_etag: Some(&etag),
        total_bytes: Some(range.total),
    })
    .map_err(TransferError::Sink)?;

    let mut next = 1_u64;
    let mut observations = Vec::new();
    let adaptive_maximum = if range.total >= limits.min_adaptive_bytes {
        limits.max_concurrency.min(limits.max_active_requests)
    } else {
        1
    };
    let mut controller = AdaptiveController::new(adaptive_maximum);
    let mut peak_concurrency = 1;
    while next < range.total {
        if cancelled() {
            return Err(TransferError::Cancelled);
        }
        let remaining_chunks = (range.total - next).div_ceil(limits.segment_bytes as u64) as usize;
        let concurrency = controller.concurrency().min(remaining_chunks);
        peak_concurrency = peak_concurrency.max(concurrency);
        let mut specs = Vec::with_capacity(concurrency);
        for _ in 0..concurrency {
            if next >= range.total {
                break;
            }
            let end = (next + limits.segment_bytes as u64 - 1).min(range.total - 1);
            specs.push((next, end));
            next = end + 1;
        }
        let started = Instant::now();
        let decision_ref = &decision;
        let cancelled_ref = &cancelled;
        let results = std::thread::scope(|scope| {
            let handles: Vec<_> = specs
                .iter()
                .map(|&(start, end)| {
                    let etag = etag.clone();
                    scope.spawn(move || {
                        fetch_range(
                            url,
                            context,
                            decision_ref,
                            budget,
                            cancelled_ref,
                            start,
                            end,
                            range.total,
                            &etag,
                        )
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|handle| {
                    let result = handle.join().map_err(|_| {
                        TransferError::RestartSequential("range worker panicked".into())
                    })?;
                    result.map_err(|error| match error {
                        TransferError::Cancelled => TransferError::Cancelled,
                        other => TransferError::RestartSequential(other.to_string()),
                    })
                })
                .collect::<Result<Vec<_>, TransferError>>()
        })?;
        let elapsed = started.elapsed();
        let mut results = results;
        results.sort_by_key(|chunk| chunk.start);
        let batch_bytes = results.iter().map(|chunk| chunk.bytes.len() as u64).sum();
        for chunk in &results {
            sink(Chunk {
                offset: chunk.start,
                bytes: &chunk.bytes,
                strong_etag: Some(&etag),
                total_bytes: Some(range.total),
            })
            .map_err(TransferError::Sink)?;
        }
        observations.push(BatchObservation {
            concurrency,
            bytes: batch_bytes,
            elapsed_ms: elapsed.as_secs_f64() * 1000.0,
        });
        controller.observe(batch_bytes, elapsed, false);
    }
    let mut fallback_reasons = decision.fallback_reasons;
    if decision.preferred != probe.headers.protocol {
        fallback_reasons.push("negotiated_lower_protocol");
    }
    Ok(TransferReport {
        bytes: range.total,
        total_bytes: Some(range.total),
        strong_etag: Some(etag),
        preferred_protocol: decision.preferred,
        negotiated_protocol: probe.headers.protocol,
        fallback_reasons,
        used_ranges: true,
        adaptive: range.total >= limits.min_adaptive_bytes && peak_concurrency > 1,
        peak_concurrency,
        observations,
        budget: budget.snapshot(),
    })
}

fn configure(
    easy: &mut Easy,
    url: &str,
    context: &RequestContext,
    decision: &ProtocolDecision,
) -> Result<(), TransferError> {
    easy.url(url).map_err(curl_error)?;
    easy.follow_location(true).map_err(curl_error)?;
    easy.fail_on_error(true).map_err(curl_error)?;
    easy.ssl_verify_peer(true).map_err(curl_error)?;
    easy.ssl_verify_host(true).map_err(curl_error)?;
    easy.buffer_size(16 * 1024).map_err(curl_error)?;
    easy.progress(true).map_err(curl_error)?;
    let version = match decision.preferred {
        Protocol::Http2 if context.http2_prior_knowledge => HttpVersion::V2PriorKnowledge,
        Protocol::Http3 => HttpVersion::V3,
        Protocol::Http2 if url.starts_with("https://") => HttpVersion::V2TLS,
        Protocol::Http2 | Protocol::Http1 | Protocol::Unknown => HttpVersion::Any,
    };
    easy.http_version(version).map_err(curl_error)?;
    for cookie in &context.cookie_lines {
        easy.cookie_list(cookie).map_err(curl_error)?;
    }
    if let Some(referer) = &context.referer {
        easy.referer(referer).map_err(curl_error)?;
    }
    Ok(())
}

fn request_headers(if_range: Option<&str>) -> Result<List, TransferError> {
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

fn probe<F, C>(
    url: &str,
    context: &RequestContext,
    decision: &ProtocolDecision,
    budget: &GlobalBudget,
    cancelled: &C,
    sink: &mut F,
) -> Result<ProbeResult, TransferError>
where
    F: FnMut(Chunk<'_>) -> io::Result<()>,
    C: Fn() -> bool + Sync,
{
    let _permit = budget.reserve(1, cancelled)?;
    let mut easy = Easy::new();
    configure(&mut easy, url, context, decision)?;
    easy.range("0-0").map_err(curl_error)?;
    easy.http_headers(request_headers(None)?)
        .map_err(curl_error)?;
    let headers = RefCell::new(ResponseHeaders::default());
    let buffered = RefCell::new(Vec::with_capacity(1));
    let streamed = Cell::new(0_u64);
    let sink_error = RefCell::new(None);
    let result = {
        let mut transfer = easy.transfer();
        transfer
            .header_function(|line| {
                headers.borrow_mut().ingest(line);
                true
            })
            .map_err(curl_error)?;
        transfer
            .write_function(|data| {
                if headers.borrow().status == Some(200) {
                    let offset = streamed.get();
                    let headers = headers.borrow();
                    if let Err(error) = sink(Chunk {
                        offset,
                        bytes: data,
                        strong_etag: strong_etag(headers.etag.as_deref()).as_deref(),
                        total_bytes: headers.content_length,
                    }) {
                        *sink_error.borrow_mut() = Some(error);
                        return Ok(0);
                    }
                    streamed.set(offset + data.len() as u64);
                    Ok(data.len())
                } else {
                    let mut body = buffered.borrow_mut();
                    if body.len() + data.len() > 1 {
                        return Ok(0);
                    }
                    body.extend_from_slice(data);
                    Ok(data.len())
                }
            })
            .map_err(curl_error)?;
        transfer
            .progress_function(|_, _, _, _| !cancelled())
            .map_err(curl_error)?;
        transfer.perform()
    };
    if let Some(error) = sink_error.into_inner() {
        return Err(TransferError::Sink(error));
    }
    if cancelled() {
        return Err(TransferError::Cancelled);
    }
    if let Err(error) = result {
        let callback_status = headers.borrow().status;
        let easy_status = easy.response_code().ok();
        if callback_status == Some(206) {
            return Err(TransferError::Transport("HTTP status 206".into()));
        }
        if let Some(status) = callback_status
            .or(easy_status)
            .filter(|status| *status >= 400)
        {
            return Err(TransferError::Transport(format!("HTTP status {status}")));
        }
        return Err(curl_error(error));
    }
    let headers = headers.into_inner();
    if !matches!(headers.status, Some(200 | 206)) {
        return Err(TransferError::Transport(format!(
            "HTTP status {}",
            headers.status.unwrap_or_default()
        )));
    }
    Ok(ProbeResult {
        headers,
        buffered: buffered.into_inner(),
        streamed: streamed.get(),
    })
}

struct RangeChunk {
    start: u64,
    bytes: Vec<u8>,
}

#[allow(clippy::too_many_arguments)]
fn fetch_range<C>(
    url: &str,
    context: &RequestContext,
    decision: &ProtocolDecision,
    budget: &GlobalBudget,
    cancelled: &C,
    start: u64,
    end: u64,
    total: u64,
    etag: &str,
) -> Result<RangeChunk, TransferError>
where
    C: Fn() -> bool + Sync,
{
    let expected = (end - start + 1) as usize;
    let _permit = budget.reserve(expected, cancelled)?;
    let mut easy = Easy::new();
    configure(&mut easy, url, context, decision)?;
    easy.range(&format!("{start}-{end}")).map_err(curl_error)?;
    easy.http_headers(request_headers(Some(etag))?)
        .map_err(curl_error)?;
    let headers = RefCell::new(ResponseHeaders::default());
    let body = RefCell::new(Vec::with_capacity(expected));
    let overflow = Cell::new(false);
    let result = {
        let mut transfer = easy.transfer();
        transfer
            .header_function(|line| {
                headers.borrow_mut().ingest(line);
                true
            })
            .map_err(curl_error)?;
        transfer
            .write_function(|data| {
                let mut body = body.borrow_mut();
                if body.len() + data.len() > expected {
                    overflow.set(true);
                    return Ok(0);
                }
                body.extend_from_slice(data);
                Ok(data.len())
            })
            .map_err(curl_error)?;
        transfer
            .progress_function(|_, _, _, _| !cancelled())
            .map_err(curl_error)?;
        transfer.perform()
    };
    if cancelled() {
        return Err(TransferError::Cancelled);
    }
    if overflow.get() {
        return Err(TransferError::Transport(
            "range exceeded its reservation".into(),
        ));
    }
    result.map_err(curl_error)?;
    let headers = headers.into_inner();
    let returned = headers.content_range;
    if headers.status != Some(206)
        || returned != Some(ContentRange { start, end, total })
        || strong_etag(headers.etag.as_deref()).as_deref() != Some(etag)
    {
        return Err(TransferError::Transport(format!(
            "range {start}-{end} did not preserve status, bounds, and identity"
        )));
    }
    let body = body.into_inner();
    if body.len() != expected {
        return Err(TransferError::Transport(format!(
            "range {start}-{end} declared {expected} bytes but delivered {}",
            body.len()
        )));
    }
    Ok(RangeChunk { start, bytes: body })
}

fn curl_error(error: curl::Error) -> TransferError {
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
    fn controller_grows_with_hysteresis_and_decreases_on_regression() {
        let mut controller = AdaptiveController::new(4);
        controller.observe(1000, Duration::from_secs(1), false);
        assert_eq!(controller.concurrency(), 1);
        controller.observe(1100, Duration::from_secs(1), false);
        assert_eq!(controller.concurrency(), 2);
        controller.observe(100, Duration::from_secs(1), false);
        assert_eq!(controller.concurrency(), 1);
        controller.observe(2000, Duration::from_secs(1), false);
        controller.observe(2100, Duration::from_secs(1), false);
        assert_eq!(
            controller.concurrency(),
            1,
            "cooldown prevents immediate oscillation"
        );
    }

    #[test]
    fn content_ranges_are_strict() {
        assert_eq!(
            parse_content_range("bytes 1-9/10"),
            Some(ContentRange {
                start: 1,
                end: 9,
                total: 10,
            })
        );
        assert_eq!(parse_content_range("bytes 9-1/10"), None);
        assert_eq!(parse_content_range("bytes 1-10/10"), None);
        assert_eq!(parse_content_range("bytes */10"), None);
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
}
