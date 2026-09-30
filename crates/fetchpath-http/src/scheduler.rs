//! The stream-first, work-stealing scheduler (FP-083 design, FP-085).
//!
//! One thread drives every lane of a download through one curl multi handle.
//! A lane is one request that owns a claim `[start, end)` of the file. Bytes
//! go to the sink as they arrive, at their own offset, so a range is never
//! held in memory waiting for the ranges before it.
//!
//! The rule that keeps this safe is clipping: every write is cut at the
//! claim's current end. A claim can shrink while its owner is mid-transfer
//! (another lane takes its tail, or the lane is replaced), and the owner can
//! only stop when its callback returns. A clipped short write is a deliberate
//! stop, not a transport error.

use crate::controller::{Change, Controller};
use crate::frontier::Frontier;
use crate::{
    BatchObservation, BudgetPermit, Chunk, DownloadSlot, GlobalBudget, LiveSegment, Protocol,
    ProtocolCapabilities, ProtocolDecision, RequestContext, ResponseHeaders, ResumePoint,
    Scheduler, SegmentMonitor, TransferError, TransferLimits, TransferReport, curl_error,
    decide_protocol, http_only, http_version_for, request_headers, strong_etag,
};
use curl::easy::{Easy2, Handler, WriteError};
use curl::multi::{Easy2Handle, Multi};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet, VecDeque};
use std::io;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

/// A range that fails this many times without progress ends the attempt.
const RETRY_BUDGET: u32 = 3;
/// A failed request that still moved this many bytes does not count as a
/// failure of its range.
const PROGRESS_RESETS_BUDGET: u64 = 64 * 1024;
const DEFAULT_RETRY_AFTER: Duration = Duration::from_secs(1);
/// Servers that ignore a range this often stop being used in parallel.
const IGNORED_RANGE_LIMIT: u32 = 2;
/// Wait per loop turn when nothing is ready.
const POLL: Duration = Duration::from_millis(20);
/// How long claiming may stay blocked by `max_ahead` before the lane that
/// holds the prefix is replaced.
const BLOCK_GRACE: Duration = Duration::from_secs(1);
const AHEAD_SECONDS: f64 = 3.0;
const STALL_RATE_SHARE: f64 = 0.10;
const RATE_HISTORY: Duration = Duration::from_secs(3);
/// The time to first byte assumed until one has been measured.
const DEFAULT_TTFB: Duration = Duration::from_millis(50);
const BACKPRESSURE_SHARE: f64 = 0.25;
/// A claim is at most this many times the longest one handed out so far.
const CLAIM_GROWTH: u64 = 4;
/// Before its first byte a range lane may wait this many times the median
/// time to first byte seen so far, if that is longer than the stall timeout.
const TTFB_ALLOWANCE: u32 = 3;

fn retry_attempts(previous: u32, received: u64, replacement: bool) -> u32 {
    let forgiven = if replacement {
        1
    } else {
        PROGRESS_RESETS_BUDGET
    };
    if received >= forgiven {
        0
    } else {
        previous + 1
    }
}

fn retry_deadline(now: Instant, wait: Duration) -> Result<Instant, TransferError> {
    now.checked_add(wait).ok_or_else(|| {
        TransferError::Transport("Retry-After exceeds the supported clock range".into())
    })
}

type SinkFn<'a> = dyn FnMut(Chunk<'_>) -> io::Result<()> + 'a;
type CancelFn<'a> = dyn Fn() -> bool + Sync + 'a;

/// The one multi handle of a download. Multiplexing is off so that every lane
/// keeps its own connection and congestion window (spec 4.7); an origin that
/// limits each connection would otherwise see one connection instead of many.
pub(crate) fn new_multi() -> Result<Multi, TransferError> {
    let mut multi = Multi::new();
    multi
        .pipelining(false, false)
        .map_err(|error| TransferError::Transport(error.to_string()))?;
    Ok(multi)
}

fn multi_error(error: curl::MultiError) -> TransferError {
    TransferError::Transport(error.to_string())
}

// ---------------------------------------------------------------------------
// Response classification

/// What the first response of a download means (spec 4.1).
#[derive(Debug, Eq, PartialEq)]
pub(crate) enum First {
    /// One request finishes the file.
    Sequential {
        etag: Option<String>,
        total: Option<u64>,
        expected_len: Option<u64>,
    },
    /// Ranges are safe: same strong validator on every lane.
    Ranged {
        etag: String,
        total: u64,
        /// One past the last byte this response covers.
        covers_end: u64,
    },
    /// The bytes so far may belong to another representation: start over.
    IdentityRestart(String),
    /// Ask again without `Range`.
    PlainGet,
    Fail(String),
}

pub(crate) fn classify_first(
    headers: &ResponseHeaders,
    offset: u64,
    resume: Option<&ResumePoint>,
    plain: bool,
) -> First {
    match headers.status {
        Some(200) if offset == 0 => First::Sequential {
            etag: strong_etag(headers.etag.as_deref()),
            total: headers.content_length,
            expected_len: headers.content_length,
        },
        Some(200) => First::IdentityRestart("the source answered a resume with 200".into()),
        Some(206) => {
            if plain {
                return First::Fail("HTTP status 206".into());
            }
            let Some(range) = headers.open_range else {
                return if offset > 0 {
                    // Retained bytes cannot be trusted next to an answer that
                    // does not say which bytes it carries.
                    First::IdentityRestart("a resume was answered without a Content-Range".into())
                } else {
                    First::Fail("HTTP status 206".into())
                };
            };
            if range.start != offset {
                return if offset > 0 {
                    First::IdentityRestart(format!(
                        "resume asked for byte {offset} and got {}",
                        range.start
                    ))
                } else {
                    First::Fail(format!("range starts at {} instead of 0", range.start))
                };
            }
            let strong = strong_etag(headers.etag.as_deref());
            if let Some(resume) = resume {
                if strong.as_deref() != Some(resume.strong_etag.as_str()) {
                    return First::IdentityRestart("the validator changed".into());
                }
                if resume.expected_total.is_some() && resume.expected_total != range.total {
                    return First::IdentityRestart("the total size changed".into());
                }
            }
            let len = range.end - range.start + 1;
            match (range.total, strong) {
                (None, etag) => First::Sequential {
                    etag,
                    total: None,
                    expected_len: Some(len),
                },
                (Some(total), Some(etag)) => First::Ranged {
                    etag,
                    total,
                    covers_end: range.end + 1,
                },
                (Some(total), None) if range.end + 1 == total => First::Sequential {
                    etag: None,
                    total: Some(total),
                    expected_len: Some(len),
                },
                (Some(_), None) => First::PlainGet,
            }
        }
        Some(416) if offset == 0 => First::PlainGet,
        Some(416) => First::IdentityRestart("the resume offset is past the end".into()),
        Some(status) => First::Fail(format!("HTTP status {status}")),
        None => First::Fail("no HTTP status".into()),
    }
}

/// Why a lane's response was refused.
#[derive(Debug)]
enum Verdict {
    Identity(String),
    PlainGet,
    /// A later lane got 200 with the same strong validator: that node
    /// ignores ranges.
    IgnoredRange,
    Fail(String),
}

/// A later lane's response must be exactly the range asked for, from the
/// representation the first response named.
fn check_range_response(
    headers: &ResponseHeaders,
    claim_start: u64,
    covers_end: u64,
    etag: &str,
    total: u64,
) -> Result<(), Verdict> {
    let strong = strong_etag(headers.etag.as_deref());
    match headers.status {
        Some(206) => {
            let Some(range) = headers.open_range else {
                return Err(Verdict::Fail("206 without a usable Content-Range".into()));
            };
            if strong.as_deref() != Some(etag) {
                return Err(Verdict::Identity("the validator changed".into()));
            }
            if range.total != Some(total) {
                return Err(Verdict::Identity("the total size changed".into()));
            }
            if range.start != claim_start || range.end + 1 < covers_end {
                return Err(Verdict::Fail(format!(
                    "range {claim_start}-{} came back as {}-{}",
                    covers_end - 1,
                    range.start,
                    range.end
                )));
            }
            Ok(())
        }
        Some(200) => match strong.as_deref() {
            Some(same) if same == etag => Err(Verdict::IgnoredRange),
            _ => Err(Verdict::Identity(
                "a range request got a different representation".into(),
            )),
        },
        Some(status) => Err(Verdict::Fail(format!("HTTP status {status}"))),
        None => Err(Verdict::Fail("no HTTP status".into())),
    }
}

// ---------------------------------------------------------------------------
// Claims and shared state

struct Claim {
    live: Arc<LiveSegment>,
    /// One past the last byte the request will deliver; a claim may grow up
    /// to here and no further.
    cover_end: Cell<u64>,
    /// Failures of this range so far without progress.
    attempts: u32,
    retired: Cell<bool>,
    clipped: Cell<bool>,
    started: Instant,
    first_byte: Cell<Option<Instant>>,
    last_byte: Cell<Instant>,
    history: RefCell<VecDeque<(Instant, u64)>>,
}

impl Claim {
    fn new(live: Arc<LiveSegment>, cover_end: u64, attempts: u32) -> Rc<Self> {
        let now = Instant::now();
        Rc::new(Self {
            live,
            cover_end: Cell::new(cover_end),
            attempts,
            retired: Cell::new(false),
            clipped: Cell::new(false),
            started: now,
            first_byte: Cell::new(None),
            last_byte: Cell::new(now),
            history: RefCell::new(VecDeque::new()),
        })
    }

    fn start(&self) -> u64 {
        self.live.start
    }

    fn end(&self) -> u64 {
        self.live.end.load(Ordering::Relaxed)
    }

    fn set_end(&self, end: u64) {
        self.live.end.store(end, Ordering::Relaxed);
    }

    fn received(&self) -> u64 {
        self.live.received.load(Ordering::Relaxed)
    }

    /// The next byte this claim will write.
    fn pos(&self) -> u64 {
        self.start() + self.received()
    }

    fn remaining(&self) -> u64 {
        self.end().saturating_sub(self.pos())
    }

    /// Bytes per second over the life of the request, once it has run long
    /// enough to mean something.
    fn rate(&self, now: Instant) -> Option<f64> {
        let first = self.first_byte.get()?;
        let elapsed = now.duration_since(first).as_secs_f64();
        (elapsed >= 0.05 && self.received() > 0).then(|| self.received() as f64 / elapsed)
    }

    /// Bytes per second over the last `RATE_HISTORY`, or `None` without that
    /// much history.
    fn recent_rate(&self, now: Instant) -> Option<f64> {
        let history = self.history.borrow();
        let (then, bytes) = history.front()?;
        let span = now.duration_since(*then);
        (span >= RATE_HISTORY).then(|| (self.received() - bytes) as f64 / span.as_secs_f64())
    }

    fn sample(&self, now: Instant) {
        let mut history = self.history.borrow_mut();
        let cutoff = now.checked_sub(RATE_HISTORY + Duration::from_millis(500));
        history.push_back((now, self.received()));
        while let (Some(cutoff), Some(&(then, _))) = (cutoff, history.front()) {
            if then < cutoff && history.len() > 2 {
                history.pop_front();
            } else {
                break;
            }
        }
        // Keep the oldest sample that is still at least `RATE_HISTORY` old.
        while history.len() > 2
            && history
                .get(1)
                .is_some_and(|(then, _)| now.duration_since(*then) >= RATE_HISTORY)
        {
            history.pop_front();
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Phase {
    /// The first request has not been classified.
    Opening,
    Ranged,
    /// One request finishes the file.
    Sequential {
        expected_len: Option<u64>,
    },
}

struct State {
    phase: Phase,
    /// First byte this transfer fetches.
    offset: u64,
    resume: Option<ResumePoint>,
    etag: Option<String>,
    total: Option<u64>,
    frontier: Frontier,
    claims: Vec<Rc<Claim>>,
    /// How far past the prefix a new claim may start.
    max_ahead: u64,
    /// The time-sized length of the next claim.
    claim_bytes: u64,
    min_segment: u64,
    min_adaptive: u64,
    /// Length of the claim lane 0 started with; claiming near the start may
    /// reach at least this far.
    first_claim: u64,
    /// The longest claim handed out so far. A claim is at most four times this, so
    /// a burst that inflates the rate estimate cannot give one lane a large
    /// share of the file at once.
    largest_claim: u64,
}

/// The run-ahead budget is three seconds of recently measured aggregate
/// goodput, or 64 KiB while the link is slow or not measured yet. Minimum
/// claim sizes and file size never enlarge the amount a pause may discard.
fn max_ahead_for(goodput: Option<f64>) -> u64 {
    ((goodput.unwrap_or(0.0) * AHEAD_SECONDS) as u64).max(64 * 1024)
}

/// What the next idle lane should do about the frontier.
#[derive(Debug, Eq, PartialEq)]
enum Plan {
    /// Fetch `[start, end)`.
    Claim(u64, u64),
    /// The frontier begins too far past the prefix (`max_ahead`).
    Blocked,
    /// Nothing is unclaimed.
    Empty,
}

impl State {
    /// Chooses the next claim and takes it from the frontier.
    ///
    /// The size is time-based, `rate x 1.5 s` with a `min_segment` floor,
    /// grows by at most a factor of four, is at most an even share of what is
    /// left near the end, and does not start beyond `prefix + max_ahead`.
    fn plan_claim(&mut self, lanes: usize) -> Plan {
        let Some((start, end)) = self.frontier.front() else {
            return Plan::Empty;
        };
        let limit = self.prefix().saturating_add(self.max_ahead);
        let contiguous = start == self.prefix();
        if !contiguous && (start >= limit || limit - start < self.min_segment) {
            return Plan::Blocked;
        }
        let front_len = end - start;
        // Near the end a claim is an even share of everything still
        // unreceived, in flight or not, so the lanes finish together. Sharing
        // only the unclaimed bytes would halve the tail again and again.
        let in_flight: u64 = self
            .claims
            .iter()
            .filter(|claim| !claim.retired.get())
            .map(|claim| claim.remaining())
            .sum();
        let share = (self.frontier.bytes() + in_flight)
            .div_ceil(lanes.max(1) as u64)
            .max(self.min_segment);
        let growth = self
            .largest_claim
            .saturating_mul(CLAIM_GROWTH)
            .max(self.min_segment);
        let room = if contiguous { u64::MAX } else { limit - start };
        let mut size = self
            .claim_bytes
            .max(self.min_segment)
            .min(growth)
            .min(share)
            .min(room)
            .min(front_len);
        if front_len - size < self.min_segment && front_len <= room {
            // No crumb at the end of an extent.
            size = front_len;
        }
        let (start, end) = self
            .frontier
            .take_front(size)
            .expect("the front extent was just seen");
        self.largest_claim = self.largest_claim.max(end - start);
        Plan::Claim(start, end)
    }
}

impl State {
    /// The first byte not yet written.
    fn prefix(&self) -> u64 {
        let mut prefix = self.frontier.front().map_or(u64::MAX, |(start, _)| start);
        for claim in &self.claims {
            if !claim.retired.get() && claim.pos() < claim.end() {
                prefix = prefix.min(claim.pos());
            }
        }
        prefix.min(self.total.unwrap_or(u64::MAX))
    }
}

struct Shared<'a> {
    sink: RefCell<&'a mut SinkFn<'a>>,
    cancelled: &'a CancelFn<'a>,
    sink_error: RefCell<Option<io::Error>>,
    /// Time spent inside the sink, for backpressure.
    sink_nanos: Cell<u64>,
    /// Bytes handed to the sink.
    written: Cell<u64>,
    ttfbs: RefCell<VecDeque<Duration>>,
    state: RefCell<State>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Role {
    First { plain: bool },
    Range,
}

struct LaneHandler<'a> {
    shared: Rc<Shared<'a>>,
    claim: Rc<Claim>,
    role: Role,
    headers: ResponseHeaders,
    /// Set at the first write: later header lines (trailers) are ignored.
    frozen: bool,
    verdict: Option<Verdict>,
    etag: Option<String>,
    total: Option<u64>,
}

impl Handler for LaneHandler<'_> {
    fn header(&mut self, data: &[u8]) -> bool {
        if !self.frozen {
            self.headers.ingest(data);
        }
        true
    }

    fn progress(&mut self, _: f64, _: f64, _: f64, _: f64) -> bool {
        !(self.shared.cancelled)()
    }

    fn write(&mut self, data: &[u8]) -> Result<usize, WriteError> {
        let started = Instant::now();
        let written = self.write_clipped(data);
        let shared = &self.shared;
        shared
            .sink_nanos
            .set(shared.sink_nanos.get() + started.elapsed().as_nanos() as u64);
        Ok(written)
    }
}

impl LaneHandler<'_> {
    /// Writes as much of `data` as the claim still owns and returns how much
    /// that was. A count below `data.len()` stops the transfer.
    fn write_clipped(&mut self, data: &[u8]) -> usize {
        if !self.frozen {
            self.frozen = true;
            if let Err(verdict) = self.accept() {
                self.verdict = Some(verdict);
                return 0;
            }
        }
        if self.verdict.is_some() || self.claim.retired.get() {
            return 0;
        }
        let mut consumed = 0;
        while consumed < data.len() {
            let position = self.claim.pos();
            let end = self.claim.end();
            if position >= end {
                if self.extend() {
                    continue;
                }
                // Data is left but the claim ends here: another lane owns the
                // rest.
                self.claim.clipped.set(true);
                return consumed;
            }
            let allowed_end = {
                let state = self.shared.state.borrow();
                if state.phase == Phase::Ranged && position > state.prefix() {
                    end.min(state.prefix().saturating_add(state.max_ahead).max(position))
                } else {
                    end
                }
            };
            if allowed_end < end {
                // The link slowed since this claim was issued. Return the
                // unreceived suffix without counting a transport retry.
                self.shared
                    .state
                    .borrow_mut()
                    .frontier
                    .insert(allowed_end, end);
                self.claim.set_end(allowed_end);
            }
            let take = (data.len() - consumed).min((allowed_end - position) as usize);
            if take == 0 {
                self.claim.clipped.set(true);
                return consumed;
            }
            let piece = &data[consumed..consumed + take];
            let result = {
                let mut sink = self.shared.sink.borrow_mut();
                (*sink)(Chunk {
                    offset: position,
                    bytes: piece,
                    strong_etag: self.etag.as_deref(),
                    total_bytes: self.total,
                })
            };
            if let Err(error) = result {
                *self.shared.sink_error.borrow_mut() = Some(error);
                return consumed;
            }
            let now = Instant::now();
            if self.claim.first_byte.get().is_none() {
                self.claim.first_byte.set(Some(now));
                let mut ttfbs = self.shared.ttfbs.borrow_mut();
                ttfbs.push_back(now.duration_since(self.claim.started));
                if ttfbs.len() > 16 {
                    ttfbs.pop_front();
                }
            }
            self.claim.last_byte.set(now);
            self.claim
                .live
                .received
                .fetch_add(take as u64, Ordering::Relaxed);
            self.shared
                .written
                .set(self.shared.written.get() + take as u64);
            consumed += take;
        }
        consumed
    }

    /// Classifies the response at its first write and, for the first
    /// request, fixes the claim and the frontier.
    fn accept(&mut self) -> Result<(), Verdict> {
        match self.role {
            Role::Range => {
                let (etag, total) = {
                    let state = self.shared.state.borrow();
                    (
                        state.etag.clone().unwrap_or_default(),
                        state.total.unwrap_or(0),
                    )
                };
                check_range_response(
                    &self.headers,
                    self.claim.start(),
                    self.claim.cover_end.get(),
                    &etag,
                    total,
                )?;
                self.etag = Some(etag);
                self.total = Some(total);
                Ok(())
            }
            Role::First { plain } => {
                let mut state = self.shared.state.borrow_mut();
                let resume = state.resume.clone();
                let first = classify_first(&self.headers, state.offset, resume.as_ref(), plain);
                let offset = state.offset;
                match first {
                    First::Sequential {
                        etag,
                        total,
                        expected_len,
                    } => {
                        state.phase = Phase::Sequential { expected_len };
                        state.etag = etag.clone();
                        state.total = total;
                        let end = expected_len.map_or(u64::MAX, |len| offset + len);
                        self.claim.set_end(end);
                        self.claim.cover_end.set(end);
                        self.etag = etag;
                        self.total = total;
                        Ok(())
                    }
                    First::Ranged {
                        etag,
                        total,
                        covers_end,
                    } => {
                        let remaining = total.saturating_sub(offset);
                        let claim_end = if remaining < state.min_adaptive {
                            covers_end
                        } else {
                            // A few minimum ranges until a rate is known, however
                            // large the file: a size-proportional first range
                            // would break the loss bound on a slow link. Lane 0
                            // keeps streaming through `extend`.
                            let first = state
                                .min_segment
                                .max(total / 16)
                                .min(state.min_segment.saturating_mul(2));
                            (offset + first).min(covers_end)
                        };
                        state.phase = Phase::Ranged;
                        state.etag = Some(etag.clone());
                        state.total = Some(total);
                        state.first_claim = claim_end - offset;
                        state.claim_bytes = state.first_claim.max(state.min_segment);
                        state.largest_claim = state.largest_claim.max(state.first_claim);
                        state.frontier = Frontier::new(claim_end, total);
                        self.claim.set_end(claim_end);
                        self.claim.cover_end.set(covers_end);
                        self.etag = Some(etag);
                        self.total = Some(total);
                        Ok(())
                    }
                    First::IdentityRestart(reason) => Err(Verdict::Identity(reason)),
                    First::PlainGet if plain => Err(Verdict::Fail(
                        "the source answers neither ranges nor a plain request".into(),
                    )),
                    First::PlainGet => Err(Verdict::PlainGet),
                    First::Fail(reason) => Err(Verdict::Fail(reason)),
                }
            }
        }
    }

    /// The claim reached its end and the request can deliver more: take the
    /// bytes that follow if nobody has claimed them.
    fn extend(&mut self) -> bool {
        let mut state = self.shared.state.borrow_mut();
        if state.phase != Phase::Ranged {
            return false;
        }
        let start = self.claim.end();
        let cover_end = self.claim.cover_end.get();
        if start >= cover_end {
            return false;
        }
        let limit = state.prefix().saturating_add(state.max_ahead);
        let contiguous = self.claim.pos() == state.prefix();
        if !contiguous && start >= limit {
            return false;
        }
        // This lane's own pace sizes its next stretch, within the growth cap.
        let own = self
            .claim
            .rate(Instant::now())
            .map_or(0, |rate| (rate * crate::SEGMENT_SECONDS) as u64);
        let growth = state
            .largest_claim
            .saturating_mul(CLAIM_GROWTH)
            .max(state.min_segment);
        let length = state
            .claim_bytes
            .max(own)
            .min(growth)
            .max(state.min_segment);
        let end = (start + length).min(cover_end);
        let end = if contiguous { end } else { end.min(limit) };
        match state.frontier.take_at(start, end) {
            Some(end) => {
                self.claim.set_end(end);
                state.largest_claim = state.largest_claim.max(end - start);
                true
            }
            None => false,
        }
    }
}

// ---------------------------------------------------------------------------
// The event loop

struct Lane<'a> {
    id: usize,
    handle: Easy2Handle<LaneHandler<'a>>,
    claim: Rc<Claim>,
}

struct Window {
    started: Instant,
    written: u64,
    sink_nanos: u64,
    length: Duration,
}

struct Engine<'a> {
    // Handles go before the multi handle so they detach first.
    lanes: Vec<Lane<'a>>,
    permits: Vec<BudgetPermit<'a>>,
    multi: Multi,
    slot: DownloadSlot<'a>,
    shared: Rc<Shared<'a>>,
    url: &'a str,
    context: &'a RequestContext,
    decision: ProtocolDecision,
    limits: TransferLimits,
    monitor: &'a SegmentMonitor,
    controller: Controller,
    ramped: bool,
    retries: HashMap<u64, u32>,
    fresh: HashSet<u64>,
    next_lane: usize,
    pause_until: Option<Instant>,
    hold_claims_until: Option<Instant>,
    /// A first request waiting to be (re)issued, with whether it is plain.
    pending_open: Option<bool>,
    open_failures: u32,
    plain_used: bool,
    ignored_ranges: u32,
    window: Option<Window>,
    observations: Vec<BatchObservation>,
    goodput: VecDeque<(Instant, u64)>,
    /// What finished claims measured for one lane, smoothed. A claim on a
    /// fast link ends before a running lane has a rate worth trusting.
    learned_rate: Option<f64>,
    blocked_since: Option<Instant>,
    starved: bool,
    finished: bool,
    peak_width: usize,
    started: Instant,
    protocol: Protocol,
    splits: u32,
    replacements: u32,
    retried: u32,
    throttles: u32,
    block_replacements: u32,
    requests: u32,
    connections: u32,
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn run<'a>(
    url: &'a str,
    context: &'a RequestContext,
    limits: TransferLimits,
    budget: &'a GlobalBudget,
    monitor: &'a SegmentMonitor,
    resume: Option<&ResumePoint>,
    cancelled: &'a CancelFn<'a>,
    sink: &'a mut SinkFn<'a>,
) -> Result<TransferReport, TransferError> {
    let decision = decide_protocol(ProtocolCapabilities::detect());
    let slot = budget.register_download();
    let first_permit = slot.reserve(limits.receive_buffer_bytes, &|| cancelled())?;
    let offset = resume.map_or(0, |resume| resume.offset);
    let shared = Rc::new(Shared {
        sink: RefCell::new(sink),
        cancelled,
        sink_error: RefCell::new(None),
        sink_nanos: Cell::new(0),
        written: Cell::new(0),
        ttfbs: RefCell::new(VecDeque::new()),
        state: RefCell::new(State {
            phase: Phase::Opening,
            offset,
            resume: resume.cloned(),
            etag: None,
            total: None,
            frontier: Frontier::default(),
            claims: Vec::new(),
            max_ahead: 0,
            claim_bytes: limits.min_segment_bytes as u64,
            min_segment: limits.min_segment_bytes as u64,
            min_adaptive: limits.min_adaptive_bytes,
            first_claim: 0,
            largest_claim: limits.min_segment_bytes as u64,
        }),
    });
    let mut engine = Engine {
        lanes: Vec::new(),
        permits: vec![first_permit],
        multi: new_multi()?,
        slot,
        shared,
        url,
        context,
        decision,
        limits,
        monitor,
        controller: Controller::new(1, 1),
        ramped: false,
        retries: HashMap::new(),
        fresh: HashSet::new(),
        next_lane: 0,
        pause_until: None,
        hold_claims_until: None,
        pending_open: None,
        open_failures: 0,
        plain_used: false,
        ignored_ranges: 0,
        window: None,
        observations: Vec::new(),
        goodput: VecDeque::from([(Instant::now(), 0)]),
        learned_rate: None,
        blocked_since: None,
        starved: false,
        finished: false,
        peak_width: 1,
        started: Instant::now(),
        protocol: Protocol::Unknown,
        splits: 0,
        replacements: 0,
        retried: 0,
        throttles: 0,
        block_replacements: 0,
        requests: 0,
        connections: 0,
    };
    engine.drive()
}

impl<'a> Engine<'a> {
    fn cancelled(&self) -> bool {
        (self.shared.cancelled)()
    }

    /// Prints scheduler events to stderr when `FETCHPATH_HTTP_TRACE` is set.
    /// For benchmarking only.
    fn trace(&self, message: impl FnOnce() -> String) {
        static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        if *ON.get_or_init(|| std::env::var_os("FETCHPATH_HTTP_TRACE").is_some()) {
            eprintln!(
                "[{:>7.1} ms] {}",
                self.started.elapsed().as_secs_f64() * 1000.0,
                message()
            );
        }
    }

    fn drive(&mut self) -> Result<TransferReport, TransferError> {
        self.open_first(self.limits.scheduler == Scheduler::Plain)?;
        let mut last_tick = Instant::now();
        loop {
            if self.cancelled() {
                return Err(TransferError::Cancelled);
            }
            self.multi.perform().map_err(multi_error)?;
            let done = self.collect();
            let any_done = !done.is_empty();
            for (id, result) in done {
                self.conclude(id, result)?;
            }
            if let Some(error) = self.shared.sink_error.borrow_mut().take() {
                return Err(TransferError::Sink(error));
            }
            if self.finished {
                break;
            }
            let now = Instant::now();
            let ramp_due = !self.ramped && self.shared.state.borrow().phase == Phase::Ranged;
            if any_done || ramp_due || now.duration_since(last_tick) >= POLL {
                self.tick(now)?;
                last_tick = now;
            }
            if self.finished {
                break;
            }
            let waited = Instant::now();
            self.multi.wait(&mut [], POLL).map_err(multi_error)?;
            // With no socket to watch, `wait` can return at once.
            if let Some(rest) = POLL.checked_sub(waited.elapsed())
                && self.lanes.is_empty()
            {
                std::thread::sleep(rest);
            }
        }
        self.report()
    }

    fn collect(&mut self) -> Vec<(usize, Result<(), curl::Error>)> {
        let mut done = Vec::new();
        self.multi.messages(|message| {
            if let (Some(result), Ok(token)) = (message.result(), message.token()) {
                done.push((token, result));
            }
        });
        done
    }

    // -- starting requests --------------------------------------------------

    fn open_first(&mut self, plain: bool) -> Result<(), TransferError> {
        let (offset, etag) = {
            let state = self.shared.state.borrow();
            (
                state.offset,
                state
                    .resume
                    .as_ref()
                    .map(|resume| resume.strong_etag.clone()),
            )
        };
        let live = self.monitor.add(offset, u64::MAX);
        let claim = Claim::new(live, u64::MAX, 0);
        let range = (!plain).then(|| format!("{offset}-"));
        self.start_request(
            claim,
            Role::First { plain },
            range,
            if plain { None } else { etag },
            false,
        )
    }

    fn start_range(&mut self, start: u64, end: u64, attempts: u32) -> Result<(), TransferError> {
        let fresh = self.fresh.remove(&start);
        let etag = self.shared.state.borrow().etag.clone();
        let live = self.monitor.add(start, end);
        let claim = Claim::new(live, end, attempts);
        self.start_request(
            claim,
            Role::Range,
            Some(format!("{start}-{}", end - 1)),
            etag,
            fresh,
        )
    }

    fn start_request(
        &mut self,
        claim: Rc<Claim>,
        role: Role,
        range: Option<String>,
        if_range: Option<String>,
        fresh: bool,
    ) -> Result<(), TransferError> {
        let handler = LaneHandler {
            shared: Rc::clone(&self.shared),
            claim: Rc::clone(&claim),
            role,
            headers: ResponseHeaders::default(),
            frozen: false,
            verdict: None,
            etag: None,
            total: None,
        };
        let mut easy = Easy2::new(handler);
        easy.url(self.url).map_err(curl_error)?;
        http_only(easy.raw())?;
        easy.follow_location(true).map_err(curl_error)?;
        easy.fail_on_error(true).map_err(curl_error)?;
        easy.ssl_verify_peer(true).map_err(curl_error)?;
        easy.ssl_verify_host(true).map_err(curl_error)?;
        easy.buffer_size(self.limits.receive_buffer_bytes)
            .map_err(curl_error)?;
        easy.progress(true).map_err(curl_error)?;
        easy.http_version(http_version_for(self.url, self.context, &self.decision))
            .map_err(curl_error)?;
        for cookie in &self.context.cookie_lines {
            easy.cookie_list(cookie).map_err(curl_error)?;
        }
        if let Some(referer) = &self.context.referer {
            easy.referer(referer).map_err(curl_error)?;
        }
        if let Some(range) = &range {
            easy.range(range).map_err(curl_error)?;
        }
        easy.http_headers(request_headers(if_range.as_deref())?)
            .map_err(curl_error)?;
        if fresh {
            easy.fresh_connect(true).map_err(curl_error)?;
        }
        let id = self.next_lane;
        self.next_lane += 1;
        self.trace(|| {
            format!(
                "lane {id} starts {} fresh={fresh}",
                range.as_deref().unwrap_or("(plain)")
            )
        });
        let mut handle = self.multi.add2(easy).map_err(multi_error)?;
        handle.set_token(id).map_err(curl_error)?;
        self.shared
            .state
            .borrow_mut()
            .claims
            .push(Rc::clone(&claim));
        self.lanes.push(Lane { id, handle, claim });
        self.requests += 1;
        Ok(())
    }

    // -- finishing requests -------------------------------------------------

    /// Detaches a lane's handle, which stops its transfer, and returns what
    /// it learned.
    fn detach(
        &mut self,
        index: usize,
    ) -> Result<(Rc<Claim>, Easy2<LaneHandler<'a>>), TransferError> {
        let Lane { handle, claim, .. } = self.lanes.remove(index);
        let easy = self.multi.remove2(handle).map_err(multi_error)?;
        self.connections += easy.num_connects().unwrap_or(0) as u32;
        if let Some(first) = claim.first_byte.get() {
            let elapsed = claim.last_byte.get().duration_since(first).as_secs_f64();
            if claim.received() >= 256 * 1024 && elapsed >= 0.001 {
                let rate = claim.received() as f64 / elapsed;
                self.learned_rate = Some(self.learned_rate.map_or(rate, |old| (old + rate) / 2.0));
            }
        }
        self.monitor.remove(&claim.live);
        self.shared
            .state
            .borrow_mut()
            .claims
            .retain(|other| !Rc::ptr_eq(other, &claim));
        if matches!(easy.get_ref().role, Role::First { .. }) {
            let protocol = easy.get_ref().headers.protocol;
            if protocol != Protocol::Unknown {
                self.protocol = protocol;
            }
        }
        Ok((claim, easy))
    }

    fn conclude(
        &mut self,
        id: usize,
        result: Result<(), curl::Error>,
    ) -> Result<(), TransferError> {
        let Some(index) = self.lanes.iter().position(|lane| lane.id == id) else {
            return Ok(());
        };
        let (claim, mut easy) = self.detach(index)?;
        self.trace(|| {
            format!(
                "lane {id} ends at {} of {}..{} result={:?}",
                claim.pos(),
                claim.start(),
                claim.end(),
                result.as_ref().map_err(|error| error.to_string())
            )
        });
        if self.cancelled() {
            return Err(TransferError::Cancelled);
        }
        if let Some(error) = self.shared.sink_error.borrow_mut().take() {
            return Err(TransferError::Sink(error));
        }
        let status = easy
            .get_ref()
            .headers
            .status
            .or_else(|| easy.response_code().ok().filter(|code| *code > 0));
        let (verdict, frozen, role, retry_after, headers) = {
            let handler = easy.get_mut();
            (
                handler.verdict.take(),
                handler.frozen,
                handler.role,
                handler.headers.retry_after,
                handler.headers.clone(),
            )
        };
        if let Some(verdict) = verdict {
            return self.on_verdict(verdict, role, &claim);
        }
        match result {
            Ok(()) => self.on_complete(role, frozen, &claim, &headers),
            Err(error) => self.on_error(role, frozen, &claim, &error, status, retry_after),
        }
    }

    fn on_verdict(
        &mut self,
        verdict: Verdict,
        role: Role,
        claim: &Rc<Claim>,
    ) -> Result<(), TransferError> {
        match verdict {
            Verdict::Identity(reason) => Err(TransferError::IdentityChanged(reason)),
            Verdict::PlainGet => {
                self.plain_used = true;
                self.pending_open = Some(true);
                Ok(())
            }
            Verdict::IgnoredRange => {
                self.ignored_ranges += 1;
                if self.ignored_ranges >= IGNORED_RANGE_LIMIT {
                    // Finish on one lane at a time from here.
                    self.controller.cap(1);
                }
                self.requeue(claim, "a node ignored the range", false)
            }
            Verdict::Fail(reason) => match role {
                Role::First { .. } => Err(TransferError::Rejected(reason)),
                Role::Range => self.requeue(claim, &reason, false),
            },
        }
    }

    fn on_complete(
        &mut self,
        role: Role,
        frozen: bool,
        claim: &Rc<Claim>,
        headers: &ResponseHeaders,
    ) -> Result<(), TransferError> {
        if let Role::First { plain } = role {
            if !frozen {
                // A response with no body never calls the write callback, so
                // the headers are classified here.
                let (offset, resume) = {
                    let state = self.shared.state.borrow();
                    (state.offset, state.resume.clone())
                };
                return match classify_first(headers, offset, resume.as_ref(), plain) {
                    First::Sequential {
                        total,
                        expected_len: None | Some(0),
                        etag,
                    } => {
                        let mut state = self.shared.state.borrow_mut();
                        state.phase = Phase::Sequential {
                            expected_len: Some(0),
                        };
                        state.etag = etag;
                        state.total = total;
                        self.finished = true;
                        Ok(())
                    }
                    First::IdentityRestart(reason) => Err(TransferError::IdentityChanged(reason)),
                    First::PlainGet if !plain => {
                        self.plain_used = true;
                        self.pending_open = Some(true);
                        Ok(())
                    }
                    _ => Err(TransferError::Transport(
                        "the response ended before its body".into(),
                    )),
                };
            }
            let phase = self.shared.state.borrow().phase;
            if let Phase::Sequential { expected_len } = phase {
                if let Some(expected) = expected_len
                    && claim.received() != expected
                {
                    return Err(TransferError::Transport(format!(
                        "response declared {expected} bytes but delivered {}",
                        claim.received()
                    )));
                }
                self.finished = true;
                return Ok(());
            }
        }
        if claim.remaining() > 0 {
            return self.requeue(claim, "the response ended short", false);
        }
        Ok(())
    }

    fn on_error(
        &mut self,
        role: Role,
        frozen: bool,
        claim: &Rc<Claim>,
        error: &curl::Error,
        status: Option<u32>,
        retry_after: Option<Duration>,
    ) -> Result<(), TransferError> {
        if error.is_write_error() && (claim.clipped.get() || claim.retired.get()) {
            // A deliberate stop at the claim's end, not a failure.
            return Ok(());
        }
        let now = Instant::now();
        let wait = retry_after.unwrap_or(DEFAULT_RETRY_AFTER);
        let throttled = matches!(status, Some(429 | 503));
        if throttled {
            self.throttles += 1;
            self.pause_until = Some(retry_deadline(now, wait)?);
            if self.ramped {
                self.controller.throttled();
                self.slot.set_growing(false);
                self.window = None;
            }
        }
        let phase = self.shared.state.borrow().phase;
        let what = match status.filter(|status| *status >= 400) {
            Some(status) => format!("HTTP status {status}"),
            None => error.to_string(),
        };
        match (role, phase) {
            (Role::First { plain }, Phase::Opening) if !frozen => {
                let offset = self.shared.state.borrow().offset;
                match status {
                    Some(416) if !plain => {
                        if offset > 0 {
                            return Err(TransferError::IdentityChanged(
                                "the resume offset is past the end".into(),
                            ));
                        }
                        self.plain_used = true;
                        self.pending_open = Some(true);
                        return Ok(());
                    }
                    Some(status) if status >= 400 && !throttled => {
                        return Err(TransferError::Rejected(format!("HTTP status {status}")));
                    }
                    _ => {}
                }
                if error.is_couldnt_connect()
                    || error.is_couldnt_resolve_host()
                    || error.is_couldnt_resolve_proxy()
                    || error.is_ssl_connect_error()
                    || error.is_peer_failed_verification()
                {
                    // Nothing answered. Retrying would only repeat the wait.
                    return Err(TransferError::Rejected(what));
                }
                self.open_failures += 1;
                if self.open_failures > RETRY_BUDGET {
                    return Err(TransferError::Transport(what));
                }
                self.retried += 1;
                self.pending_open = Some(plain);
                Ok(())
            }
            (_, Phase::Sequential { .. }) => Err(TransferError::Transport(what)),
            (_, Phase::Opening) => Err(TransferError::Transport(what)),
            (_, Phase::Ranged) => {
                if status == Some(416) {
                    return Err(TransferError::IdentityChanged(
                        "a range is past the end of the source".into(),
                    ));
                }
                self.requeue(claim, &what, false)
            }
        }
    }

    /// Puts what a claim did not receive back at the front of the frontier.
    /// A range that keeps failing without progress ends the attempt.
    fn requeue(&mut self, claim: &Rc<Claim>, why: &str, fresh: bool) -> Result<(), TransferError> {
        let (position, end) = (claim.pos(), claim.end());
        if position >= end {
            return Ok(());
        }
        // A lane that was replaced (stalled, crawling, parked behind) counts
        // against the range only when it delivered nothing; a failure needs
        // 64 KiB of progress to be forgiven.
        let attempts = retry_attempts(claim.attempts, claim.received(), fresh);
        if attempts > RETRY_BUDGET {
            return Err(TransferError::Transport(format!(
                "the range at byte {position} failed {attempts} times: {why}"
            )));
        }
        self.retries.insert(position, attempts);
        if fresh {
            self.fresh.insert(position);
        }
        self.shared
            .state
            .borrow_mut()
            .frontier
            .insert(position, end);
        self.retried += 1;
        Ok(())
    }

    // -- the scheduling tick ------------------------------------------------

    fn tick(&mut self, now: Instant) -> Result<(), TransferError> {
        if let Some(plain) = self.pending_open
            && self.lanes.is_empty()
            && self.pause_until.is_none_or(|until| now >= until)
        {
            self.pending_open = None;
            self.open_first(plain)?;
        }
        let phase = self.shared.state.borrow().phase;
        if phase != Phase::Ranged {
            return self.check_open_stall(now);
        }
        if !self.ramped {
            self.ramp();
        }
        // A callback may end exactly at a shrunken claim boundary. Release
        // every completed owner without waiting for another body callback.
        while let Some(index) = self
            .lanes
            .iter()
            .position(|lane| lane.claim.remaining() == 0)
        {
            let (_, easy) = self.detach(index)?;
            drop(easy);
        }
        self.sample_rates(now);
        self.detect_stalls(now)?;
        self.resize_lanes();
        self.assign(now)?;
        self.window_step(now);
        let state = self.shared.state.borrow();
        if state.frontier.is_empty() && self.lanes.is_empty() && self.pending_open.is_none() {
            drop(state);
            self.finished = true;
        }
        Ok(())
    }

    /// Before the file is known to support ranges, and on a one-request
    /// download, a stall is only a long silence.
    fn check_open_stall(&mut self, now: Instant) -> Result<(), TransferError> {
        for lane in &self.lanes {
            let silent = now.duration_since(lane.claim.last_byte.get().max(lane.claim.started));
            if silent >= self.limits.open_timeout {
                return Err(TransferError::Transport(format!(
                    "no data for {} s",
                    silent.as_secs()
                )));
            }
        }
        Ok(())
    }

    fn ramp(&mut self) {
        let remaining = {
            let state = self.shared.state.borrow();
            state.total.unwrap_or(0).saturating_sub(state.offset)
        };
        let maximum = if remaining >= self.limits.min_adaptive_bytes {
            self.limits
                .max_concurrency
                .min(self.limits.max_active_requests)
        } else {
            1
        };
        self.controller = Controller::new(2, maximum);
        self.peak_width = self.controller.width();
        self.slot.set_growing(self.controller.growing());
        self.ramped = true;
    }

    fn sample_rates(&mut self, now: Instant) {
        let written = self.shared.written.get();
        self.goodput.push_back((now, written));
        while self
            .goodput
            .front()
            .is_some_and(|(then, _)| now.duration_since(*then) > Duration::from_millis(1200))
        {
            self.goodput.pop_front();
        }
        let goodput = self.goodput.front().and_then(|(then, before)| {
            let span = now.duration_since(*then).as_secs_f64();
            (span >= 0.01).then(|| (written - before) as f64 / span)
        });
        // One lane's pace: the median of what running lanes measure for
        // themselves, or an even share of the aggregate. Dividing the
        // aggregate by however many lanes happen to be running at this
        // instant would swing with every claim that ends.
        let mut own: Vec<f64> = self
            .lanes
            .iter()
            .filter(|lane| lane.claim.received() >= 128 * 1024)
            // A lane that has stopped delivering says nothing about the link.
            .filter(|lane| {
                now.duration_since(lane.claim.last_byte.get()) < Duration::from_millis(500)
            })
            .filter_map(|lane| lane.claim.rate(now))
            .collect();
        own.sort_by(|a, b| a.total_cmp(b));
        let held = self.controller.width().min(self.permits.len()).max(1);
        let lane_rate = match (own.get(own.len() / 2).copied(), self.learned_rate) {
            (Some(running), Some(learned)) => Some(running.max(learned)),
            (running, learned) => running.or(learned),
        }
        .or_else(|| goodput.map(|rate| rate / held as f64));
        let mut state = self.shared.state.borrow_mut();
        let floor = state.min_segment;
        // Until a rate is known, a claim is as long as the first one.
        let start = state.first_claim.max(floor);
        state.claim_bytes = lane_rate.map_or(start, |rate| {
            ((rate * crate::SEGMENT_SECONDS) as u64).max(floor)
        });
        // The lanes actually held, not the width the controller wants.
        state.max_ahead = max_ahead_for(goodput);
        for lane in &self.lanes {
            lane.claim.sample(now);
        }
    }

    /// Replaces a lane that sends nothing, or that holds the prefix while
    /// crawling next to healthy lanes.
    fn detect_stalls(&mut self, now: Instant) -> Result<(), TransferError> {
        let prefix = self.shared.state.borrow().prefix();
        let mut rates: Vec<f64> = self
            .lanes
            .iter()
            .filter_map(|lane| lane.claim.recent_rate(now))
            .collect();
        rates.sort_by(|a, b| a.total_cmp(b));
        let median = rates.get(rates.len() / 2).copied();
        // Before a lane's first byte its wait includes the time to first
        // byte, so it gets a few times what earlier lanes measured.
        let ttfb = {
            let mut sorted: Vec<Duration> = self.shared.ttfbs.borrow().iter().copied().collect();
            sorted.sort();
            sorted.get(sorted.len() / 2).copied()
        };
        let waiting_allowance = ttfb.map_or(self.limits.stall_timeout, |ttfb| {
            self.limits.stall_timeout.max(ttfb * TTFB_ALLOWANCE)
        });
        let mut stalled = None;
        for (index, lane) in self.lanes.iter().enumerate() {
            let claim = &lane.claim;
            if claim.remaining() == 0 {
                continue;
            }
            let silent = now.duration_since(claim.last_byte.get().max(claim.started));
            let allowance = if claim.first_byte.get().is_none() {
                waiting_allowance
            } else {
                self.limits.stall_timeout
            };
            let crawling = self.lanes.len() >= 2
                && claim.pos() == prefix
                && claim
                    .recent_rate(now)
                    .zip(median)
                    .is_some_and(|(rate, median)| rate < STALL_RATE_SHARE * median);
            if silent >= allowance || crawling {
                stalled = Some(index);
                break;
            }
        }
        if let Some(index) = stalled {
            self.replace(index, "the lane stopped delivering")?;
        }
        Ok(())
    }

    /// Stops a lane, drops its connection and returns its unreceived bytes
    /// to the front of the frontier, to be fetched on a fresh connection.
    fn replace(&mut self, index: usize, why: &str) -> Result<(), TransferError> {
        let (claim, easy) = self.detach(index)?;
        self.trace(|| {
            format!(
                "replace {}..{} at {}: {why}",
                claim.start(),
                claim.end(),
                claim.pos()
            )
        });
        claim.retired.set(true);
        drop(easy);
        self.replacements += 1;
        self.window = None;
        self.requeue(&claim, why, true)
    }

    fn resize_lanes(&mut self) {
        let width = self.controller.width();
        while self.permits.len() < width {
            match self.slot.try_reserve(self.limits.receive_buffer_bytes) {
                Some(permit) => self.permits.push(permit),
                // The budget or this download's fair share is used up. Ask
                // again on the next tick: other downloads finish, and the
                // share grows.
                None => break,
            }
        }
        while self.permits.len() > width.max(self.lanes.len()).max(1) {
            self.permits.pop();
        }
    }

    /// Gives every idle lane work: a claim from the frontier, or half of
    /// the slowest claim in flight.
    fn assign(&mut self, now: Instant) -> Result<(), TransferError> {
        self.starved = false;
        let capacity = self.permits.len().min(self.controller.width());
        if self.lanes.len() >= capacity {
            return Ok(());
        }
        let held = self.hold_claims_until.is_some_and(|until| now < until);
        let paused = self.pause_until.is_some_and(|until| now < until);
        if held || paused {
            self.starved = true;
            return Ok(());
        }
        while self.lanes.len() < capacity {
            if !self.fill_one(now, capacity)? {
                self.starved = true;
                break;
            }
        }
        Ok(())
    }

    fn fill_one(&mut self, now: Instant, capacity: usize) -> Result<bool, TransferError> {
        let plan = self.shared.state.borrow_mut().plan_claim(capacity);
        match plan {
            Plan::Claim(start, end) => {
                self.blocked_since = None;
                let attempts = self.retries.remove(&start).unwrap_or(0);
                self.start_range(start, end, attempts)?;
                Ok(true)
            }
            Plan::Blocked => {
                let since = *self.blocked_since.get_or_insert(now);
                if now.duration_since(since) >= BLOCK_GRACE {
                    // Every lane is parked behind the one that holds the prefix.
                    self.blocked_since = None;
                    if let Some(index) = self.prefix_lane()
                        && self.slower_than_median(index, now)
                    {
                        self.block_replacements += 1;
                        self.replace(index, "the prefix lane holds back every other lane")?;
                        return Ok(true);
                    }
                }
                Ok(false)
            }
            Plan::Empty => {
                self.blocked_since = None;
                self.steal(now)
            }
        }
    }

    /// A lane that only looks slow next to healthy ones is not replaced for
    /// parking them; it must really be slower than the median lane.
    fn slower_than_median(&self, index: usize, now: Instant) -> bool {
        let mut rates: Vec<f64> = self
            .lanes
            .iter()
            .filter_map(|lane| lane.claim.rate(now))
            .collect();
        rates.sort_by(|a, b| a.total_cmp(b));
        let Some(mine) = self.lanes[index].claim.rate(now) else {
            return false;
        };
        let median = rates
            .get(rates.len() / 2)
            .copied()
            .into_iter()
            .chain(self.learned_rate)
            .max_by(|a, b| a.total_cmp(b));
        median.is_some_and(|median| mine < median)
    }

    fn prefix_lane(&self) -> Option<usize> {
        let prefix = self.shared.state.borrow().prefix();
        self.lanes
            .iter()
            .position(|lane| lane.claim.pos() == prefix && lane.claim.remaining() > 0)
    }

    /// Splits the claim with the most time left and starts the second half
    /// on the idle lane.
    fn steal(&mut self, now: Instant) -> Result<bool, TransferError> {
        let (min_segment, ttfb) = {
            let state = self.shared.state.borrow();
            let ttfbs = self.shared.ttfbs.borrow();
            let mut sorted: Vec<Duration> = ttfbs.iter().copied().collect();
            sorted.sort();
            (
                state.min_segment,
                sorted
                    .get(sorted.len() / 2)
                    .copied()
                    .unwrap_or(DEFAULT_TTFB),
            )
        };
        let aggregate = self.goodput.front().and_then(|(then, before)| {
            let span = now.duration_since(*then).as_secs_f64();
            (span >= 0.1).then(|| {
                (self.shared.written.get() - before) as f64 / span / self.lanes.len().max(1) as f64
            })
        });
        let horizon = 2.0 * crate::SEGMENT_SECONDS;
        let mut best: Option<(usize, f64, f64)> = None;
        for (index, lane) in self.lanes.iter().enumerate() {
            let claim = &lane.claim;
            let remaining = claim.remaining();
            let Some(rate) = claim.rate(now).or(aggregate).filter(|rate| *rate > 0.0) else {
                continue;
            };
            let time_left = remaining as f64 / rate;
            if time_left > horizon
                && remaining >= 2 * min_segment
                && best.is_none_or(|(_, _, best_left)| time_left > best_left)
            {
                best = Some((index, rate, time_left));
            }
        }
        let Some((index, rate, _)) = best else {
            return Ok(false);
        };
        let claim = Rc::clone(&self.lanes[index].claim);
        let (position, end) = (claim.pos(), claim.end());
        let remaining = (end - position) as f64;
        // The stealer's time to first byte comes out of its share.
        let mut split = position + ((remaining - rate * ttfb.as_secs_f64()).max(0.0) / 2.0) as u64;
        if end.saturating_sub(split) < min_segment {
            split = end - min_segment;
        }
        if split <= position {
            return Ok(false);
        }
        self.trace(|| format!("steal {split}..{end} from {position}..{end} rate={rate:.0}"));
        claim.set_end(split);
        self.splits += 1;
        self.window = None;
        self.start_range(split, end, 0)?;
        Ok(true)
    }

    fn window_step(&mut self, now: Instant) {
        let width = self.controller.width();
        let all_started = !self.lanes.is_empty()
            && self
                .lanes
                .iter()
                .all(|lane| lane.claim.first_byte.get().is_some());
        let full = self.lanes.len() >= width;
        let Some(window) = &self.window else {
            if all_started && full && !self.starved {
                let ttfbs = self.shared.ttfbs.borrow();
                let mut sorted: Vec<Duration> = ttfbs.iter().copied().collect();
                sorted.sort();
                let ttfb = sorted
                    .get(sorted.len() / 2)
                    .copied()
                    .unwrap_or(DEFAULT_TTFB);
                drop(ttfbs);
                self.window = Some(Window {
                    started: now,
                    written: self.shared.written.get(),
                    sink_nanos: self.shared.sink_nanos.get(),
                    length: (ttfb * 4).max(Duration::from_secs(1)),
                });
            }
            return;
        };
        // A window in which a lane sat idle for lack of work, or the width
        // moved, says nothing about the link. A claim that ends and the next
        // one that starts is ordinary turnover and stays in the window; a
        // lane that has been silent for longer is not.
        let silent = self.lanes.iter().any(|lane| {
            now.duration_since(lane.claim.last_byte.get().max(lane.claim.started))
                > Duration::from_millis(500)
        });
        if self.starved || !full || silent {
            self.window = None;
            return;
        }
        let elapsed = now.duration_since(window.started);
        if elapsed < window.length {
            return;
        }
        let seconds = elapsed.as_secs_f64();
        let bytes = self.shared.written.get() - window.written;
        let goodput = bytes as f64 / seconds;
        let busy = (self.shared.sink_nanos.get() - window.sink_nanos) as f64 / 1e9;
        let backpressure = busy / seconds > BACKPRESSURE_SHARE;
        self.observations.push(BatchObservation {
            concurrency: width,
            bytes,
            elapsed_ms: seconds * 1000.0,
        });
        self.window = None;
        let outcome = self.controller.window(goodput, backpressure);
        self.trace(|| {
            format!(
                "window width={width} goodput={:.1} MiB/s busy={:.2} -> {:?}",
                goodput / 1048576.0,
                busy / seconds,
                outcome
            )
        });
        if outcome.hold_claims {
            self.hold_claims_until = Some(now + Duration::from_secs(1));
        }
        if outcome.change.is_some() {
            if let Some(Change::Grew(_)) = outcome.change {
                self.peak_width = self.peak_width.max(self.controller.width());
            }
            self.slot.set_growing(self.controller.growing());
        }
    }

    fn report(&mut self) -> Result<TransferReport, TransferError> {
        let state = self.shared.state.borrow();
        let written = self.shared.written.get();
        let ranged = state.phase == Phase::Ranged;
        if ranged
            && let Some(total) = state.total
            && state.offset + written != total
        {
            return Err(TransferError::Transport(format!(
                "the ranges delivered {} of {total} bytes",
                state.offset + written
            )));
        }
        let mut fallback_reasons = self.decision.fallback_reasons.clone();
        if self.decision.preferred != self.protocol {
            fallback_reasons.push("negotiated_lower_protocol");
        }
        Ok(TransferReport {
            bytes: state.offset + written,
            total_bytes: state.total,
            strong_etag: state.etag.clone(),
            preferred_protocol: self.decision.preferred,
            negotiated_protocol: self.protocol,
            fallback_reasons,
            used_ranges: ranged,
            adaptive: self.peak_width > 1,
            peak_concurrency: if ranged { self.peak_width } else { 1 },
            observations: std::mem::take(&mut self.observations),
            budget: self.slot.budget.snapshot(),
            splits: self.splits,
            replacements: self.replacements,
            retries: self.retried,
            throttles: self.throttles,
            block_replacements: self.block_replacements,
            final_concurrency: if self.ramped {
                self.controller.width()
            } else {
                1
            },
            requests: self.requests,
            connections_opened: self.connections,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIB: u64 = 1024 * 1024;

    fn headers(lines: &[&str]) -> ResponseHeaders {
        let mut headers = ResponseHeaders::default();
        for line in lines {
            headers.ingest(line.as_bytes());
        }
        headers
    }

    fn resume(offset: u64, etag: &str, total: Option<u64>) -> ResumePoint {
        ResumePoint {
            offset,
            strong_etag: etag.into(),
            expected_total: total,
        }
    }

    // -- first-response classification: one test per row of spec 4.1 --------

    #[test]
    fn row_200_at_zero_streams_sequentially() {
        let response = headers(&["HTTP/1.1 200 OK", "ETag: \"a\"", "Content-Length: 10"]);
        assert_eq!(
            classify_first(&response, 0, None, false),
            First::Sequential {
                etag: Some("\"a\"".into()),
                total: Some(10),
                expected_len: Some(10),
            }
        );
    }

    #[test]
    fn row_200_on_a_resume_is_an_identity_restart() {
        let response = headers(&["HTTP/1.1 200 OK", "ETag: \"a\"", "Content-Length: 10"]);
        let point = resume(5, "\"a\"", Some(10));
        assert!(matches!(
            classify_first(&response, 5, Some(&point), false),
            First::IdentityRestart(_)
        ));
    }

    #[test]
    fn row_206_with_a_strong_etag_and_known_total_is_accepted() {
        let response = headers(&[
            "HTTP/1.1 206 Partial Content",
            "ETag: \"a\"",
            "Content-Range: bytes 0-99/100",
        ]);
        assert_eq!(
            classify_first(&response, 0, None, false),
            First::Ranged {
                etag: "\"a\"".into(),
                total: 100,
                covers_end: 100,
            }
        );
    }

    #[test]
    fn row_206_capped_below_the_total_is_accepted_for_what_it_covers() {
        let response = headers(&[
            "HTTP/1.1 206 Partial Content",
            "ETag: \"a\"",
            "Content-Range: bytes 0-49/100",
        ]);
        assert_eq!(
            classify_first(&response, 0, None, false),
            First::Ranged {
                etag: "\"a\"".into(),
                total: 100,
                covers_end: 50,
            }
        );
    }

    #[test]
    fn row_206_capped_without_a_strong_etag_falls_back_to_a_plain_get() {
        for etag in ["ETag: W/\"a\"", "X-None: 1"] {
            let response = headers(&[
                "HTTP/1.1 206 Partial Content",
                etag,
                "Content-Range: bytes 0-49/100",
            ]);
            assert_eq!(classify_first(&response, 0, None, false), First::PlainGet);
        }
    }

    #[test]
    fn row_206_with_an_unknown_total_is_sequential() {
        let response = headers(&[
            "HTTP/1.1 206 Partial Content",
            "ETag: \"a\"",
            "Content-Range: bytes 0-99/*",
        ]);
        assert_eq!(
            classify_first(&response, 0, None, false),
            First::Sequential {
                etag: Some("\"a\"".into()),
                total: None,
                expected_len: Some(100),
            }
        );
    }

    #[test]
    fn row_206_at_zero_without_a_strong_etag_is_sequential_when_it_is_the_whole_file() {
        let response = headers(&[
            "HTTP/1.1 206 Partial Content",
            "ETag: W/\"a\"",
            "Content-Range: bytes 0-99/100",
        ]);
        assert_eq!(
            classify_first(&response, 0, None, false),
            First::Sequential {
                etag: None,
                total: Some(100),
                expected_len: Some(100),
            }
        );
        // A range that does not start at zero cannot be trusted.
        let shifted = headers(&[
            "HTTP/1.1 206 Partial Content",
            "Content-Range: bytes 10-99/100",
        ]);
        assert!(matches!(
            classify_first(&shifted, 0, None, false),
            First::Fail(_)
        ));
    }

    #[test]
    fn row_416_at_zero_retries_once_plain_and_past_zero_restarts() {
        let response = headers(&["HTTP/1.1 416 Range Not Satisfiable"]);
        assert_eq!(classify_first(&response, 0, None, false), First::PlainGet);
        let point = resume(50, "\"a\"", Some(100));
        assert!(matches!(
            classify_first(&response, 50, Some(&point), false),
            First::IdentityRestart(_)
        ));
    }

    #[test]
    fn a_resume_checks_the_validator_start_and_total() {
        let point = resume(50, "\"a\"", Some(100));
        let good = headers(&[
            "HTTP/1.1 206 Partial Content",
            "ETag: \"a\"",
            "Content-Range: bytes 50-99/100",
        ]);
        assert!(matches!(
            classify_first(&good, 50, Some(&point), false),
            First::Ranged { total: 100, .. }
        ));
        for changed in [
            // another validator
            vec![
                "HTTP/1.1 206 Partial Content",
                "ETag: \"b\"",
                "Content-Range: bytes 50-99/100",
            ],
            // a weak validator
            vec![
                "HTTP/1.1 206 Partial Content",
                "ETag: W/\"a\"",
                "Content-Range: bytes 50-99/100",
            ],
            // another total
            vec![
                "HTTP/1.1 206 Partial Content",
                "ETag: \"a\"",
                "Content-Range: bytes 50-119/120",
            ],
            // another start
            vec![
                "HTTP/1.1 206 Partial Content",
                "ETag: \"a\"",
                "Content-Range: bytes 0-99/100",
            ],
            // an unknown total cannot equal the recorded one
            vec![
                "HTTP/1.1 206 Partial Content",
                "ETag: \"a\"",
                "Content-Range: bytes 50-99/*",
            ],
        ] {
            let response = headers(&changed);
            assert!(
                matches!(
                    classify_first(&response, 50, Some(&point), false),
                    First::IdentityRestart(_)
                ),
                "{changed:?}"
            );
        }
    }

    #[test]
    fn other_statuses_and_a_206_without_a_range_are_refused_as_they_are() {
        let error = headers(&["HTTP/1.1 404 Not Found"]);
        assert_eq!(
            classify_first(&error, 0, None, false),
            First::Fail("HTTP status 404".into())
        );
        let bare = headers(&["HTTP/1.1 206 Partial Content", "ETag: \"a\""]);
        assert_eq!(
            classify_first(&bare, 0, None, false),
            First::Fail("HTTP status 206".into())
        );
        // On a resume the retained bytes are dropped instead.
        let point = resume(50, "\"a\"", Some(100));
        assert!(matches!(
            classify_first(&bare, 50, Some(&point), false),
            First::IdentityRestart(_)
        ));
    }

    #[test]
    fn later_lanes_must_return_the_asked_range_of_the_same_representation() {
        let ok = headers(&[
            "HTTP/1.1 206 Partial Content",
            "ETag: \"a\"",
            "Content-Range: bytes 10-19/100",
        ]);
        assert!(check_range_response(&ok, 10, 20, "\"a\"", 100).is_ok());
        let other = headers(&[
            "HTTP/1.1 206 Partial Content",
            "ETag: \"b\"",
            "Content-Range: bytes 10-19/100",
        ]);
        assert!(matches!(
            check_range_response(&other, 10, 20, "\"a\"", 100),
            Err(Verdict::Identity(_))
        ));
        let resized = headers(&[
            "HTTP/1.1 206 Partial Content",
            "ETag: \"a\"",
            "Content-Range: bytes 10-19/101",
        ]);
        assert!(matches!(
            check_range_response(&resized, 10, 20, "\"a\"", 100),
            Err(Verdict::Identity(_))
        ));
        let elsewhere = headers(&[
            "HTTP/1.1 206 Partial Content",
            "ETag: \"a\"",
            "Content-Range: bytes 11-20/100",
        ]);
        assert!(matches!(
            check_range_response(&elsewhere, 10, 20, "\"a\"", 100),
            Err(Verdict::Fail(_))
        ));
        // 200 with the same validator: a node that ignores ranges.
        let whole = headers(&["HTTP/1.1 200 OK", "ETag: \"a\""]);
        assert!(matches!(
            check_range_response(&whole, 10, 20, "\"a\"", 100),
            Err(Verdict::IgnoredRange)
        ));
        // 200 with another validator, or none: the identity changed.
        for lines in [
            vec!["HTTP/1.1 200 OK", "ETag: \"b\""],
            vec!["HTTP/1.1 200 OK"],
        ] {
            let response = headers(&lines);
            assert!(matches!(
                check_range_response(&response, 10, 20, "\"a\"", 100),
                Err(Verdict::Identity(_))
            ));
        }
    }

    // -- clipping (spec 4.3) -------------------------------------------------

    type Log = Rc<RefCell<Vec<(u64, usize)>>>;

    fn ranged_state(total: u64) -> State {
        State {
            phase: Phase::Ranged,
            offset: 0,
            resume: None,
            etag: Some("\"e\"".into()),
            total: Some(total),
            frontier: Frontier::default(),
            claims: Vec::new(),
            max_ahead: 8 * MIB,
            claim_bytes: MIB,
            min_segment: MIB,
            min_adaptive: 4 * MIB,
            first_claim: MIB,
            largest_claim: MIB,
        }
    }

    /// A handler on a claim `[start, end)` whose sink records `(offset, len)`.
    fn lane<'a>(
        sink: &'a mut SinkFn<'a>,
        cancelled: &'a CancelFn<'a>,
        state: State,
        start: u64,
        end: u64,
        cover_end: u64,
    ) -> LaneHandler<'a> {
        let claim = Claim::new(LiveSegment::new(start, end), cover_end, 0);
        let shared = Rc::new(Shared {
            sink: RefCell::new(sink),
            cancelled,
            sink_error: RefCell::new(None),
            sink_nanos: Cell::new(0),
            written: Cell::new(0),
            ttfbs: RefCell::new(VecDeque::new()),
            state: RefCell::new(state),
        });
        shared.state.borrow_mut().claims.push(Rc::clone(&claim));
        LaneHandler {
            shared,
            claim,
            role: Role::Range,
            headers: ResponseHeaders::default(),
            frozen: true,
            verdict: None,
            etag: Some("\"e\"".into()),
            total: Some(100 * MIB),
        }
    }

    #[test]
    fn a_buffer_that_straddles_a_split_point_is_cut_there_and_stops_the_lane() {
        let log: Log = Rc::default();
        let recorder = Rc::clone(&log);
        let mut sink = move |chunk: Chunk<'_>| {
            recorder
                .borrow_mut()
                .push((chunk.offset, chunk.bytes.len()));
            Ok(())
        };
        let cancelled = || false;
        let mut lane = lane(&mut sink, &cancelled, ranged_state(100), 0, 100, 100);
        assert_eq!(lane.write_clipped(&[0; 80]), 80);
        // Another lane takes everything from byte 90.
        lane.claim.set_end(90);
        assert_eq!(
            lane.write_clipped(&[0; 30]),
            10,
            "only 10 bytes are still ours"
        );
        assert!(lane.claim.clipped.get());
        assert_eq!(*log.borrow(), vec![(0, 80), (80, 10)]);
        // The lane is stopped, and a callback that arrives anyway writes nothing.
        assert_eq!(lane.write_clipped(&[0; 5]), 0);
        assert_eq!(log.borrow().len(), 2);
    }

    #[test]
    fn a_claim_end_that_shrinks_between_two_callbacks_is_honoured() {
        let log: Log = Rc::default();
        let recorder = Rc::clone(&log);
        let mut sink = move |chunk: Chunk<'_>| {
            recorder
                .borrow_mut()
                .push((chunk.offset, chunk.bytes.len()));
            Ok(())
        };
        let cancelled = || false;
        let mut lane = lane(&mut sink, &cancelled, ranged_state(100), 0, 100, 100);
        assert_eq!(lane.write_clipped(&[0; 30]), 30);
        lane.claim.set_end(50);
        assert_eq!(lane.write_clipped(&[0; 60]), 20);
        assert_eq!(*log.borrow(), vec![(0, 30), (30, 20)]);
        assert_eq!(lane.claim.pos(), 50);
    }

    #[test]
    fn a_replaced_claim_writes_nothing_on_a_late_callback() {
        let log: Log = Rc::default();
        let recorder = Rc::clone(&log);
        let mut sink = move |chunk: Chunk<'_>| {
            recorder
                .borrow_mut()
                .push((chunk.offset, chunk.bytes.len()));
            Ok(())
        };
        let cancelled = || false;
        let mut lane = lane(&mut sink, &cancelled, ranged_state(100), 0, 100, 100);
        assert_eq!(lane.write_clipped(&[0; 10]), 10);
        lane.claim.retired.set(true);
        assert_eq!(lane.write_clipped(&[0; 10]), 0);
        assert_eq!(*log.borrow(), vec![(0, 10)]);
    }

    #[test]
    fn a_short_write_from_the_sink_is_an_error_and_stops_the_lane() {
        let mut sink = |_: Chunk<'_>| -> io::Result<()> { Err(io::Error::other("disk full")) };
        let cancelled = || false;
        let mut lane = lane(&mut sink, &cancelled, ranged_state(100), 0, 100, 100);
        assert_eq!(lane.write_clipped(&[0; 10]), 0);
        assert!(lane.shared.sink_error.borrow().is_some());
        assert_eq!(lane.claim.received(), 0, "nothing counts as received");
    }

    #[test]
    fn a_claim_extends_over_unclaimed_bytes_and_stops_at_claimed_ones() {
        let log: Log = Rc::default();
        let recorder = Rc::clone(&log);
        let mut sink = move |chunk: Chunk<'_>| {
            recorder
                .borrow_mut()
                .push((chunk.offset, chunk.bytes.len()));
            Ok(())
        };
        let cancelled = || false;
        let mut state = ranged_state(100 * MIB);
        // Lane 0 asked for the rest of the file; bytes 10.. are unclaimed.
        state.frontier.insert(10, 40);
        state.min_segment = 1;
        state.claim_bytes = 20;
        state.largest_claim = 20;
        let mut lane = lane(&mut sink, &cancelled, state, 0, 10, 100 * MIB);
        // The buffer crosses the claim's end; the next 20 bytes are free.
        assert_eq!(lane.write_clipped(&[0; 25]), 25);
        assert_eq!(lane.claim.end(), 30);
        assert_eq!(*log.borrow(), vec![(0, 10), (10, 15)]);
        // Someone else claims everything from 30 on: the lane may not go further.
        lane.shared.state.borrow_mut().frontier = Frontier::default();
        assert_eq!(lane.write_clipped(&[0; 25]), 5);
        assert_eq!(lane.claim.end(), 30);
        assert!(lane.claim.clipped.get());
    }

    #[test]
    fn a_claim_does_not_extend_beyond_the_ahead_limit() {
        let mut sink = |_: Chunk<'_>| Ok(());
        let cancelled = || false;
        let mut state = ranged_state(100 * MIB);
        state.max_ahead = 5;
        state.min_segment = 1;
        state.frontier.insert(50, 100);
        // Another lane still holds byte 0, so the prefix is 0 and the bytes
        // after this claim start beyond prefix + max_ahead.
        state
            .claims
            .push(Claim::new(LiveSegment::new(0, 10), 10, 0));
        let mut lane = lane(&mut sink, &cancelled, state, 10, 50, 100);
        assert_eq!(lane.write_clipped(&[0; 60]), 0);
        assert!(lane.claim.clipped.get());
        assert_eq!(lane.claim.end(), 10);
        assert_eq!(lane.shared.state.borrow().frontier.front(), Some((10, 50)));
    }

    #[test]
    fn an_old_claim_is_clipped_to_the_new_ahead_budget_and_its_suffix_returned() {
        let mut sink = |_: Chunk<'_>| Ok(());
        let cancelled = || false;
        let mut state = ranged_state(100);
        state.max_ahead = 20;
        state.min_segment = 1;
        state.frontier = Frontier::new(50, 100);
        state
            .claims
            .push(Claim::new(LiveSegment::new(0, 10), 10, 0));
        let mut lane = lane(&mut sink, &cancelled, state, 10, 50, 100);
        assert_eq!(lane.write_clipped(&[0; 60]), 10);
        assert_eq!(lane.claim.end(), 20);
        assert_eq!(lane.shared.state.borrow().frontier.front(), Some((20, 50)));
        assert!(lane.claim.clipped.get());
    }

    #[test]
    fn low_rate_ahead_budget_never_inherits_the_file_or_range_floor() {
        assert_eq!(max_ahead_for(None), 64 * 1024);
        assert_eq!(max_ahead_for(Some(1024.0)), 64 * 1024);
        assert_eq!(max_ahead_for(Some(5_000_000.0)), 15_000_000);
        let mut state = planner(8 * 1024 * MIB);
        state.max_ahead = max_ahead_for(Some(1024.0));
        state
            .claims
            .push(Claim::new(LiveSegment::new(0, 4 * MIB), 4 * MIB, 0));
        assert_eq!(state.plan_claim(4), Plan::Blocked);
        // The prefix lane can still take work, so the low-rate fallback cannot hang.
        state.claims.clear();
        state.frontier = Frontier::new(0, 8 * 1024 * MIB);
        assert!(matches!(state.plan_claim(1), Plan::Claim(0, _)));
    }

    #[test]
    fn retry_after_keeps_the_full_deadline_and_rejects_clock_overflow() {
        let now = Instant::now();
        assert_eq!(
            retry_deadline(now, Duration::from_secs(120)).unwrap() - now,
            Duration::from_secs(120)
        );
        assert!(matches!(
            retry_deadline(now, Duration::from_secs(u64::MAX)),
            Err(TransferError::Transport(_))
        ));
    }

    #[test]
    fn healthy_crawl_replacements_do_not_consume_the_retry_budget() {
        let mut attempts = 0;
        for _ in 0..20 {
            attempts = retry_attempts(attempts, 1024, true);
        }
        assert_eq!(attempts, 0);
        for _ in 0..4 {
            attempts = retry_attempts(attempts, 0, true);
        }
        assert!(attempts > RETRY_BUDGET);
        assert_eq!(retry_attempts(2, 1024, false), 3);
    }

    // -- planning claims ------------------------------------------------------

    fn planner(total: u64) -> State {
        let mut state = ranged_state(total);
        state.frontier = Frontier::new(4 * MIB, total);
        state.max_ahead = 1 << 40;
        state.first_claim = 4 * MIB;
        state.largest_claim = 4 * MIB;
        state.claim_bytes = 4 * MIB;
        state
    }

    #[test]
    fn a_claim_is_sized_by_time_grows_by_at_most_four_times_and_keeps_the_floor() {
        let mut state = planner(1000 * MIB);
        // A fast lane: 50 MiB in 1.5 s would be far more than double.
        state.claim_bytes = 50 * MIB;
        assert_eq!(state.plan_claim(2), Plan::Claim(4 * MIB, 20 * MIB));
        assert_eq!(state.largest_claim, 16 * MIB);
        assert_eq!(state.plan_claim(2), Plan::Claim(20 * MIB, 70 * MIB));
        // A slow lane never gets less than the floor.
        state.claim_bytes = 10;
        let Plan::Claim(start, end) = state.plan_claim(2) else {
            panic!("a claim was expected");
        };
        assert_eq!(end - start, MIB);
    }

    #[test]
    fn near_the_end_a_claim_is_an_even_share_of_what_is_unreceived() {
        let mut state = planner(100 * MIB);
        state.claim_bytes = 1000 * MIB;
        state.largest_claim = 1000 * MIB;
        // Two lanes and 96 MiB unclaimed: each takes half.
        assert_eq!(state.plan_claim(2), Plan::Claim(4 * MIB, 52 * MIB));
        // The share counts what the first lane still has to receive, so the
        // second lane does not halve the rest again.
        let claim = Claim::new(LiveSegment::new(4 * MIB, 52 * MIB), 52 * MIB, 0);
        state.claims.push(claim);
        state.frontier = Frontier::new(52 * MIB, 100 * MIB);
        assert_eq!(state.plan_claim(2), Plan::Claim(52 * MIB, 100 * MIB));
    }

    #[test]
    fn an_extent_does_not_leave_a_crumb() {
        let mut state = planner(20 * MIB);
        state.frontier = Frontier::new(0, 5 * MIB + 100);
        state.claim_bytes = 5 * MIB;
        state.largest_claim = 5 * MIB;
        let Plan::Claim(start, end) = state.plan_claim(1) else {
            panic!("a claim was expected");
        };
        assert_eq!((start, end), (0, 5 * MIB + 100));
    }

    #[test]
    fn claiming_is_blocked_beyond_max_ahead_of_the_prefix() {
        let mut state = planner(100 * MIB);
        state.max_ahead = 3 * MIB;
        // The prefix is 0 (a lane is at byte 0); the frontier starts at 4 MiB.
        state
            .claims
            .push(Claim::new(LiveSegment::new(0, 4 * MIB), 4 * MIB, 0));
        assert_eq!(state.plan_claim(2), Plan::Blocked);
        // Once the prefix moves, claiming resumes.
        let moved = Claim::new(LiveSegment::new(0, 4 * MIB), 4 * MIB, 0);
        moved.live.received.store(2 * MIB, Ordering::Relaxed);
        state.claims = vec![moved];
        assert!(matches!(state.plan_claim(2), Plan::Claim(..)));
    }

    #[test]
    fn nothing_unclaimed_is_reported_as_empty() {
        let mut state = planner(4 * MIB);
        assert_eq!(state.plan_claim(2), Plan::Empty);
    }

    /// Claims, failures and returns in a pseudo-random order never hand out a
    /// byte twice and never lose one.
    #[test]
    fn every_byte_is_owned_by_exactly_one_claim_through_claims_and_returns() {
        let total = 64 * MIB;
        let mut state = planner(total);
        state.frontier = Frontier::new(0, total);
        state.first_claim = MIB;
        state.largest_claim = MIB;
        let mut seed = 0x9e37_79b9_u64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let mut in_flight: Vec<(u64, u64)> = Vec::new();
        let mut done: Vec<(u64, u64)> = Vec::new();
        let mut guard = 0;
        while !state.frontier.is_empty() || !in_flight.is_empty() {
            guard += 1;
            assert!(guard < 100_000, "the walk does not end");
            state.claim_bytes = (1 + next() % 40) * MIB / 2;
            match next() % 3 {
                0 => {
                    if let Plan::Claim(start, end) = state.plan_claim(1 + (next() % 4) as usize) {
                        in_flight.push((start, end));
                    }
                }
                1 if !in_flight.is_empty() => {
                    // A lane fails part way: the rest goes back.
                    let index = (next() as usize) % in_flight.len();
                    let (start, end) = in_flight.swap_remove(index);
                    let cut = start + next() % (end - start);
                    if cut > start {
                        done.push((start, cut));
                    }
                    state.frontier.insert(cut, end);
                }
                _ if !in_flight.is_empty() => {
                    let index = (next() as usize) % in_flight.len();
                    done.push(in_flight.swap_remove(index));
                }
                _ => {}
            }
        }
        done.sort_unstable();
        let mut position = 0;
        for (start, end) in done {
            assert_eq!(start, position, "a gap or an overlap at {position}");
            position = end;
        }
        assert_eq!(position, total);
    }

    // -- HTTP/2 multiplexing is off (spec 4.7) ----------------------------------

    struct Collector;

    impl Handler for Collector {
        fn write(&mut self, data: &[u8]) -> Result<usize, WriteError> {
            Ok(data.len())
        }
    }

    /// A cleartext HTTP/2 server that speaks just enough to answer `200 ok`
    /// to every request stream, and notes how many streams each connection
    /// carried. It holds each answer until a second stream arrives on the
    /// same connection or 700 ms pass, so a multiplexing client is caught
    /// sending two streams down one connection.
    fn h2c_server(
        connections: Arc<std::sync::Mutex<Vec<usize>>>,
    ) -> (String, Arc<std::sync::atomic::AtomicBool>) {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        std::thread::spawn(move || {
            while !flag.load(Ordering::SeqCst) {
                let Ok((mut stream, _)) = listener.accept() else {
                    std::thread::sleep(Duration::from_millis(2));
                    continue;
                };
                let connections = Arc::clone(&connections);
                let flag = Arc::clone(&flag);
                std::thread::spawn(move || {
                    stream.set_nonblocking(false).unwrap();
                    stream
                        .set_read_timeout(Some(Duration::from_millis(50)))
                        .unwrap();
                    let index = {
                        let mut all = connections.lock().unwrap();
                        all.push(0);
                        all.len() - 1
                    };
                    let mut preface = [0_u8; 24];
                    if stream.read_exact(&mut preface).is_err() {
                        return;
                    }
                    // Our (empty) SETTINGS.
                    let _ = stream.write_all(&[0, 0, 0, 4, 0, 0, 0, 0, 0]);
                    let mut pending: Vec<u32> = Vec::new();
                    let mut first_at = None;
                    let deadline = Instant::now() + Duration::from_secs(8);
                    let mut header = [0_u8; 9];
                    let mut have = 0;
                    while Instant::now() < deadline && !flag.load(Ordering::SeqCst) {
                        match stream.read(&mut header[have..]) {
                            Ok(0) => return,
                            Ok(count) => have += count,
                            Err(error)
                                if matches!(
                                    error.kind(),
                                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                                ) => {}
                            Err(_) => return,
                        }
                        if have == 9 {
                            have = 0;
                            let length = u32::from_be_bytes([0, header[0], header[1], header[2]]);
                            let (kind, flags) = (header[3], header[4]);
                            let id = u32::from_be_bytes([
                                header[5] & 0x7f,
                                header[6],
                                header[7],
                                header[8],
                            ]);
                            let mut payload = vec![0_u8; length as usize];
                            stream
                                .set_read_timeout(Some(Duration::from_secs(2)))
                                .unwrap();
                            if stream.read_exact(&mut payload).is_err() {
                                return;
                            }
                            stream
                                .set_read_timeout(Some(Duration::from_millis(50)))
                                .unwrap();
                            match kind {
                                // SETTINGS without ACK: acknowledge.
                                4 if flags & 1 == 0 => {
                                    let _ = stream.write_all(&[0, 0, 0, 4, 1, 0, 0, 0, 0]);
                                }
                                // HEADERS: a request stream.
                                1 => {
                                    pending.push(id);
                                    connections.lock().unwrap()[index] += 1;
                                    first_at.get_or_insert_with(Instant::now);
                                }
                                _ => {}
                            }
                        }
                        let waited =
                            first_at.is_some_and(|at| at.elapsed() > Duration::from_millis(700));
                        if !pending.is_empty() && (pending.len() >= 2 || waited) {
                            for id in pending.drain(..) {
                                let id = id.to_be_bytes();
                                // HEADERS `:status 200`, then DATA `ok` ending the stream.
                                let _ = stream
                                    .write_all(&[0, 0, 1, 1, 4, id[0], id[1], id[2], id[3], 0x88]);
                                let _ = stream.write_all(&[
                                    0, 0, 2, 0, 1, id[0], id[1], id[2], id[3], b'o', b'k',
                                ]);
                            }
                        }
                    }
                });
            }
        });
        (url, stop)
    }

    #[test]
    fn two_requests_in_flight_use_two_http2_connections_because_multiplexing_is_off() {
        let connections = Arc::new(std::sync::Mutex::new(Vec::new()));
        let (url, stop) = h2c_server(Arc::clone(&connections));
        let multi = new_multi().unwrap();
        let mut handles = Vec::new();
        for _ in 0..2 {
            let mut easy = Easy2::new(Collector);
            easy.url(&url).unwrap();
            easy.http_version(curl::easy::HttpVersion::V2PriorKnowledge)
                .unwrap();
            handles.push(multi.add2(easy).unwrap());
            // Let the first request reach the server before the second is
            // added, as a lane that starts later does.
            let until = Instant::now() + Duration::from_millis(250);
            while Instant::now() < until {
                multi.perform().unwrap();
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut running = 2;
        while running > 0 && Instant::now() < deadline {
            running = multi.perform().unwrap();
            multi.wait(&mut [], Duration::from_millis(20)).unwrap();
        }
        assert_eq!(running, 0, "both requests finished");
        let mut done = 0;
        multi.messages(|message| {
            if message.result().is_some_and(|result| result.is_ok()) {
                done += 1;
            }
        });
        stop.store(true, Ordering::SeqCst);
        assert_eq!(done, 2, "both requests succeeded");
        let streams = connections.lock().unwrap().clone();
        assert_eq!(
            streams.len(),
            2,
            "two connections, one per request: {streams:?}"
        );
        assert!(
            streams.iter().all(|&count| count == 1),
            "each connection carried one stream: {streams:?}"
        );
    }
}
