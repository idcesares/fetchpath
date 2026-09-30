//! The round scheduler FP-015 shipped: one probe byte, then rounds of ranges
//! held in memory and written in order. It stays behind an internal switch
//! (`FETCHPATH_HTTP_SCHEDULER=rounds`) so the benchmark can compare it with the
//! stream-first scheduler. FP-087 deletes it.

use crate::{
    BatchObservation, Chunk, ContentRange, GlobalBudget, ProtocolCapabilities, ProtocolDecision,
    RequestContext, ResponseHeaders, SEGMENT_SECONDS, SegmentMonitor, TransferError,
    TransferLimits, TransferReport, configure, curl_error, decide_protocol, request_headers,
    strong_etag,
};
use curl::easy::Easy;
use std::cell::{Cell, RefCell};
use std::io;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

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

pub(crate) fn run<F, C>(
    url: &str,
    context: &RequestContext,
    limits: TransferLimits,
    budget: &GlobalBudget,
    monitor: &SegmentMonitor,
    cancelled: C,
    mut sink: F,
) -> Result<TransferReport, TransferError>
where
    F: FnMut(Chunk<'_>) -> io::Result<()>,
    C: Fn() -> bool + Sync + Send,
{
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
            ..TransferReport::default()
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
    // One handle per lane, kept across rounds, so each lane reuses its
    // connections (and its redirect's) instead of opening new ones per range.
    let mut lanes: Vec<Easy> = (0..adaptive_maximum).map(|_| Easy::new()).collect();
    let mut segment = limits.min_segment_bytes;
    let mut peak_concurrency = 1;
    while next < range.total {
        if cancelled() {
            return Err(TransferError::Cancelled);
        }
        let remaining_chunks = (range.total - next).div_ceil(segment as u64) as usize;
        let concurrency = controller.concurrency().min(remaining_chunks);
        peak_concurrency = peak_concurrency.max(concurrency);
        let mut specs = Vec::with_capacity(concurrency);
        for _ in 0..concurrency {
            if next >= range.total {
                break;
            }
            let end = (next + segment as u64 - 1).min(range.total - 1);
            specs.push((next, end));
            next = end + 1;
        }
        let started = Instant::now();
        let decision_ref = &decision;
        let cancelled_ref = &cancelled;
        let live = monitor.begin(&specs);
        let results = std::thread::scope(|scope| {
            let handles: Vec<_> = specs
                .iter()
                .zip(&live)
                .zip(lanes.iter_mut())
                .map(|((&(start, end), segment), easy)| {
                    let etag = etag.clone();
                    scope.spawn(move || {
                        fetch_range(
                            easy,
                            url,
                            context,
                            decision_ref,
                            budget,
                            cancelled_ref,
                            start,
                            end,
                            range.total,
                            &etag,
                            &segment.received,
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
        // Written, so no longer in flight.
        monitor.clear();
        observations.push(BatchObservation {
            concurrency,
            bytes: batch_bytes,
            elapsed_ms: elapsed.as_secs_f64() * 1000.0,
        });
        controller.observe(batch_bytes, elapsed, false);
        // Each lane's rate sizes the next ranges.
        let lane_rate = batch_bytes as f64 / concurrency as f64 / elapsed.as_secs_f64().max(0.001);
        segment = ((lane_rate * SEGMENT_SECONDS) as usize)
            .clamp(limits.min_segment_bytes, limits.segment_bytes);
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
        ..TransferReport::default()
    })
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
    easy: &mut Easy,
    url: &str,
    context: &RequestContext,
    decision: &ProtocolDecision,
    budget: &GlobalBudget,
    cancelled: &C,
    start: u64,
    end: u64,
    total: u64,
    etag: &str,
    progress: &AtomicU64,
) -> Result<RangeChunk, TransferError>
where
    C: Fn() -> bool + Sync,
{
    let expected = (end - start + 1) as usize;
    let _permit = budget.reserve(expected, cancelled)?;
    configure(easy, url, context, decision)?;
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
                progress.fetch_add(data.len() as u64, Ordering::Relaxed);
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
