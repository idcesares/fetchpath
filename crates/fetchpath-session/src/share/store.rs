//! The ticket store.
//!
//! A ticket is a random id in the link's path and a separate random key in
//! its fragment. The listener sees the id; the key never leaves the process
//! except in the link given to the person once, at creation.
//!
//! A file share holds the opened file for as long as the ticket lives, with
//! writers and deleters locked out by the system (Windows share mode). That
//! is what freezes the content: the framing reuses one key and nonce prefix
//! across Range resumes, so a byte changed between two responses would
//! encrypt two plaintexts under one nonce. A size and modified-time re-check
//! on every read is defense in depth, not the mechanism.

use super::frame::{self, Frame, Layout};
use std::fs::File;
use std::io;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant, SystemTime};
use zeroize::Zeroizing;

pub const DEFAULT_EXPIRY: Duration = Duration::from_secs(24 * 60 * 60);
pub const MAX_EXPIRY: Duration = Duration::from_secs(30 * 24 * 60 * 60);
/// The most one `read` returns, so a response is written in bounded pieces.
pub const MAX_READ: usize = 256 * 1024;
/// Bytes per second one ticket may be served, averaged over its life.
const RATE: u64 = 32 * 1024 * 1024;
/// A ticket may serve this much beyond one full transfer: the bytes a broken
/// connection or a resume sends twice.
const BUDGET_SLACK: u64 = 4 * 1024 * 1024;
const MAX_LIVE: usize = 16;
/// Ended tickets stay listed this long, then are forgotten.
const KEEP_ENDED: Duration = Duration::from_secs(60 * 60);
const MAX_EVENTS: usize = 64;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ShareKind {
    /// A file the person chose.
    File,
    /// The engine's own file, for the setup test. Served while sharing is off.
    Test,
}

pub enum Source {
    /// Opened by the caller with writers and deleters locked out.
    File(File),
    Memory(Vec<u8>),
}

pub struct NewShare {
    pub kind: ShareKind,
    pub name: String,
    pub source: Source,
    /// The file's size as hashed; the opened file must still be this long.
    pub size: u64,
    pub expiry: Duration,
}

/// What creation returns, once. `link_path` is `/s/<id>#<key>`.
pub struct Created {
    pub handle: String,
    pub link_path: String,
    pub expires_in: Duration,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Phase {
    Live,
    Completed,
    Revoked,
    Expired,
    /// The file changed, or could no longer be read, while the link was live.
    Changed,
    /// More bytes were requested than a normal download needs.
    OverBudget,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ShareError {
    /// Sharing is off.
    Off,
    /// The request's Host is not the base URL's.
    WrongHost,
    Unknown,
    /// Revoked, expired, used up, changed or over budget.
    Gone,
    /// The requested range is outside the stream.
    Range,
    TooMany,
    BadName,
    Unavailable,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ShareEventKind {
    Created,
    Opened,
    Completed,
    Revoked,
    Expired,
    Changed,
    OverBudget,
}

/// For the audit log: which link, what happened. Never the id or the key.
#[derive(Clone, Debug)]
pub struct ShareEvent {
    pub handle: String,
    pub kind: ShareEventKind,
}

#[derive(Clone, Debug)]
pub struct ShareInfo {
    pub handle: String,
    pub kind: ShareKind,
    pub name: String,
    pub size: u64,
    pub phase: Phase,
    pub expires_in: Duration,
    pub opened: u32,
    pub delivered: u64,
    pub cipher_len: u64,
}

pub struct Piece {
    pub bytes: Vec<u8>,
    /// How long the caller waits before asking for more.
    pub wait: Duration,
}

struct State {
    phase: Phase,
    ended: Option<Instant>,
    /// Encrypted bytes handed over contiguously from the start.
    contiguous: u64,
    served: u64,
    first_read: Option<Instant>,
    opened: u32,
}

struct Entry {
    handle: String,
    id: String,
    kind: ShareKind,
    name: String,
    meta: Vec<u8>,
    size: u64,
    fingerprint: Option<(u64, Option<SystemTime>)>,
    // Kept for as long as the ticket lives: dropping it frees the file.
    source: RwLock<Option<Source>>,
    frame: Frame,
    layout: Layout,
    expires: Instant,
    state: Mutex<State>,
}

#[derive(Default)]
struct Config {
    enabled: bool,
    base_host: Option<String>,
}

#[derive(Default)]
struct Shared {
    skew: Mutex<Duration>,
    events: Mutex<Vec<ShareEvent>>,
    /// The base-URL host a test link was last delivered through.
    test_passed: Mutex<Option<String>>,
    config: Mutex<Config>,
}

impl Shared {
    fn now(&self) -> Instant {
        Instant::now() + *self.skew.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn event(&self, handle: &str, kind: ShareEventKind) {
        let mut events = self.events.lock().unwrap_or_else(|p| p.into_inner());
        if events.len() == MAX_EVENTS {
            events.remove(0);
        }
        events.push(ShareEvent {
            handle: handle.to_owned(),
            kind,
        });
    }
}

#[derive(Default)]
pub struct ShareStore {
    shared: Arc<Shared>,
    entries: Mutex<Vec<Arc<Entry>>>,
}

fn random<const N: usize>() -> Result<[u8; N], ShareError> {
    let mut bytes = [0_u8; N];
    getrandom::fill(&mut bytes).map_err(|_| ShareError::Unavailable)?;
    Ok(bytes)
}

fn base64url(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for group in bytes.chunks(3) {
        let n = group
            .iter()
            .enumerate()
            .fold(0_u32, |n, (i, byte)| n | u32::from(*byte) << (16 - 8 * i));
        for i in 0..=group.len() {
            out.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
        }
    }
    out
}

fn fingerprint(file: &File) -> io::Result<(u64, Option<SystemTime>)> {
    let meta = file.metadata()?;
    Ok((meta.len(), meta.modified().ok()))
}

#[cfg(windows)]
fn read_at(file: &File, buf: &mut [u8], offset: u64) -> io::Result<usize> {
    std::os::windows::fs::FileExt::seek_read(file, buf, offset)
}

#[cfg(unix)]
fn read_at(file: &File, buf: &mut [u8], offset: u64) -> io::Result<usize> {
    std::os::unix::fs::FileExt::read_at(file, buf, offset)
}

fn read_exact_at(file: &File, mut buf: &mut [u8], mut offset: u64) -> io::Result<()> {
    while !buf.is_empty() {
        match read_at(file, buf, offset)? {
            0 => return Err(io::ErrorKind::UnexpectedEof.into()),
            read => {
                buf = &mut buf[read..];
                offset += read as u64;
            }
        }
    }
    Ok(())
}

impl ShareStore {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Whether file links are served, and the one host they are served for.
    pub fn configure(&self, enabled: bool, base_host: Option<String>) {
        let mut config = self.shared.config.lock().unwrap_or_else(|p| p.into_inner());
        config.enabled = enabled;
        config.base_host = base_host.map(|host| host.to_ascii_lowercase());
    }

    /// The base-URL host a test link was delivered through, if one was.
    pub fn test_passed(&self) -> Option<String> {
        self.shared
            .test_passed
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    /// Forgets a passed test and ends any test link still live, so only the
    /// newest test link can pass.
    pub fn clear_test(&self) {
        let entries = self.entries.lock().unwrap_or_else(|p| p.into_inner());
        for entry in entries.iter().filter(|entry| entry.kind == ShareKind::Test) {
            entry.end(&self.shared, Phase::Revoked);
        }
        drop(entries);
        *self
            .shared
            .test_passed
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = None;
    }

    pub fn create(&self, new: NewShare) -> Result<Created, ShareError> {
        if new.name.is_empty() || new.name.len() > usize::from(u16::MAX) {
            return Err(ShareError::BadName);
        }
        let meta = frame::meta(&new.name);
        let layout = Layout::new(meta.len() as u64 + new.size, frame::CHUNK_LOG2)
            .ok_or(ShareError::Unavailable)?;
        let fingerprint = match &new.source {
            Source::File(file) => {
                let frozen = fingerprint(file).map_err(|_| ShareError::Unavailable)?;
                // The caller hashed this many bytes; the open file must be
                // the same one.
                if frozen.0 != new.size {
                    return Err(ShareError::Unavailable);
                }
                Some(frozen)
            }
            Source::Memory(_) => None,
        };
        let key = Zeroizing::new(random::<32>()?);
        let frame =
            Frame::new(&key, random::<16>()?, frame::CHUNK_LOG2).ok_or(ShareError::Unavailable)?;
        let id = base64url(&random::<32>()?);
        let handle = base64url(&random::<6>()?);
        let link_path = format!("/s/{id}#{}", base64url(&*key));
        let expiry = new.expiry.min(MAX_EXPIRY);
        let now = self.shared.now();

        let mut entries = self.entries.lock().unwrap_or_else(|p| p.into_inner());
        self.sweep_locked(&mut entries);
        let live = entries
            .iter()
            .filter(|entry| entry.phase() == Phase::Live)
            .count();
        if live >= MAX_LIVE {
            return Err(ShareError::TooMany);
        }
        entries.push(Arc::new(Entry {
            handle: handle.clone(),
            id,
            kind: new.kind,
            name: new.name,
            meta,
            size: new.size,
            fingerprint,
            source: RwLock::new(Some(new.source)),
            frame,
            layout,
            expires: now + expiry,
            state: Mutex::new(State {
                phase: Phase::Live,
                ended: None,
                contiguous: 0,
                served: 0,
                first_read: None,
                opened: 0,
            }),
        }));
        self.shared.event(&handle, ShareEventKind::Created);
        Ok(Created {
            handle,
            link_path,
            expires_in: expiry,
        })
    }

    pub fn list(&self) -> Vec<ShareInfo> {
        let mut entries = self.entries.lock().unwrap_or_else(|p| p.into_inner());
        self.sweep_locked(&mut entries);
        let now = self.shared.now();
        entries
            .iter()
            .map(|entry| {
                let state = entry.state.lock().unwrap_or_else(|p| p.into_inner());
                ShareInfo {
                    handle: entry.handle.clone(),
                    kind: entry.kind,
                    name: entry.name.clone(),
                    size: entry.size,
                    phase: state.phase,
                    expires_in: entry.expires.saturating_duration_since(now),
                    opened: state.opened,
                    delivered: state.contiguous,
                    cipher_len: entry.layout.cipher_len(),
                }
            })
            .collect()
    }

    /// Ends the link now, releasing its file. `false` for an unknown or
    /// already ended one.
    pub fn revoke(&self, handle: &str) -> bool {
        let entries = self.entries.lock().unwrap_or_else(|p| p.into_inner());
        entries
            .iter()
            .find(|entry| entry.handle == handle)
            .is_some_and(|entry| entry.end(&self.shared, Phase::Revoked))
    }

    pub fn revoke_all(&self) {
        let entries = self.entries.lock().unwrap_or_else(|p| p.into_inner());
        for entry in entries.iter() {
            entry.end(&self.shared, Phase::Revoked);
        }
    }

    /// True while a link can still be used, so the engine stays up for it.
    pub fn has_live(&self) -> bool {
        let mut entries = self.entries.lock().unwrap_or_else(|p| p.into_inner());
        self.sweep_locked(&mut entries);
        entries.iter().any(|entry| entry.phase() == Phase::Live)
    }

    /// The ids of the links that can still be fetched, for a relay to route.
    /// An id is a bearer capability for a single-use ticket: never log it.
    pub fn live_ids(&self) -> Vec<String> {
        let mut entries = self.entries.lock().unwrap_or_else(|p| p.into_inner());
        self.sweep_locked(&mut entries);
        entries
            .iter()
            .filter(|entry| entry.phase() == Phase::Live)
            .map(|entry| entry.id.clone())
            .collect()
    }

    pub fn drain_events(&self) -> Vec<ShareEvent> {
        std::mem::take(&mut *self.shared.events.lock().unwrap_or_else(|p| p.into_inner()))
    }

    /// Moves the store's clock forward. For tests only.
    #[doc(hidden)]
    pub fn advance(&self, by: Duration) {
        *self.shared.skew.lock().unwrap_or_else(|p| p.into_inner()) += by;
    }

    fn sweep_locked(&self, entries: &mut Vec<Arc<Entry>>) {
        let now = self.shared.now();
        for entry in entries.iter() {
            if now >= entry.expires {
                entry.end(&self.shared, Phase::Expired);
            }
        }
        entries.retain(|entry| {
            let state = entry.state.lock().unwrap_or_else(|p| p.into_inner());
            state
                .ended
                .is_none_or(|ended| now.saturating_duration_since(ended) < KEEP_ENDED)
        });
    }

    /// Expires what is due. Called on the engine's tick.
    pub fn sweep(&self) {
        let mut entries = self.entries.lock().unwrap_or_else(|p| p.into_inner());
        self.sweep_locked(&mut entries);
    }

    fn find(&self, id: &str, host: &str, counts_open: bool) -> Result<Arc<Entry>, ShareError> {
        {
            let config = self.shared.config.lock().unwrap_or_else(|p| p.into_inner());
            if config.base_host.as_deref() != Some(&host.to_ascii_lowercase()) {
                return Err(ShareError::WrongHost);
            }
        }
        let mut entries = self.entries.lock().unwrap_or_else(|p| p.into_inner());
        self.sweep_locked(&mut entries);
        let entry = entries
            .iter()
            .find(|entry| entry.id == id)
            .cloned()
            .ok_or(ShareError::Unknown)?;
        drop(entries);
        let enabled = self
            .shared
            .config
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .enabled;
        if entry.kind == ShareKind::File && !enabled {
            return Err(ShareError::Off);
        }
        let mut state = entry.state.lock().unwrap_or_else(|p| p.into_inner());
        if state.phase != Phase::Live {
            return Err(ShareError::Gone);
        }
        if counts_open {
            state.opened += 1;
        }
        drop(state);
        if counts_open {
            self.shared.event(&entry.handle, ShareEventKind::Opened);
        }
        Ok(entry)
    }

    /// Checks that the page may be shown. Does not use the ticket up.
    pub fn probe(&self, id: &str, host: &str) -> Result<(), ShareError> {
        self.find(id, host, false).map(|_| ())
    }

    /// Starts serving the encrypted stream.
    pub fn open(&self, id: &str, host: &str) -> Result<Transfer, ShareError> {
        let entry = self.find(id, host, true)?;
        Ok(Transfer {
            entry,
            shared: self.shared.clone(),
            host: Some(host.to_ascii_lowercase()),
        })
    }
}

impl Entry {
    fn phase(&self) -> Phase {
        self.state.lock().unwrap_or_else(|p| p.into_inner()).phase
    }

    /// `true` when this call ended a live ticket. Frees the file.
    fn end(&self, shared: &Shared, phase: Phase) -> bool {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        if state.phase != Phase::Live {
            return false;
        }
        state.phase = phase;
        state.ended = Some(shared.now());
        drop(state);
        *self.source.write().unwrap_or_else(|p| p.into_inner()) = None;
        shared.event(
            &self.handle,
            match phase {
                Phase::Completed => ShareEventKind::Completed,
                Phase::Expired => ShareEventKind::Expired,
                Phase::Changed => ShareEventKind::Changed,
                Phase::OverBudget => ShareEventKind::OverBudget,
                Phase::Revoked | Phase::Live => ShareEventKind::Revoked,
            },
        );
        true
    }

    fn read_plain(&self, from: u64, to: u64, out: &mut Vec<u8>) -> io::Result<()> {
        let meta_len = self.meta.len() as u64;
        if from < meta_len {
            out.extend_from_slice(&self.meta[from as usize..to.min(meta_len) as usize]);
        }
        if to > meta_len {
            let (from, to) = (from.max(meta_len) - meta_len, to - meta_len);
            let source = self.source.read().unwrap_or_else(|p| p.into_inner());
            match source.as_ref() {
                None => return Err(io::ErrorKind::NotFound.into()),
                Some(Source::Memory(bytes)) => {
                    out.extend_from_slice(&bytes[from as usize..to as usize]);
                }
                Some(Source::File(file)) => {
                    let at = out.len();
                    out.resize(at + (to - from) as usize, 0);
                    read_exact_at(file, &mut out[at..], from)?;
                }
            }
        }
        Ok(())
    }
}

/// One download of one ticket, as the listener drives it.
pub struct Transfer {
    entry: Arc<Entry>,
    shared: Arc<Shared>,
    /// The base host in force when the download began; a test that finishes
    /// after the base URL changed does not count for the new one.
    host: Option<String>,
}

impl Transfer {
    pub fn cipher_len(&self) -> u64 {
        self.entry.layout.cipher_len()
    }

    /// Up to `len` encrypted bytes from `start`, never more than [`MAX_READ`].
    /// Fails once the ticket is no longer live, its file changed, or it has
    /// served more than a normal download needs.
    pub fn read(&self, start: u64, len: usize) -> Result<Piece, ShareError> {
        let entry = &*self.entry;
        let total = entry.layout.cipher_len();
        if start >= total {
            return Err(ShareError::Range);
        }
        let len = len.min(MAX_READ).min((total - start) as usize);
        let now = self.shared.now();
        let mut state = entry.state.lock().unwrap_or_else(|p| p.into_inner());
        if state.phase != Phase::Live {
            return Err(ShareError::Gone);
        }
        drop(state);
        if now >= entry.expires {
            entry.end(&self.shared, Phase::Expired);
            return Err(ShareError::Gone);
        }
        if let Some(frozen) = entry.fingerprint {
            let held = entry.source.read().unwrap_or_else(|p| p.into_inner());
            let same = match held.as_ref() {
                Some(Source::File(file)) => fingerprint(file).is_ok_and(|now| now == frozen),
                _ => false,
            };
            drop(held);
            if !same {
                entry.end(&self.shared, Phase::Changed);
                return Err(ShareError::Gone);
            }
        }
        state = entry.state.lock().unwrap_or_else(|p| p.into_inner());
        if state.served + len as u64 > total + BUDGET_SLACK {
            drop(state);
            entry.end(&self.shared, Phase::OverBudget);
            return Err(ShareError::Gone);
        }
        state.served += len as u64;
        let first = *state.first_read.get_or_insert(now);
        let owed = Duration::from_secs_f64(state.served as f64 / RATE as f64);
        let wait = owed.saturating_sub(now.saturating_duration_since(first));
        drop(state);

        let end = start + len as u64;
        let mut buf = Vec::new();
        let mut buf_start = 0;
        if start < frame::HEADER_LEN as u64 {
            buf.extend_from_slice(entry.frame.header());
        }
        let mut index = if start < frame::HEADER_LEN as u64 {
            0
        } else {
            let index = entry.layout.chunk_at(start);
            buf_start = entry.layout.cipher_start(index);
            index
        };
        while buf_start + (buf.len() as u64) < end {
            let (from, to) = entry.layout.plain_range(index);
            let mut plain = Vec::with_capacity((to - from) as usize);
            if entry.read_plain(from, to, &mut plain).is_err() {
                entry.end(&self.shared, Phase::Changed);
                return Err(ShareError::Gone);
            }
            buf.extend(
                entry
                    .frame
                    .seal(index as u32, entry.layout.is_last(index), &plain),
            );
            index += 1;
        }
        let from = (start - buf_start) as usize;
        Ok(Piece {
            bytes: buf[from..from + len].to_vec(),
            wait,
        })
    }

    /// Records that bytes `start..end` were written to the recipient. The
    /// ticket is used up when the whole stream has been, in order.
    pub fn delivered(&self, start: u64, end: u64) {
        let entry = &*self.entry;
        let mut state = entry.state.lock().unwrap_or_else(|p| p.into_inner());
        if state.phase != Phase::Live {
            return;
        }
        if start <= state.contiguous && end > state.contiguous {
            state.contiguous = end;
        }
        let complete = state.contiguous >= entry.layout.cipher_len();
        drop(state);
        if complete && entry.end(&self.shared, Phase::Completed) && entry.kind == ShareKind::Test {
            *self
                .shared
                .test_passed
                .lock()
                .unwrap_or_else(|p| p.into_inner()) = self.host.clone();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOST: &str = "share.example.test";

    fn store(enabled: bool) -> Arc<ShareStore> {
        let store = ShareStore::new();
        store.configure(enabled, Some(HOST.to_owned()));
        store
    }

    fn memory(kind: ShareKind, bytes: Vec<u8>) -> NewShare {
        NewShare {
            kind,
            name: "note.txt".to_owned(),
            size: bytes.len() as u64,
            source: Source::Memory(bytes),
            expiry: DEFAULT_EXPIRY,
        }
    }

    fn id_of(created: &Created) -> String {
        let path = created.link_path.strip_prefix("/s/").unwrap();
        path.split('#').next().unwrap().to_owned()
    }

    /// Reads the whole stream the way a client does, in order.
    fn download(store: &ShareStore, id: &str) -> Result<Vec<u8>, ShareError> {
        let transfer = store.open(id, HOST)?;
        let mut out = Vec::new();
        while (out.len() as u64) < transfer.cipher_len() {
            let piece = transfer.read(out.len() as u64, MAX_READ)?;
            transfer.delivered(out.len() as u64, (out.len() + piece.bytes.len()) as u64);
            out.extend(piece.bytes);
        }
        Ok(out)
    }

    #[test]
    fn a_link_is_used_up_by_one_complete_download() {
        let store = store(true);
        let created = store
            .create(memory(ShareKind::File, vec![1; 70_000]))
            .unwrap();
        let id = id_of(&created);
        assert_eq!(
            download(&store, &id).map(|v| v.len()),
            Ok(22 + 2 + 8 + 70_000 + 16 * 2)
        );
        assert!(matches!(store.open(&id, HOST), Err(ShareError::Gone)));
        assert!(matches!(store.probe(&id, HOST), Err(ShareError::Gone)));
    }

    #[test]
    fn a_resume_continues_until_the_last_byte_is_delivered() {
        let store = store(true);
        let id = id_of(
            &store
                .create(memory(ShareKind::File, vec![2; 200_000]))
                .unwrap(),
        );
        let first = store.open(&id, HOST).unwrap();
        let total = first.cipher_len();
        let piece = first.read(0, 100_000).unwrap();
        first.delivered(0, piece.bytes.len() as u64);
        // The client drops and asks again from where it stopped.
        let again = store.open(&id, HOST).unwrap();
        let from = piece.bytes.len() as u64;
        let middle = again.read(from, 50_000).unwrap();
        again.delivered(from, from + 50_000);
        assert!(store.probe(&id, HOST).is_ok(), "not complete yet");
        let rest = again.read(from + 50_000, MAX_READ).unwrap();
        assert_eq!(from + 50_000 + rest.bytes.len() as u64, total);
        again.delivered(from + 50_000, total);
        assert!(matches!(store.probe(&id, HOST), Err(ShareError::Gone)));
        drop(middle);
    }

    #[test]
    fn asking_only_for_the_last_bytes_does_not_use_the_link_up() {
        let store = store(true);
        let id = id_of(
            &store
                .create(memory(ShareKind::File, vec![3; 1000]))
                .unwrap(),
        );
        let transfer = store.open(&id, HOST).unwrap();
        let len = transfer.cipher_len();
        let piece = transfer.read(len - 10, 10).unwrap();
        transfer.delivered(len - 10, len - 10 + piece.bytes.len() as u64);
        assert!(store.probe(&id, HOST).is_ok());
    }

    #[test]
    fn an_expired_link_is_gone_and_stays_gone() {
        let store = store(true);
        let id = id_of(&store.create(memory(ShareKind::File, vec![4; 10])).unwrap());
        store.advance(DEFAULT_EXPIRY + Duration::from_secs(1));
        assert!(matches!(store.open(&id, HOST), Err(ShareError::Gone)));
        assert!(!store.has_live());
    }

    #[test]
    fn a_revoked_link_stops_a_transfer_in_progress() {
        let store = store(true);
        let created = store
            .create(memory(ShareKind::File, vec![5; 500_000]))
            .unwrap();
        let id = id_of(&created);
        let transfer = store.open(&id, HOST).unwrap();
        assert!(transfer.read(0, 1000).is_ok());
        assert!(store.revoke(&created.handle));
        assert!(matches!(transfer.read(1000, 1000), Err(ShareError::Gone)));
        assert!(matches!(store.open(&id, HOST), Err(ShareError::Gone)));
        assert!(!store.revoke(&created.handle), "already ended");
    }

    #[test]
    fn only_the_base_url_host_is_served() {
        let store = store(true);
        let id = id_of(&store.create(memory(ShareKind::File, vec![6; 10])).unwrap());
        for host in ["fetchpath.localhost", "127.0.0.1:8080", "evil.example", ""] {
            assert!(
                matches!(store.open(&id, host), Err(ShareError::WrongHost)),
                "{host}"
            );
            assert!(
                matches!(store.probe(&id, host), Err(ShareError::WrongHost)),
                "{host}"
            );
        }
        assert!(store.open(&id, "SHARE.example.test").is_ok());
    }

    #[test]
    fn unknown_ids_and_file_links_while_sharing_is_off() {
        let store = store(false);
        assert!(matches!(
            store.probe("nope", HOST),
            Err(ShareError::Unknown)
        ));
        let id = id_of(&store.create(memory(ShareKind::File, vec![7; 10])).unwrap());
        assert!(matches!(store.open(&id, HOST), Err(ShareError::Off)));
        // The setup test is the one thing served while sharing is off.
        let test = id_of(&store.create(memory(ShareKind::Test, vec![8; 10])).unwrap());
        assert!(store.open(&test, HOST).is_ok());
    }

    #[test]
    fn the_test_passes_only_for_a_complete_delivery_through_the_base_host() {
        let store = store(false);
        let test = id_of(
            &store
                .create(memory(ShareKind::Test, vec![9; 100_000]))
                .unwrap(),
        );
        let transfer = store.open(&test, HOST).unwrap();
        let piece = transfer.read(0, 1000).unwrap();
        transfer.delivered(0, piece.bytes.len() as u64);
        assert_eq!(
            store.test_passed(),
            None,
            "a partial delivery does not pass"
        );
        download(&store, &test).unwrap();
        assert_eq!(store.test_passed().as_deref(), Some(HOST));
        store.clear_test();
        assert_eq!(store.test_passed(), None);
    }

    #[test]
    fn a_new_test_ends_the_old_test_link_and_keeps_the_host_it_began_on() {
        let store = store(false);
        let old = id_of(
            &store
                .create(memory(ShareKind::Test, vec![9; 100_000]))
                .unwrap(),
        );
        let begun = store.open(&old, HOST).unwrap();
        store.clear_test();
        assert_eq!(store.open(&old, HOST).err(), Some(ShareError::Gone));

        let fresh = id_of(
            &store
                .create(memory(ShareKind::Test, vec![9; 100_000]))
                .unwrap(),
        );
        let transfer = store.open(&fresh, HOST).unwrap();
        store.configure(false, Some("other.example.test".to_owned()));
        let mut at = 0;
        while at < transfer.cipher_len() {
            let piece = transfer.read(at, MAX_READ).unwrap();
            at += piece.bytes.len() as u64;
            transfer.delivered(at - piece.bytes.len() as u64, at);
        }
        assert_eq!(store.test_passed().as_deref(), Some(HOST));
        drop(begun);
    }

    #[test]
    fn a_ticket_stops_serving_past_its_byte_budget() {
        let store = store(true);
        let id = id_of(
            &store
                .create(memory(ShareKind::File, vec![1; 300_000]))
                .unwrap(),
        );
        let transfer = store.open(&id, HOST).unwrap();
        let total = transfer.cipher_len();
        let mut served = 0;
        let outcome = loop {
            // Always the first range: a client re-asking without progress.
            match transfer.read(0, MAX_READ.min(total as usize)) {
                Ok(piece) => served += piece.bytes.len() as u64,
                Err(why) => break why,
            }
        };
        assert_eq!(outcome, ShareError::Gone);
        assert!(served <= total + BUDGET_SLACK, "{served} of {total}");
        assert!(
            served >= total + BUDGET_SLACK - MAX_READ as u64,
            "{served} of {total}"
        );
        assert!(
            store
                .list()
                .iter()
                .all(|info| info.phase == Phase::OverBudget)
        );
    }

    #[test]
    fn reads_are_rate_limited_by_a_wait_not_a_failure() {
        let store = store(true);
        let id = id_of(
            &store
                .create(memory(ShareKind::File, vec![1; 3 * MAX_READ]))
                .unwrap(),
        );
        let transfer = store.open(&id, HOST).unwrap();
        // The first read owes exactly its share of the cap: no time has passed.
        let wait = transfer.read(0, MAX_READ).unwrap().wait;
        let owed = Duration::from_secs_f64(MAX_READ as f64 / RATE as f64);
        assert!(wait <= owed && wait > owed / 2, "{wait:?} of {owed:?}");
    }

    #[test]
    fn events_and_the_list_never_carry_the_id_or_the_key() {
        let store = store(true);
        let created = store.create(memory(ShareKind::File, vec![1; 10])).unwrap();
        let id = id_of(&created);
        let key = created.link_path.split('#').nth(1).unwrap().to_owned();
        download(&store, &id).unwrap();
        let text = format!("{:?}{:?}", store.drain_events(), store.list());
        assert!(!text.contains(&id) && !text.contains(&key));
        assert!(text.contains("Created") && text.contains("Opened") && text.contains("Completed"));
    }

    #[test]
    fn the_served_bytes_open_with_the_fragment_key() {
        let store = store(true);
        let created = store
            .create(memory(ShareKind::File, vec![42; 70_000]))
            .unwrap();
        let id = id_of(&created);
        let key_text = created.link_path.split('#').nth(1).unwrap();
        let stream = download(&store, &id).unwrap();
        assert_eq!(key_text.len(), 43);
        let key = {
            let table: Vec<u8> = (b'A'..=b'Z')
                .chain(b'a'..=b'z')
                .chain(b'0'..=b'9')
                .chain(*b"-_")
                .collect();
            let bits: String = key_text
                .bytes()
                .map(|c| format!("{:06b}", table.iter().position(|t| *t == c).unwrap()))
                .collect();
            let mut key = [0_u8; 32];
            for (i, byte) in key.iter_mut().enumerate() {
                *byte = u8::from_str_radix(&bits[i * 8..i * 8 + 8], 2).unwrap();
            }
            key
        };
        let salt: [u8; 16] = stream[6..22].try_into().unwrap();
        let frame = Frame::new(&key, salt, stream[5]).unwrap();
        let full = (1 << frame::CHUNK_LOG2) + frame::TAG_LEN;
        let mut plain = Vec::new();
        let body = &stream[frame::HEADER_LEN..];
        let chunks: Vec<&[u8]> = body.chunks(full).collect();
        for (i, chunk) in chunks.iter().enumerate() {
            plain.extend(frame.open(i as u32, i + 1 == chunks.len(), chunk).unwrap());
        }
        assert_eq!(&plain[..2], &[0, 8]);
        assert_eq!(&plain[2..10], b"note.txt");
        assert_eq!(&plain[10..], &vec![42; 70_000][..]);
    }

    #[test]
    fn the_name_and_size_are_not_in_the_clear_stream() {
        let store = store(true);
        let id = id_of(
            &store
                .create(memory(ShareKind::File, b"secret-body".to_vec()))
                .unwrap(),
        );
        let stream = download(&store, &id).unwrap();
        let text = String::from_utf8_lossy(&stream);
        assert!(!text.contains("note.txt") && !text.contains("secret-body"));
    }

    #[cfg(windows)]
    mod frozen {
        use super::*;
        use std::os::windows::fs::OpenOptionsExt;

        /// Opens the way the engine does for a share: nobody else may write,
        /// rename or delete while the handle is held.
        fn locked(path: &std::path::Path) -> File {
            std::fs::OpenOptions::new()
                .read(true)
                .share_mode(1) // FILE_SHARE_READ
                .open(path)
                .unwrap()
        }

        fn file_share(path: &std::path::Path, size: u64) -> NewShare {
            NewShare {
                kind: ShareKind::File,
                name: "doc.bin".to_owned(),
                source: Source::File(locked(path)),
                size,
                expiry: DEFAULT_EXPIRY,
            }
        }

        #[test]
        fn while_the_link_is_live_the_system_refuses_a_write_a_rename_and_a_delete() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("doc.bin");
            std::fs::write(&path, vec![1_u8; 100_000]).unwrap();
            let store = store(true);
            let created = store.create(file_share(&path, 100_000)).unwrap();

            assert!(std::fs::OpenOptions::new().write(true).open(&path).is_err());
            assert!(std::fs::write(&path, b"changed").is_err());
            assert!(std::fs::rename(&path, dir.path().join("moved.bin")).is_err());
            assert!(std::fs::remove_file(&path).is_err());
            // Reading stays possible, and the link still serves the original.
            assert!(std::fs::read(&path).is_ok());
            assert!(download(&store, &id_of(&created)).is_ok());

            // Revoked: the file is the person's again.
            let again = store.create(file_share(&path, 100_000));
            assert!(again.is_ok());
            store.revoke_all();
            std::fs::write(&path, b"changed").unwrap();
        }

        #[test]
        fn a_file_already_open_for_writing_cannot_be_shared() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("doc.bin");
            std::fs::write(&path, b"data").unwrap();
            let _writer = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
            let refused = std::fs::OpenOptions::new()
                .read(true)
                .share_mode(1)
                .open(&path);
            assert!(refused.is_err());
        }

        #[test]
        fn completion_and_expiry_release_the_file() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("doc.bin");
            std::fs::write(&path, vec![1_u8; 1000]).unwrap();
            let store = store(true);
            let id = id_of(&store.create(file_share(&path, 1000)).unwrap());
            download(&store, &id).unwrap();
            std::fs::write(&path, b"free again").unwrap();

            std::fs::write(&path, vec![1_u8; 1000]).unwrap();
            store.create(file_share(&path, 1000)).unwrap();
            store.advance(DEFAULT_EXPIRY + Duration::from_secs(1));
            store.sweep();
            std::fs::remove_file(&path).unwrap();
        }

        #[test]
        fn a_file_that_is_not_the_size_that_was_hashed_is_refused() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("doc.bin");
            std::fs::write(&path, vec![1_u8; 1000]).unwrap();
            let store = store(true);
            let mut new = file_share(&path, 1000);
            new.size = 999;
            assert!(matches!(store.create(new), Err(ShareError::Unavailable)));
        }
    }
}
