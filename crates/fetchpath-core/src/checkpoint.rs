use fetchpath_storage::{CheckpointRecord, CheckpointStore, CompletedExtent, FaultInjector};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, VecDeque};
use std::fs::File;
use std::io;
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

const HASH_BUFFER_BYTES: usize = 256 * 1024;
/// Minimum new out-of-order extent slice to hash and persist on periodic
/// checkpoints so slow trickles never churn tiny extent records. Graceful
/// pauses (`persist_small_extents: true`) persist all remaining bytes.
const EXTENT_COMMIT_MIN_BYTES: u64 = 64 * 1024;
/// Target coalescing flush threshold per active lane (512 KiB, FP-148).
pub(crate) const COALESCE_FLUSH_BYTES: usize = 512 * 1024;
/// Maximum age of a partial coalescing buffer before flushing on slow links.
const COALESCE_MAX_AGE: Duration = Duration::from_millis(200);
/// Default total memory cap for local coalescing + async disk write queue
/// (stays strictly within GlobalBudget's 32 MiB ceiling under slow-disk stress).
pub(crate) const DEFAULT_WRITE_QUEUE_BUDGET: usize = 8 * 1024 * 1024;
const MAX_LANE_BUFFERS: usize = 16;
const MAX_FREE_BUFFERS: usize = 16;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct ResponseHeaders {
    pub status: Option<u32>,
    pub etag: Option<String>,
    pub content_length: Option<u64>,
    pub content_range: Option<ContentRange>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ContentRange {
    pub start: u64,
    pub end: u64,
    pub total: Option<u64>,
}

impl ResponseHeaders {
    pub fn ingest(&mut self, line: &[u8]) {
        let Ok(line) = std::str::from_utf8(line) else {
            return;
        };
        let line = line.trim();
        if line.starts_with("HTTP/") {
            *self = Self::default();
            self.status = line
                .split_ascii_whitespace()
                .nth(1)
                .and_then(|value| value.parse().ok());
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

#[cfg(test)]
pub(crate) fn source_key(url: &str) -> String {
    source_key_with_context(url, "")
}

pub(crate) fn source_key_with_context(url: &str, context_fingerprint: &str) -> String {
    // Query values can contain signed URLs or credentials. They never enter
    // persistent checkpoint identity; the strong response validator is the
    // authority for reusing retained bytes.
    let redacted = url.split(['?', '#']).next().unwrap_or(url);
    let mut hasher = Sha256::new();
    hasher.update(redacted.as_bytes());
    hasher.update([0]);
    hasher.update(context_fingerprint.as_bytes());
    let digest = format!("{:x}", hasher.finalize());
    digest[..32].to_owned()
}

#[cfg(test)]
pub(crate) fn strong_etag(value: Option<&str>) -> Option<String> {
    let value = value?.trim();
    if value.starts_with("W/") || value.starts_with("w/") {
        return None;
    }
    if value.len() >= 2 && value.starts_with('"') && value.ends_with('"') {
        Some(value.to_owned())
    } else {
        None
    }
}

fn parse_content_range(value: &str) -> Option<ContentRange> {
    let value = value.strip_prefix("bytes ")?;
    let (range, total) = value.split_once('/')?;
    let (start, end) = range.split_once('-')?;
    let start = start.parse().ok()?;
    let end = end.parse().ok()?;
    if end < start {
        return None;
    }
    let total = if total == "*" {
        None
    } else {
        Some(total.parse().ok()?)
    };
    if total.is_some_and(|total| end >= total) {
        return None;
    }
    Some(ContentRange { start, end, total })
}

/// Which bytes of the staging file have been written this attempt, tracked
/// purely in memory without touching the disk or running SHA-256 on the socket
/// callback thread (FP-146).
///
/// The set starts as `[0, committed_len)`. Completion is never inferred from
/// the file's length: after an in-process retry, stale bytes of the same
/// identity can sit past the prefix. Hashing and checkpoint commits run off
/// the socket thread in [`CommitQueue`].
pub(crate) struct PrefixTracker {
    /// Merged completed extents, `start -> end` (end exclusive).
    done: BTreeMap<u64, u64>,
    /// Length of the contiguous prefix starting at byte 0.
    prefix: u64,
    /// Bytes recorded in all, prefix included.
    total: u64,
}

impl PrefixTracker {
    #[cfg(test)]
    pub(crate) fn new(committed: u64) -> Self {
        Self::with_extents(committed, &[])
    }

    pub(crate) fn with_extents(committed: u64, extents: &[(u64, u64)]) -> Self {
        let mut tracker = Self {
            done: BTreeMap::new(),
            prefix: 0,
            total: 0,
        };
        if committed > 0 {
            tracker.done.insert(0, committed);
            tracker.prefix = committed;
            tracker.total = committed;
        }
        for &(start, end) in extents {
            if start < end {
                let mut merged_start = start;
                let mut merged_end = end;
                if let Some((&before_start, &before_end)) = tracker.done.range(..=start).next_back()
                    && before_end == start
                {
                    tracker.done.remove(&before_start);
                    merged_start = before_start;
                }
                if let Some(&after_end) = tracker.done.get(&end) {
                    tracker.done.remove(&end);
                    merged_end = after_end;
                }
                tracker.done.insert(merged_start, merged_end);
                tracker.total += end - start;
            }
        }
        tracker.prefix = tracker.done.get(&0).copied().unwrap_or(0);
        tracker
    }

    /// The first byte not yet written.
    pub(crate) fn prefix(&self) -> u64 {
        self.prefix
    }

    /// Bytes written so far, in or beyond the prefix.
    pub(crate) fn written(&self) -> u64 {
        self.total
    }

    /// Merged completed byte extents strictly past the contiguous prefix.
    pub(crate) fn non_prefix_extents(&self) -> Vec<(u64, u64)> {
        self.done
            .iter()
            .filter(|&(&start, _)| start > self.prefix)
            .map(|(&start, &end)| (start, end))
            .collect()
    }

    /// Fails when `len` bytes at `offset` overlap bytes already recorded: a
    /// byte is written once. Call it before the write, so an overlap never
    /// reaches the file.
    pub(crate) fn check(&self, offset: u64, len: usize) -> io::Result<()> {
        let end = offset + len as u64;
        let overlaps_before = len > 0
            && self
                .done
                .range(..end)
                .next_back()
                .is_some_and(|(_, &before_end)| before_end > offset);
        if overlaps_before {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("bytes {offset}..{end} overlap bytes already written"),
            ));
        }
        Ok(())
    }

    /// Records that `len` bytes are now on disk at `offset`. Call it only after
    /// the write returned, and after [`Self::check`] passed.
    pub(crate) fn record(&mut self, offset: u64, len: usize) -> io::Result<()> {
        if len == 0 {
            return Ok(());
        }
        self.check(offset, len)?;
        let end = offset + len as u64;
        let mut start = offset;
        let mut stop = end;
        // Merge with a neighbour that ends where this begins.
        if let Some((&before_start, &before_end)) = self.done.range(..=offset).next_back()
            && before_end == offset
        {
            self.done.remove(&before_start);
            start = before_start;
        }
        // Merge with a neighbour that begins where this ends.
        if let Some(&after_end) = self.done.get(&end) {
            self.done.remove(&end);
            stop = after_end;
        }
        self.done.insert(start, stop);
        self.total += len as u64;
        self.prefix = self.done.get(&0).copied().unwrap_or(0);
        Ok(())
    }
}

pub(crate) struct CommitRequest {
    pub prefix: u64,
    /// Merged completed non-prefix extents `(start, end)` strictly past `prefix`.
    pub extents: Vec<(u64, u64)>,
    pub source_key: String,
    pub strong_etag: Option<String>,
    pub expected_total: Option<u64>,
    /// When true (graceful pause), persist even sub-64 KiB out-of-order tails.
    pub persist_small_extents: bool,
}

struct QueueState {
    /// Highest contiguous prefix written to staging so far.
    target_prefix: u64,
    /// Pending durable checkpoint commit; a newer request replaces an unstarted one.
    pending_commit: Option<CommitRequest>,
    /// Set when the transfer ends and wants the worker to drain and exit.
    closed: bool,
    /// When true on close, finish hashing up to `target_prefix` and record `final_digest`.
    finish_hash: bool,
    /// When true, stop immediately without hashing remaining tail or committing.
    aborted: bool,
    error: Option<io::Error>,
    /// Prefix length hashed so far by the worker.
    hashed: u64,
    /// Contiguous prefix length of the last checkpoint that reached disk.
    committed: u64,
    /// Total retained bytes (prefix + out-of-order extents) of the last checkpoint on disk.
    committed_bytes: u64,
    /// Final hex SHA-256 of `[0, hashed)` once `finish` completed cleanly.
    final_digest: Option<String>,
}

/// Pipelined background hasher and checkpoint committer (FP-146, FP-147).
///
/// Owns the running `Sha256` over `[0, hashed)`, cached digests for committed
/// out-of-order extents, and a single reused 256 KiB read buffer on a
/// dedicated read-only staging handle, so neither out-of-order read-back hashing
/// nor `FlushFileBuffers` ever stalls the socket event loop. On clean
/// completion, returns the single-pass finalized SHA-256 directly.
pub(crate) struct CommitQueue {
    state: Mutex<QueueState>,
    changed: Condvar,
}

impl CommitQueue {
    pub(crate) fn new(committed_prefix: u64, committed_bytes: u64) -> Self {
        Self {
            state: Mutex::new(QueueState {
                target_prefix: committed_prefix,
                pending_commit: None,
                closed: false,
                finish_hash: false,
                aborted: false,
                error: None,
                hashed: committed_prefix,
                committed: committed_prefix,
                committed_bytes: committed_bytes.max(committed_prefix),
                final_digest: None,
            }),
            changed: Condvar::new(),
        }
    }

    /// Tells the background hasher that `[0, prefix)` is now written to staging.
    pub(crate) fn advance_prefix(&self, prefix: u64) {
        let mut state = self.state.lock().expect("commit queue poisoned");
        if !state.closed && !state.aborted && prefix > state.target_prefix {
            state.target_prefix = prefix;
            self.changed.notify_all();
        }
    }

    /// Queues a durable checkpoint commit without waiting.
    pub(crate) fn submit_commit(&self, request: CommitRequest) {
        let mut state = self.state.lock().expect("commit queue poisoned");
        if !state.closed && !state.aborted {
            if request.prefix > state.target_prefix {
                state.target_prefix = request.prefix;
            }
            state.pending_commit = Some(request);
            self.changed.notify_all();
        }
    }

    /// True once the worker has failed; the sink then stops the transfer.
    pub(crate) fn failed(&self) -> bool {
        self.state
            .lock()
            .expect("commit queue poisoned")
            .error
            .is_some()
    }

    /// Non-destructive error check for the sink callback so post-scope
    /// `failed()` and `take_error()` remain authoritative.
    pub(crate) fn check_error(&self) -> io::Result<()> {
        let state = self.state.lock().expect("commit queue poisoned");
        if let Some(error) = state.error.as_ref() {
            return Err(io::Error::new(error.kind(), error.to_string()));
        }
        Ok(())
    }

    pub(crate) fn take_error(&self) -> Option<io::Error> {
        self.state
            .lock()
            .expect("commit queue poisoned")
            .error
            .take()
    }

    /// Prefix length of the newest checkpoint on disk.
    pub(crate) fn committed(&self) -> u64 {
        self.state.lock().expect("commit queue poisoned").committed
    }

    /// Total retained bytes (prefix + extents) of the newest checkpoint on disk.
    pub(crate) fn committed_bytes(&self) -> u64 {
        self.state
            .lock()
            .expect("commit queue poisoned")
            .committed_bytes
    }

    /// Prefix length hashed by the worker when it stopped.
    pub(crate) fn hashed(&self) -> u64 {
        self.state.lock().expect("commit queue poisoned").hashed
    }

    /// Final SHA-256 produced by [`Self::finish`], after the worker has joined.
    pub(crate) fn take_final_digest(&self) -> Option<String> {
        self.state
            .lock()
            .expect("commit queue poisoned")
            .final_digest
            .take()
    }

    /// Asks the worker to finish hashing `[0, final_prefix)`, process any
    /// pending checkpoint, record the final digest, and exit.
    pub(crate) fn finish(&self, final_prefix: u64) {
        let mut state = self.state.lock().expect("commit queue poisoned");
        if final_prefix > state.target_prefix {
            state.target_prefix = final_prefix;
        }
        state.finish_hash = true;
        state.closed = true;
        self.changed.notify_all();
    }

    /// Asks the worker to finish any already-queued checkpoint and exit without
    /// hashing unrequested tail bytes.
    pub(crate) fn close(&self) {
        let mut state = self.state.lock().expect("commit queue poisoned");
        state.closed = true;
        self.changed.notify_all();
    }

    /// Stops the worker at once without finishing tail hashing or commits
    /// (used for `RemoveStaging` cancellation).
    pub(crate) fn abort(&self) {
        let mut state = self.state.lock().expect("commit queue poisoned");
        state.aborted = true;
        state.closed = true;
        state.pending_commit = None;
        self.changed.notify_all();
    }

    fn is_aborted(&self) -> bool {
        self.state.lock().expect("commit queue poisoned").aborted
    }

    fn record_error(&self, error: io::Error) {
        let mut state = self.state.lock().expect("commit queue poisoned");
        state.error = Some(error);
        state.aborted = true;
        state.closed = true;
        state.pending_commit = None;
    }

    fn hash_prefix_into(
        &self,
        store: &CheckpointStore,
        reader: &mut Option<File>,
        buffer: &mut [u8],
        hasher: &mut Sha256,
        hashed: &mut u64,
        goal: u64,
    ) -> io::Result<bool> {
        while *hashed < goal {
            if self.is_aborted() {
                return Ok(false);
            }
            let file = open_reader(store, reader)?;
            let count = (goal - *hashed).min(buffer.len() as u64) as usize;
            fetchpath_storage::read_exact_at(file, *hashed, &mut buffer[..count])?;
            hasher.update(&buffer[..count]);
            *hashed += count as u64;
        }
        Ok(true)
    }

    fn hash_extent_slice(
        &self,
        store: &CheckpointStore,
        reader: &mut Option<File>,
        buffer: &mut [u8],
        start: u64,
        end: u64,
    ) -> io::Result<Option<String>> {
        let mut extent_hasher = Sha256::new();
        let mut pos = start;
        while pos < end {
            if self.is_aborted() {
                return Ok(None);
            }
            let file = open_reader(store, reader)?;
            let count = (end - pos).min(buffer.len() as u64) as usize;
            fetchpath_storage::read_exact_at(file, pos, &mut buffer[..count])?;
            extent_hasher.update(&buffer[..count]);
            pos += count as u64;
        }
        Ok(Some(format!("{:x}", extent_hasher.finalize())))
    }

    /// The worker thread's body. `initial_hasher` must hold the SHA-256 state
    /// over `[0, initial_offset)` and `initial_extents` holds any already
    /// verified out-of-order extents strictly past `initial_offset`.
    pub(crate) fn run(
        &self,
        store: &CheckpointStore,
        initial_offset: u64,
        mut hasher: Sha256,
        initial_extents: Vec<CompletedExtent>,
        faults: &dyn FaultInjector,
    ) {
        let mut hashed = initial_offset;
        let mut cached_extents = initial_extents;
        let mut reader: Option<File> = None;
        let mut buffer = vec![0_u8; HASH_BUFFER_BYTES];

        loop {
            let (hash_goal, commit, closed, finish_hash) = {
                let mut state = self.state.lock().expect("commit queue poisoned");
                loop {
                    if state.aborted {
                        state.hashed = hashed;
                        return;
                    }
                    let commit = state.pending_commit.take();
                    let goal = if state.finish_hash || !state.closed {
                        state.target_prefix
                    } else if let Some(ref req) = commit {
                        req.prefix
                    } else {
                        hashed
                    };
                    if hashed < goal || commit.is_some() {
                        break (goal, commit, state.closed, state.finish_hash);
                    }
                    if state.closed {
                        state.hashed = hashed;
                        if state.finish_hash {
                            state.final_digest = Some(format!("{:x}", hasher.clone().finalize()));
                        }
                        return;
                    }
                    state = self.changed.wait(state).expect("commit queue poisoned");
                }
            };

            // Advance SHA-256 across [hashed, hash_goal) using a single reused
            // buffer on a dedicated read handle, checking for abort per chunk.
            match self.hash_prefix_into(
                store,
                &mut reader,
                &mut buffer,
                &mut hasher,
                &mut hashed,
                hash_goal,
            ) {
                Ok(true) => {}
                Ok(false) => {
                    let mut state = self.state.lock().expect("commit queue poisoned");
                    state.hashed = hashed;
                    return;
                }
                Err(error) => {
                    self.record_error(error);
                    return;
                }
            }

            if let Some(req) = commit {
                // If target_prefix advanced into the start of a requested
                // out-of-order extent, fold that contiguous extent into the
                // prefix first so every persisted extent stays strictly past
                // `committed_len`.
                let mut pending_extents = Vec::with_capacity(req.extents.len());
                for (start, end) in req.extents {
                    if end <= hashed {
                        continue;
                    }
                    if start <= hashed {
                        match self.hash_prefix_into(
                            store,
                            &mut reader,
                            &mut buffer,
                            &mut hasher,
                            &mut hashed,
                            end,
                        ) {
                            Ok(true) => continue,
                            Ok(false) => {
                                let mut state = self.state.lock().expect("commit queue poisoned");
                                state.hashed = hashed;
                                return;
                            }
                            Err(error) => {
                                self.record_error(error);
                                return;
                            }
                        }
                    }
                    pending_extents.push((start, end));
                }

                // Reuse already-hashed out-of-order extents strictly past
                // `hashed` and hash only newly completed slices.
                let min_slice = if req.persist_small_extents {
                    1
                } else {
                    EXTENT_COMMIT_MIN_BYTES
                };
                let mut next_extents = Vec::new();
                for (req_start, req_end) in pending_extents {
                    let mut pos = req_start;
                    for cached in &cached_extents {
                        if cached.end <= pos || cached.start < req_start {
                            continue;
                        }
                        if cached.start >= req_end {
                            break;
                        }
                        if cached.end > req_end {
                            continue;
                        }
                        if cached.start > pos {
                            let slice_len = cached.start - pos;
                            if slice_len >= min_slice {
                                match self.hash_extent_slice(
                                    store,
                                    &mut reader,
                                    &mut buffer,
                                    pos,
                                    cached.start,
                                ) {
                                    Ok(Some(sha256)) => next_extents.push(CompletedExtent {
                                        start: pos,
                                        end: cached.start,
                                        sha256,
                                    }),
                                    Ok(None) => {
                                        let mut state =
                                            self.state.lock().expect("commit queue poisoned");
                                        state.hashed = hashed;
                                        return;
                                    }
                                    Err(error) => {
                                        self.record_error(error);
                                        return;
                                    }
                                }
                            }
                        }
                        next_extents.push(cached.clone());
                        pos = cached.end;
                    }
                    if req_end > pos && req_end - pos >= min_slice {
                        match self.hash_extent_slice(store, &mut reader, &mut buffer, pos, req_end)
                        {
                            Ok(Some(sha256)) => next_extents.push(CompletedExtent {
                                start: pos,
                                end: req_end,
                                sha256,
                            }),
                            Ok(None) => {
                                let mut state = self.state.lock().expect("commit queue poisoned");
                                state.hashed = hashed;
                                return;
                            }
                            Err(error) => {
                                self.record_error(error);
                                return;
                            }
                        }
                    }
                }

                if self.is_aborted() {
                    let mut state = self.state.lock().expect("commit queue poisoned");
                    state.hashed = hashed;
                    return;
                }
                let digest = format!("{:x}", hasher.clone().finalize());
                let len = hashed;
                let record = CheckpointRecord::downloading_with_extents(
                    req.source_key,
                    len,
                    digest,
                    req.strong_etag,
                    req.expected_total,
                    next_extents.clone(),
                );
                let retained = record.retained_bytes();
                let result = store
                    .sync_payload_reopened(faults)
                    .and_then(|()| store.commit(record, faults).map(|_| ()));
                match result {
                    Ok(()) => {
                        cached_extents = next_extents;
                        let mut state = self.state.lock().expect("commit queue poisoned");
                        state.hashed = hashed;
                        state.committed = state.committed.max(len);
                        state.committed_bytes = state.committed_bytes.max(retained);
                    }
                    Err(error) => {
                        self.record_error(error);
                        return;
                    }
                }
            } else if closed && finish_hash {
                // Loop back once to pick up any newer target_prefix or finalize.
                let mut state = self.state.lock().expect("commit queue poisoned");
                state.hashed = hashed;
            }
        }
    }
}

fn open_reader<'a>(store: &CheckpointStore, reader: &'a mut Option<File>) -> io::Result<&'a File> {
    if reader.is_none() {
        *reader = Some(File::open(store.staging())?);
    }
    Ok(reader.as_ref().expect("reader initialized"))
}

struct WriteBatch {
    offset: u64,
    data: Vec<u8>,
}

struct WriterState {
    pending: VecDeque<WriteBatch>,
    /// Bytes currently queued in `pending` plus the batch being written to disk.
    in_flight_bytes: usize,
    peak_buffered_bytes: usize,
    backpressure_waits: u64,
    closed: bool,
    aborted: bool,
    error: Option<io::Error>,
    free_pool: Vec<Vec<u8>>,
    /// Extents durably written to the OS file handle (updated strictly AFTER
    /// `store.write_payload_at` succeeds so `CommitQueue` never reads unwritten bytes).
    on_disk: PrefixTracker,
}

/// Bounded asynchronous disk write queue with recycled buffers and explicit
/// backpressure (FP-148).
///
/// The socket thread coalesces small libcurl callbacks into 512 KiB contiguous
/// batches via [`LaneCoalescer`] and pushes them here; a dedicated disk I/O
/// worker thread drains them with `write_payload_at`, updates `on_disk`, and
/// wakes [`CommitQueue`] only after bytes have reached the OS file.
pub(crate) struct WriteQueue {
    state: Mutex<WriterState>,
    not_empty: Condvar,
    not_full: Condvar,
    max_bytes: usize,
}

impl WriteQueue {
    pub(crate) fn new(committed_prefix: u64, extents: &[(u64, u64)], max_bytes: usize) -> Self {
        Self {
            state: Mutex::new(WriterState {
                pending: VecDeque::new(),
                in_flight_bytes: 0,
                peak_buffered_bytes: 0,
                backpressure_waits: 0,
                closed: false,
                aborted: false,
                error: None,
                free_pool: Vec::new(),
                on_disk: PrefixTracker::with_extents(committed_prefix, extents),
            }),
            not_empty: Condvar::new(),
            not_full: Condvar::new(),
            max_bytes: max_bytes.max(1),
        }
    }

    pub(crate) fn max_bytes(&self) -> usize {
        self.max_bytes
    }

    pub(crate) fn check_error(&self) -> io::Result<()> {
        let state = self.state.lock().expect("write queue poisoned");
        if let Some(error) = state.error.as_ref() {
            return Err(io::Error::new(error.kind(), error.to_string()));
        }
        Ok(())
    }

    pub(crate) fn failed(&self) -> bool {
        self.state
            .lock()
            .expect("write queue poisoned")
            .error
            .is_some()
    }

    pub(crate) fn take_error(&self) -> Option<io::Error> {
        self.state
            .lock()
            .expect("write queue poisoned")
            .error
            .take()
    }

    fn take_buffer(&self, capacity: usize) -> Vec<u8> {
        let mut state = self.state.lock().expect("write queue poisoned");
        if let Some(mut buf) = state.free_pool.pop() {
            buf.clear();
            buf.reserve(capacity);
            buf
        } else {
            Vec::with_capacity(capacity)
        }
    }

    /// Enqueues a coalesced write batch, blocking on backpressure when total
    /// buffered bytes (`in_flight_bytes + local_bytes + data.len()`) exceed
    /// `max_bytes`. Polls `cancelled` every 20 ms so cancel never deadlocks.
    ///
    /// Returns `Ok(None)` when the batch was queued, or `Ok(Some(data))` if
    /// cancellation interrupted backpressure before queueing so the caller can
    /// retain the batch for a `RetainStaging` flush or discard it on removal.
    pub(crate) fn push(
        &self,
        offset: u64,
        data: Vec<u8>,
        local_bytes: usize,
        cancelled: &dyn Fn() -> bool,
    ) -> io::Result<Option<Vec<u8>>> {
        if data.is_empty() {
            return Ok(None);
        }
        let batch_len = data.len();
        let mut state = self.state.lock().expect("write queue poisoned");
        while !state.aborted
            && state.error.is_none()
            && state.in_flight_bytes > 0
            && state.in_flight_bytes + local_bytes + batch_len > self.max_bytes
        {
            if cancelled() {
                return Ok(Some(data));
            }
            state.backpressure_waits += 1;
            let (next, _) = self
                .not_full
                .wait_timeout(state, Duration::from_millis(20))
                .expect("write queue poisoned");
            state = next;
        }
        if let Some(error) = state.error.as_ref() {
            return Err(io::Error::new(error.kind(), error.to_string()));
        }
        if state.aborted || state.closed {
            return Ok(Some(data));
        }
        state.in_flight_bytes += batch_len;
        state.peak_buffered_bytes = state
            .peak_buffered_bytes
            .max(state.in_flight_bytes + local_bytes);
        state.pending.push_back(WriteBatch { offset, data });
        self.not_empty.notify_one();
        Ok(None)
    }

    /// Closes the queue gracefully so the disk worker drains all pending
    /// batches and exits.
    pub(crate) fn close(&self) {
        let mut state = self.state.lock().expect("write queue poisoned");
        state.closed = true;
        self.not_empty.notify_all();
    }

    /// Aborts immediately, dropping all unwritten batches and waking any
    /// backpressured producer (used for `RemoveStaging` cancel).
    pub(crate) fn abort(&self) {
        let mut state = self.state.lock().expect("write queue poisoned");
        state.aborted = true;
        state.closed = true;
        state.pending.clear();
        state.in_flight_bytes = 0;
        self.not_empty.notify_all();
        self.not_full.notify_all();
    }

    /// Returns `(contiguous_prefix, total_written, non_prefix_extents)` of all
    /// bytes durably written to the OS file handle.
    pub(crate) fn on_disk_snapshot(&self) -> (u64, u64, Vec<(u64, u64)>) {
        let state = self.state.lock().expect("write queue poisoned");
        (
            state.on_disk.prefix(),
            state.on_disk.written(),
            state.on_disk.non_prefix_extents(),
        )
    }

    #[cfg(test)]
    pub(crate) fn stats(&self) -> (usize, u64) {
        let state = self.state.lock().expect("write queue poisoned");
        (state.peak_buffered_bytes, state.backpressure_waits)
    }

    /// Dedicated disk I/O worker thread body. Writes coalesced batches to
    /// `file`, updates `on_disk`, and drives [`CommitQueue`] hashing and
    /// periodic checkpoint commits strictly after each write completes.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn run(
        &self,
        store: &CheckpointStore,
        file: &File,
        commit_queue: &CommitQueue,
        identity: &Mutex<(Option<String>, Option<u64>)>,
        source_key: String,
        hash_pipeline_bytes: u64,
        checkpoint_bytes: u64,
        checkpoint_interval: Duration,
        faults: &dyn FaultInjector,
    ) {
        let (mut last_hashed_notify, initial_written) = {
            let state = self.state.lock().expect("write queue poisoned");
            (state.on_disk.prefix(), state.on_disk.written())
        };
        let mut last_submit = (Instant::now(), initial_written);

        loop {
            let batch = {
                let mut state = self.state.lock().expect("write queue poisoned");
                loop {
                    if state.aborted {
                        state.pending.clear();
                        state.in_flight_bytes = 0;
                        self.not_full.notify_all();
                        return;
                    }
                    if let Some(batch) = state.pending.pop_front() {
                        break batch;
                    }
                    if state.closed {
                        return;
                    }
                    state = self.not_empty.wait(state).expect("write queue poisoned");
                }
            };

            let batch_len = batch.data.len();
            if let Err(error) = store.write_payload_at(file, batch.offset, &batch.data, faults) {
                let mut state = self.state.lock().expect("write queue poisoned");
                state.error = Some(error);
                state.aborted = true;
                state.closed = true;
                state.pending.clear();
                state.in_flight_bytes = 0;
                self.not_full.notify_all();
                return;
            }

            let mut commit_req: Option<CommitRequest> = None;
            let mut advance_to: Option<u64> = None;
            {
                let mut state = self.state.lock().expect("write queue poisoned");
                state.in_flight_bytes = state.in_flight_bytes.saturating_sub(batch_len);
                let mut buf = batch.data;
                if state.free_pool.len() < MAX_FREE_BUFFERS
                    && buf.capacity() <= COALESCE_FLUSH_BYTES * 2
                {
                    buf.clear();
                    state.free_pool.push(buf);
                }
                if let Err(error) = state.on_disk.record(batch.offset, batch_len) {
                    state.error = Some(error);
                    state.aborted = true;
                    state.closed = true;
                    state.pending.clear();
                    state.in_flight_bytes = 0;
                    self.not_full.notify_all();
                    return;
                }
                let prefix = state.on_disk.prefix();
                let written = state.on_disk.written();
                if prefix.saturating_sub(last_hashed_notify) >= hash_pipeline_bytes {
                    advance_to = Some(prefix);
                    last_hashed_notify = prefix;
                }
                if written.saturating_sub(last_submit.1) >= checkpoint_bytes
                    && last_submit.0.elapsed() >= checkpoint_interval
                {
                    let (etag, total) = identity.lock().expect("identity lock poisoned").clone();
                    if etag.is_some() {
                        commit_req = Some(CommitRequest {
                            prefix,
                            extents: state.on_disk.non_prefix_extents(),
                            source_key: source_key.clone(),
                            strong_etag: etag,
                            expected_total: total,
                            persist_small_extents: false,
                        });
                        last_submit = (Instant::now(), written);
                        last_hashed_notify = prefix;
                    }
                }
                self.not_full.notify_all();
            }

            if let Some(req) = commit_req {
                commit_queue.submit_commit(req);
            } else if let Some(prefix) = advance_to {
                commit_queue.advance_prefix(prefix);
            }
        }
    }
}

struct LaneBuffer {
    start: u64,
    data: Vec<u8>,
    created_at: Instant,
}

/// Producer-side per-lane write coalescer owned exclusively by the socket
/// callback thread (FP-148). Accumulates contiguous 16 KiB callbacks into
/// 512 KiB batches before handing them off to [`WriteQueue`].
pub(crate) struct LaneCoalescer<'a> {
    queue: &'a WriteQueue,
    buffers: Vec<LaneBuffer>,
    local_bytes: usize,
    flush_bytes: usize,
}

impl<'a> LaneCoalescer<'a> {
    pub(crate) fn new(queue: &'a WriteQueue, flush_bytes: usize) -> Self {
        Self {
            queue,
            buffers: Vec::new(),
            local_bytes: 0,
            flush_bytes: flush_bytes.max(1),
        }
    }

    pub(crate) fn enqueue(
        &mut self,
        offset: u64,
        bytes: &[u8],
        cancelled: &dyn Fn() -> bool,
    ) -> io::Result<()> {
        self.queue.check_error()?;
        if bytes.is_empty() {
            return Ok(());
        }
        if let Some(index) = self
            .buffers
            .iter()
            .position(|buf| buf.start + buf.data.len() as u64 == offset)
        {
            self.buffers[index].data.extend_from_slice(bytes);
            self.local_bytes += bytes.len();
            if self.buffers[index].data.len() >= self.flush_bytes
                || self.buffers[index].created_at.elapsed() >= COALESCE_MAX_AGE
            {
                self.flush_index(index, cancelled)?;
            }
        } else {
            self.flush_stale(cancelled)?;
            if self.buffers.len() >= MAX_LANE_BUFFERS {
                self.flush_largest(cancelled)?;
            }
            let capacity = self.flush_bytes.min(bytes.len().max(64 * 1024));
            let mut data = self.queue.take_buffer(capacity);
            data.extend_from_slice(bytes);
            self.local_bytes += bytes.len();
            self.buffers.push(LaneBuffer {
                start: offset,
                data,
                created_at: Instant::now(),
            });
            let last = self.buffers.len() - 1;
            if self.buffers[last].data.len() >= self.flush_bytes {
                self.flush_index(last, cancelled)?;
            }
        }
        let local_cap = (self.queue.max_bytes() / 2).max(self.flush_bytes);
        while self.local_bytes > local_cap && !self.buffers.is_empty() {
            self.flush_largest(cancelled)?;
        }
        Ok(())
    }

    fn flush_stale(&mut self, cancelled: &dyn Fn() -> bool) -> io::Result<()> {
        let mut index = 0;
        while index < self.buffers.len() {
            if self.buffers[index].created_at.elapsed() >= COALESCE_MAX_AGE {
                self.flush_index(index, cancelled)?;
            } else {
                index += 1;
            }
        }
        Ok(())
    }

    fn flush_largest(&mut self, cancelled: &dyn Fn() -> bool) -> io::Result<()> {
        if let Some((index, _)) = self
            .buffers
            .iter()
            .enumerate()
            .max_by_key(|(_, buf)| buf.data.len())
        {
            self.flush_index(index, cancelled)?;
        }
        Ok(())
    }

    fn flush_index(&mut self, index: usize, cancelled: &dyn Fn() -> bool) -> io::Result<()> {
        let batch = self.buffers.swap_remove(index);
        let len = batch.data.len();
        self.local_bytes = self.local_bytes.saturating_sub(len);
        match self
            .queue
            .push(batch.start, batch.data, self.local_bytes, cancelled)?
        {
            None => Ok(()),
            Some(data) => {
                // Restore the unpushed batch so `RetainStaging` can flush it
                // cleanly during shutdown while `RemoveStaging` discards it.
                self.local_bytes += data.len();
                self.buffers.push(LaneBuffer {
                    start: batch.start,
                    data,
                    created_at: batch.created_at,
                });
                Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "write backpressure interrupted by cancellation",
                ))
            }
        }
    }

    pub(crate) fn flush_all(&mut self, cancelled: &dyn Fn() -> bool) -> io::Result<()> {
        // Flush in ascending offset order so sequential tails land contiguously.
        self.buffers.sort_by_key(|buf| buf.start);
        let drained = std::mem::take(&mut self.buffers);
        let mut iter = drained.into_iter();
        while let Some(batch) = iter.next() {
            self.local_bytes = self.local_bytes.saturating_sub(batch.data.len());
            if let Some(data) =
                self.queue
                    .push(batch.start, batch.data, self.local_bytes, cancelled)?
            {
                self.local_bytes += data.len();
                self.buffers.push(LaneBuffer {
                    start: batch.start,
                    data,
                    created_at: batch.created_at,
                });
                // Remaining items in `iter` are still accounted for in `self.local_bytes`.
                self.buffers.extend(iter);
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "write flush interrupted by cancellation",
                ));
            }
        }
        self.local_bytes = 0;
        Ok(())
    }

    pub(crate) fn discard(&mut self) {
        self.buffers.clear();
        self.local_bytes = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fetchpath_storage::NoFaults;

    #[test]
    fn only_strong_quoted_etags_authorize_resume() {
        assert_eq!(strong_etag(Some("\"v1\"")), Some("\"v1\"".into()));
        assert_eq!(strong_etag(Some("W/\"v1\"")), None);
        assert_eq!(strong_etag(Some("v1")), None);
        assert_eq!(strong_etag(None), None);
    }

    #[test]
    fn parses_satisfiable_content_ranges() {
        assert_eq!(
            parse_content_range("bytes 10-19/100"),
            Some(ContentRange {
                start: 10,
                end: 19,
                total: Some(100)
            })
        );
        assert_eq!(parse_content_range("bytes 10-9/100"), None);
        assert_eq!(parse_content_range("bytes 10-100/100"), None);
    }

    #[test]
    fn persistent_source_keys_exclude_query_secrets() {
        assert_eq!(
            source_key("https://example.test/file?token=secret-one"),
            source_key("https://example.test/file?token=secret-two")
        );
        assert_ne!(
            source_key("https://example.test/file"),
            source_key("https://example.test/other")
        );
        assert_ne!(
            source_key_with_context("https://example.test/file", "context-a"),
            source_key_with_context("https://example.test/file", "context-b")
        );
    }

    fn scratch_store(label: &str) -> (std::path::PathBuf, CheckpointStore, File) {
        let dir = std::env::temp_dir().join(format!(
            "fetchpath-prefix-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let store = CheckpointStore::new(&dir.join("file.bin"), "key").unwrap();
        let file = store.open_staging().unwrap();
        (dir, store, file)
    }

    fn deliver(tracker: &mut PrefixTracker, file: &File, body: &[u8], start: usize, end: usize) {
        fetchpath_storage::write_all_at(file, start as u64, &body[start..end]).unwrap();
        tracker.record(start as u64, end - start).unwrap();
    }

    #[test]
    fn background_hasher_follows_out_of_order_extents_without_callback_readback() {
        let body: Vec<u8> = (0..300_000_u32).map(|value| (value % 253) as u8).collect();
        let (dir, store, file) = scratch_store("digest");
        let mut tracker = PrefixTracker::new(0);
        let queue = CommitQueue::new(0, 0);
        std::thread::scope(|scope| {
            let worker = scope.spawn(|| queue.run(&store, 0, Sha256::new(), Vec::new(), &NoFaults));
            deliver(&mut tracker, &file, &body, 200_000, 300_000);
            assert_eq!(tracker.prefix(), 0);
            deliver(&mut tracker, &file, &body, 100_000, 200_000);
            assert_eq!(tracker.prefix(), 0);
            deliver(&mut tracker, &file, &body, 0, 40_000);
            assert_eq!(tracker.prefix(), 40_000);
            queue.advance_prefix(tracker.prefix());
            deliver(&mut tracker, &file, &body, 40_000, 100_000);
            assert_eq!(tracker.prefix(), 300_000);
            assert_eq!(tracker.written(), 300_000);
            queue.finish(tracker.prefix());
            worker.join().unwrap();
        });
        assert_eq!(queue.hashed(), 300_000);
        assert_eq!(
            queue.take_final_digest().unwrap(),
            format!("{:x}", Sha256::digest(&body))
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn background_hasher_resumes_from_a_committed_prefix() {
        let body: Vec<u8> = (0..50_000_u32).map(|value| (value % 7) as u8).collect();
        let (dir, store, file) = scratch_store("resume");
        fetchpath_storage::write_all_at(&file, 0, &body[..20_000]).unwrap();
        let mut hasher = Sha256::new();
        hasher.update(&body[..20_000]);
        let mut tracker = PrefixTracker::new(20_000);
        assert_eq!(tracker.prefix(), 20_000);
        let queue = CommitQueue::new(20_000, 20_000);
        std::thread::scope(|scope| {
            let worker = scope.spawn(|| queue.run(&store, 20_000, hasher, Vec::new(), &NoFaults));
            deliver(&mut tracker, &file, &body, 30_000, 50_000);
            assert_eq!(tracker.prefix(), 20_000);
            deliver(&mut tracker, &file, &body, 20_000, 30_000);
            queue.finish(tracker.prefix());
            worker.join().unwrap();
        });
        assert_eq!(
            queue.take_final_digest().unwrap(),
            format!("{:x}", Sha256::digest(&body))
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_byte_delivered_twice_is_refused() {
        let mut tracker = PrefixTracker::new(0);
        tracker.record(0, 500).unwrap();
        for (start, end) in [(0_u64, 500_u64), (400, 600), (100, 200)] {
            let error = tracker.record(start, (end - start) as usize).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        }
        assert_eq!(tracker.written(), 500, "a refused write records nothing");
    }

    #[test]
    fn the_commit_thread_keeps_only_the_newest_queued_checkpoint() {
        let (dir, store, file) = scratch_store("coalesce");
        let body = [9_u8; 30];
        fetchpath_storage::write_all_at(&file, 0, &body).unwrap();
        let queue = CommitQueue::new(0, 0);
        std::thread::scope(|scope| {
            let worker = scope.spawn(|| queue.run(&store, 0, Sha256::new(), Vec::new(), &NoFaults));
            for len in [10_u64, 20, 30] {
                queue.submit_commit(CommitRequest {
                    prefix: len,
                    extents: Vec::new(),
                    source_key: "key".into(),
                    strong_etag: Some("\"v1\"".into()),
                    expected_total: Some(100),
                    persist_small_extents: false,
                });
            }
            queue.close();
            worker.join().unwrap();
        });
        // Whatever was coalesced, the newest checkpoint is the one on disk.
        assert_eq!(queue.committed(), 30);
        let latest = store.latest().unwrap().unwrap();
        assert_eq!(latest.committed_len, 30);
        assert_eq!(latest.local_sha256, format!("{:x}", Sha256::digest(body)));
        assert!(!queue.failed());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_failed_commit_stops_the_thread_and_is_reported() {
        struct Broken;
        impl FaultInjector for Broken {
            fn check(&self, point: fetchpath_storage::FaultPoint) -> io::Result<()> {
                if point == fetchpath_storage::FaultPoint::PayloadFlush {
                    return Err(io::Error::other("flush failed"));
                }
                Ok(())
            }
        }
        let (dir, store, file) = scratch_store("fail");
        fetchpath_storage::write_all_at(&file, 0, &[1_u8; 10]).unwrap();
        let queue = CommitQueue::new(0, 0);
        std::thread::scope(|scope| {
            let worker = scope.spawn(|| queue.run(&store, 0, Sha256::new(), Vec::new(), &Broken));
            queue.submit_commit(CommitRequest {
                prefix: 10,
                extents: Vec::new(),
                source_key: "key".into(),
                strong_etag: Some("\"v1\"".into()),
                expected_total: Some(100),
                persist_small_extents: false,
            });
            worker.join().unwrap();
        });
        assert!(queue.failed());
        assert_eq!(queue.committed(), 0);
        assert!(queue.take_error().is_some());
        assert!(store.latest().unwrap().is_none(), "nothing was committed");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn commit_queue_persists_reuses_and_folds_out_of_order_extents() {
        let body: Vec<u8> = (0..400_000_u32).map(|value| (value % 251) as u8).collect();
        let (dir, store, file) = scratch_store("v2-extents");
        let mut tracker = PrefixTracker::new(0);
        let queue = CommitQueue::new(0, 0);
        std::thread::scope(|scope| {
            let worker = scope.spawn(|| queue.run(&store, 0, Sha256::new(), Vec::new(), &NoFaults));
            deliver(&mut tracker, &file, &body, 0, 100_000);
            deliver(&mut tracker, &file, &body, 200_000, 300_000);
            assert_eq!(tracker.non_prefix_extents(), vec![(200_000, 300_000)]);
            queue.submit_commit(CommitRequest {
                prefix: tracker.prefix(),
                extents: tracker.non_prefix_extents(),
                source_key: "key".into(),
                strong_etag: Some("\"v1\"".into()),
                expected_total: Some(body.len() as u64),
                persist_small_extents: false,
            });
            while queue.committed_bytes() < 200_000 {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            let first = store.latest().unwrap().unwrap();
            assert_eq!(first.committed_len, 100_000);
            assert_eq!(first.extents.len(), 1);
            assert_eq!(
                (first.extents[0].start, first.extents[0].end),
                (200_000, 300_000)
            );
            assert!(store.validate_staging_state(&first).unwrap().is_some());

            // Extend the out-of-order range: [200k, 300k) is reused, [300k, 400k) hashed once.
            deliver(&mut tracker, &file, &body, 300_000, 400_000);
            queue.submit_commit(CommitRequest {
                prefix: tracker.prefix(),
                extents: tracker.non_prefix_extents(),
                source_key: "key".into(),
                strong_etag: Some("\"v1\"".into()),
                expected_total: Some(body.len() as u64),
                persist_small_extents: false,
            });
            while queue.committed_bytes() < 300_000 {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            let second = store.latest().unwrap().unwrap();
            assert_eq!(second.committed_len, 100_000);
            assert_eq!(second.extents.len(), 2);
            assert_eq!(second.retained_bytes(), 300_000);
            assert!(store.validate_staging_state(&second).unwrap().is_some());

            // Fill the missing gap [100k, 200k): prefix jumps to 400k and folds all extents.
            deliver(&mut tracker, &file, &body, 100_000, 200_000);
            assert_eq!(tracker.prefix(), 400_000);
            assert!(tracker.non_prefix_extents().is_empty());
            queue.finish(tracker.prefix());
            worker.join().unwrap();
        });
        assert!(!queue.failed());
        assert_eq!(
            queue.take_final_digest().unwrap(),
            format!("{:x}", Sha256::digest(&body))
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn write_pipeline_coalesces_writes_and_bounds_memory_under_slow_disk_backpressure() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct SlowDisk {
            writes: AtomicUsize,
        }
        impl FaultInjector for SlowDisk {
            fn check(&self, point: fetchpath_storage::FaultPoint) -> io::Result<()> {
                if point == fetchpath_storage::FaultPoint::PayloadWrite {
                    self.writes.fetch_add(1, Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(15));
                }
                Ok(())
            }
        }

        let body: Vec<u8> = (0..(1024 * 1024_u32))
            .map(|value| (value % 251) as u8)
            .collect();
        let (dir, store, file) = scratch_store("coalesce-backpressure");
        let faults = SlowDisk {
            writes: AtomicUsize::new(0),
        };
        let commit_queue = CommitQueue::new(0, 0);
        let max_bytes = 384 * 1024;
        let flush_bytes = 128 * 1024;
        let write_queue = WriteQueue::new(0, &[], max_bytes);
        let identity = Mutex::new((Some("\"v1\"".to_owned()), Some(body.len() as u64)));

        std::thread::scope(|scope| {
            let commit_worker =
                scope.spawn(|| commit_queue.run(&store, 0, Sha256::new(), Vec::new(), &faults));
            let write_worker = scope.spawn(|| {
                write_queue.run(
                    &store,
                    &file,
                    &commit_queue,
                    &identity,
                    "key".into(),
                    256 * 1024,
                    64 * 1024,
                    Duration::from_secs(60),
                    &faults,
                )
            });

            let mut coalescer = LaneCoalescer::new(&write_queue, flush_bytes);
            // Interleave 4 lanes delivering 16 KiB chunks (64 callbacks total).
            let quarter = body.len() / 4;
            let chunk = 16 * 1024;
            for step in 0..(quarter / chunk) {
                for lane in 0..4 {
                    let offset = lane * quarter + step * chunk;
                    coalescer
                        .enqueue(offset as u64, &body[offset..offset + chunk], &|| false)
                        .unwrap();
                }
            }
            coalescer.flush_all(&|| false).unwrap();
            write_queue.close();
            write_worker.join().unwrap();
            let (prefix, written, _) = write_queue.on_disk_snapshot();
            assert_eq!(prefix, body.len() as u64);
            assert_eq!(written, body.len() as u64);
            commit_queue.finish(prefix);
            commit_worker.join().unwrap();
        });

        let write_calls = faults.writes.load(Ordering::SeqCst);
        let (peak_bytes, backpressure_waits) = write_queue.stats();
        assert!(
            write_calls <= 16,
            "64x 16 KiB callbacks should coalesce into <=16 disk writes, got {write_calls}"
        );
        assert!(
            peak_bytes <= max_bytes,
            "buffered bytes {peak_bytes} exceeded configured cap {max_bytes}"
        );
        assert!(
            backpressure_waits > 0,
            "slow disk must exert backpressure on fast producer"
        );
        assert!(!write_queue.failed());
        assert!(!commit_queue.failed());
        assert_eq!(std::fs::read(store.staging()).unwrap(), body);
        assert_eq!(
            commit_queue.take_final_digest().unwrap(),
            format!("{:x}", Sha256::digest(&body))
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn write_pipeline_cancel_unblocks_backpressured_producer_and_quiesces_cleanly() {
        use std::sync::atomic::{AtomicBool, Ordering};

        struct BlockingDisk {
            started: AtomicBool,
            released: Mutex<bool>,
            cv: Condvar,
        }
        impl FaultInjector for BlockingDisk {
            fn check(&self, point: fetchpath_storage::FaultPoint) -> io::Result<()> {
                if point == fetchpath_storage::FaultPoint::PayloadWrite {
                    self.started.store(true, Ordering::SeqCst);
                    let mut done = self.released.lock().unwrap();
                    while !*done {
                        done = self.cv.wait(done).unwrap();
                    }
                }
                Ok(())
            }
        }

        let (dir, store, file) = scratch_store("cancel-backpressure");
        let faults = BlockingDisk {
            started: AtomicBool::new(false),
            released: Mutex::new(false),
            cv: Condvar::new(),
        };
        let commit_queue = CommitQueue::new(0, 0);
        let write_queue = WriteQueue::new(0, &[], 64 * 1024);
        let identity = Mutex::new((None, None));
        let cancelled = AtomicBool::new(false);

        std::thread::scope(|scope| {
            let write_worker = scope.spawn(|| {
                write_queue.run(
                    &store,
                    &file,
                    &commit_queue,
                    &identity,
                    "key".into(),
                    256 * 1024,
                    64 * 1024,
                    Duration::from_secs(60),
                    &faults,
                )
            });
            // First batch enters write_payload_at and blocks inside BlockingDisk.
            write_queue
                .push(0, vec![1_u8; 64 * 1024], 0, &|| false)
                .unwrap();
            while !faults.started.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(2));
            }
            // Cancel from another thread shortly after the second batch blocks on backpressure
            // inside LaneCoalescer; the unpushed batch must be retained (not dropped).
            let mut coalescer = LaneCoalescer::new(&write_queue, 64 * 1024);
            let canceller = scope.spawn(|| {
                std::thread::sleep(Duration::from_millis(40));
                cancelled.store(true, Ordering::SeqCst);
            });
            let started_wait = Instant::now();
            let err = coalescer
                .enqueue(64 * 1024, &[2_u8; 64 * 1024], &|| {
                    cancelled.load(Ordering::SeqCst)
                })
                .unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::Interrupted);
            assert!(
                started_wait.elapsed() < Duration::from_secs(2),
                "backpressured producer must wake promptly on cancel"
            );
            canceller.join().unwrap();

            // Release the disk and flush with `&|| false` (simulating RetainStaging pause):
            // the interrupted batch is still in the coalescer and reaches disk intact.
            *faults.released.lock().unwrap() = true;
            faults.cv.notify_all();
            coalescer.flush_all(&|| false).unwrap();
            write_queue.close();
            write_worker.join().unwrap();
        });

        let (prefix, written, _) = write_queue.on_disk_snapshot();
        assert_eq!(prefix, 128 * 1024);
        assert_eq!(
            written,
            128 * 1024,
            "RetainStaging flush preserves interrupted batch"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn write_pipeline_propagates_storage_full_error() {
        struct DiskFull;
        impl FaultInjector for DiskFull {
            fn check(&self, point: fetchpath_storage::FaultPoint) -> io::Result<()> {
                if point == fetchpath_storage::FaultPoint::PayloadWrite {
                    return Err(io::Error::new(io::ErrorKind::StorageFull, "disk full"));
                }
                Ok(())
            }
        }

        let (dir, store, file) = scratch_store("disk-full");
        let commit_queue = CommitQueue::new(0, 0);
        let write_queue = WriteQueue::new(0, &[], 256 * 1024);
        let identity = Mutex::new((None, None));

        std::thread::scope(|scope| {
            let write_worker = scope.spawn(|| {
                write_queue.run(
                    &store,
                    &file,
                    &commit_queue,
                    &identity,
                    "key".into(),
                    256 * 1024,
                    64 * 1024,
                    Duration::from_secs(60),
                    &DiskFull,
                )
            });
            let _ = write_queue.push(0, vec![7_u8; 32 * 1024], 0, &|| false);
            write_queue.close();
            write_worker.join().unwrap();
        });

        assert!(write_queue.failed());
        assert_eq!(
            write_queue.take_error().unwrap().kind(),
            io::ErrorKind::StorageFull
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
}
