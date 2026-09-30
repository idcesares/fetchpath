use fetchpath_storage::{CheckpointRecord, CheckpointStore, FaultInjector};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::File;
use std::io;
use std::sync::{Condvar, Mutex};

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

/// Which bytes of the staging file have been written this attempt, and the
/// SHA-256 of the contiguous prefix of them.
///
/// The set starts as `[0, committed_len)`. Completion is never inferred from
/// the file's length: after an in-process retry, stale bytes of the same
/// identity can sit past the prefix. Lanes deliver bytes out of order. When
/// the prefix advances over bytes that arrived early, they are read back from
/// staging to extend the digest; bytes that land exactly at the prefix are
/// hashed from the buffer.
pub(crate) struct PrefixTracker {
    /// Merged completed extents, `start -> end` (end exclusive).
    done: BTreeMap<u64, u64>,
    hasher: Sha256,
    /// Length of the prefix, which is also how many bytes the hasher has seen.
    hashed: u64,
    /// Bytes recorded in all, prefix included.
    total: u64,
}

impl PrefixTracker {
    /// `hasher` must already hold the digest state of `[0, committed)`.
    pub(crate) fn new(committed: u64, hasher: Sha256) -> Self {
        let mut done = BTreeMap::new();
        if committed > 0 {
            done.insert(0, committed);
        }
        Self {
            done,
            hasher,
            hashed: committed,
            total: committed,
        }
    }

    /// The first byte not yet written.
    pub(crate) fn prefix(&self) -> u64 {
        self.hashed
    }

    /// Bytes written so far, in or beyond the prefix.
    pub(crate) fn written(&self) -> u64 {
        self.total
    }

    /// Hex SHA-256 of the prefix.
    pub(crate) fn digest(&self) -> String {
        format!("{:x}", self.hasher.clone().finalize())
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

    /// Records that `bytes` are now on disk at `offset`. Call it only after
    /// the write returned, and after [`Self::check`] passed.
    pub(crate) fn record(&mut self, file: &File, offset: u64, bytes: &[u8]) -> io::Result<()> {
        if bytes.is_empty() {
            return Ok(());
        }
        self.check(offset, bytes.len())?;
        let end = offset + bytes.len() as u64;
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
        self.total += bytes.len() as u64;

        let prefix = self.done.get(&0).copied().unwrap_or(0);
        if prefix > self.hashed {
            let mut at = self.hashed;
            if offset <= at && end > at {
                let take_end = end.min(prefix);
                self.hasher
                    .update(&bytes[(at - offset) as usize..(take_end - offset) as usize]);
                at = take_end;
            }
            let mut buffer = Vec::new();
            while at < prefix {
                let count = (prefix - at).min(1024 * 1024) as usize;
                buffer.resize(count, 0);
                fetchpath_storage::read_exact_at(file, at, &mut buffer)?;
                self.hasher.update(&buffer);
                at += count as u64;
            }
            self.hashed = prefix;
        }
        Ok(())
    }
}

struct QueueState {
    pending: Option<CheckpointRecord>,
    closed: bool,
    error: Option<io::Error>,
    /// Prefix length of the last checkpoint that reached disk.
    committed: u64,
}

/// Hands checkpoints to a thread that fsyncs the payload and commits the
/// record, so `FlushFileBuffers` never stalls the socket loop. A newer
/// checkpoint replaces one that has not started yet: only the latest prefix
/// matters.
pub(crate) struct CommitQueue {
    state: Mutex<QueueState>,
    changed: Condvar,
}

impl CommitQueue {
    pub(crate) fn new(committed: u64) -> Self {
        Self {
            state: Mutex::new(QueueState {
                pending: None,
                closed: false,
                error: None,
                committed,
            }),
            changed: Condvar::new(),
        }
    }

    /// Queues a checkpoint without waiting.
    pub(crate) fn submit(&self, record: CheckpointRecord) {
        let mut state = self.state.lock().expect("commit queue poisoned");
        if !state.closed {
            state.pending = Some(record);
            self.changed.notify_all();
        }
    }

    /// True once the thread has failed; the sink then stops the transfer.
    pub(crate) fn failed(&self) -> bool {
        self.state
            .lock()
            .expect("commit queue poisoned")
            .error
            .is_some()
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

    /// Asks the thread to finish what it holds and exit.
    pub(crate) fn close(&self) {
        let mut state = self.state.lock().expect("commit queue poisoned");
        state.closed = true;
        self.changed.notify_all();
    }

    /// The thread's body. Returns after `close`, having committed whatever
    /// was still queued.
    pub(crate) fn run(&self, store: &CheckpointStore, faults: &dyn FaultInjector) {
        loop {
            let record = {
                let mut state = self.state.lock().expect("commit queue poisoned");
                loop {
                    if let Some(record) = state.pending.take() {
                        break record;
                    }
                    if state.closed {
                        return;
                    }
                    state = self.changed.wait(state).expect("commit queue poisoned");
                }
            };
            let len = record.committed_len;
            let result = store
                .sync_payload_reopened(faults)
                .and_then(|()| store.commit(record, faults).map(|_| ()));
            let mut state = self.state.lock().expect("commit queue poisoned");
            match result {
                Ok(()) => state.committed = state.committed.max(len),
                Err(error) => {
                    state.error = Some(error);
                    state.closed = true;
                    state.pending = None;
                    return;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    fn scratch_file(label: &str) -> (std::path::PathBuf, File) {
        let path = std::env::temp_dir().join(format!(
            "fetchpath-prefix-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let file = std::fs::OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        (path, file)
    }

    fn deliver(tracker: &mut PrefixTracker, file: &File, body: &[u8], start: usize, end: usize) {
        fetchpath_storage::write_all_at(file, start as u64, &body[start..end]).unwrap();
        tracker
            .record(file, start as u64, &body[start..end])
            .unwrap();
    }

    #[test]
    fn the_prefix_digest_follows_out_of_order_bytes_by_reading_them_back() {
        let body: Vec<u8> = (0..300_000_u32).map(|value| (value % 253) as u8).collect();
        let (path, file) = scratch_file("digest");
        let mut tracker = PrefixTracker::new(0, Sha256::new());
        // Lanes finish in a different order than the file runs.
        deliver(&mut tracker, &file, &body, 200_000, 300_000);
        assert_eq!(tracker.prefix(), 0);
        deliver(&mut tracker, &file, &body, 100_000, 200_000);
        assert_eq!(tracker.prefix(), 0);
        deliver(&mut tracker, &file, &body, 0, 40_000);
        assert_eq!(tracker.prefix(), 40_000);
        // The piece that bridges to the early ones hashes its own bytes from
        // the buffer and reads the rest back.
        deliver(&mut tracker, &file, &body, 40_000, 100_000);
        assert_eq!(tracker.prefix(), 300_000);
        assert_eq!(tracker.written(), 300_000);
        assert_eq!(tracker.digest(), format!("{:x}", Sha256::digest(&body)));
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn a_tracker_resumes_from_a_committed_prefix() {
        let body: Vec<u8> = (0..50_000_u32).map(|value| (value % 7) as u8).collect();
        let (path, file) = scratch_file("resume");
        fetchpath_storage::write_all_at(&file, 0, &body[..20_000]).unwrap();
        let mut hasher = Sha256::new();
        hasher.update(&body[..20_000]);
        let mut tracker = PrefixTracker::new(20_000, hasher);
        assert_eq!(tracker.prefix(), 20_000);
        deliver(&mut tracker, &file, &body, 30_000, 50_000);
        assert_eq!(tracker.prefix(), 20_000);
        deliver(&mut tracker, &file, &body, 20_000, 30_000);
        assert_eq!(tracker.digest(), format!("{:x}", Sha256::digest(&body)));
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn a_byte_delivered_twice_is_refused() {
        let body = vec![7_u8; 1000];
        let (path, file) = scratch_file("overlap");
        let mut tracker = PrefixTracker::new(0, Sha256::new());
        deliver(&mut tracker, &file, &body, 0, 500);
        for (start, end) in [(0, 500), (400, 600), (100, 200)] {
            let error = tracker
                .record(&file, start as u64, &body[start..end])
                .unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        }
        assert_eq!(tracker.written(), 500, "a refused write records nothing");
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn the_commit_thread_keeps_only_the_newest_queued_checkpoint() {
        use fetchpath_storage::NoFaults;
        let dir = std::env::temp_dir().join(format!(
            "fetchpath-queue-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let destination = dir.join("file.bin");
        let store = CheckpointStore::new(&destination, "key").unwrap();
        drop(store.open_staging().unwrap());
        let queue = CommitQueue::new(0);
        std::thread::scope(|scope| {
            let worker = scope.spawn(|| queue.run(&store, &NoFaults));
            for len in [10_u64, 20, 30] {
                queue.submit(CheckpointRecord::downloading(
                    "key".into(),
                    len,
                    "0".repeat(64),
                    Some("\"v1\"".into()),
                    Some(100),
                ));
            }
            queue.close();
            worker.join().unwrap();
        });
        // Whatever was coalesced, the newest checkpoint is the one on disk.
        assert_eq!(queue.committed(), 30);
        assert_eq!(store.latest().unwrap().unwrap().committed_len, 30);
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
        let dir = std::env::temp_dir().join(format!(
            "fetchpath-queue-fail-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let store = CheckpointStore::new(&dir.join("file.bin"), "key").unwrap();
        drop(store.open_staging().unwrap());
        let queue = CommitQueue::new(0);
        std::thread::scope(|scope| {
            let worker = scope.spawn(|| queue.run(&store, &Broken));
            queue.submit(CheckpointRecord::downloading(
                "key".into(),
                10,
                "0".repeat(64),
                Some("\"v1\"".into()),
                Some(100),
            ));
            worker.join().unwrap();
        });
        assert!(queue.failed());
        assert_eq!(queue.committed(), 0);
        assert!(queue.take_error().is_some());
        assert!(store.latest().unwrap().is_none(), "nothing was committed");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
