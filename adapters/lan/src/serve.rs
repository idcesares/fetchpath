//! Serving cache entries to paired peers.
//!
//! An entry leaves this machine only when all of these hold: LAN mode is on,
//! the requesting peer is pinned and authenticated, the entry's provenance is
//! `Public`, and the entry carries a trusted digest (every cache entry does, by
//! construction). An absent entry, a `Credentialed` entry and a request made
//! while LAN mode is off all receive the same `not_available` refusal, so a
//! peer cannot learn that private content exists.

use crate::frame::{MAX_CHUNK_BYTES, kind};
use crate::identity::{DeviceIdentity, PeerKey};
use crate::pins::PinStore;
use crate::session::{SecureChannel, SessionError, accept_session_with};
use fetchpath_cache::{ContentCache, ContentId};
use std::fs::File;
use std::io::{self, Read};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub(crate) const NOT_AVAILABLE: &str = "not_available";
pub(crate) const BUDGET_EXHAUSTED: &str = "budget_exhausted";
pub(crate) const BAD_REQUEST: &str = "bad_request";

/// How long a session may sit idle between requests.
const IDLE_TIMEOUT: Duration = Duration::from_secs(60);

/// Limits on what one session may take from this device.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UploadBudget {
    /// Total content bytes one session may receive. A request that would
    /// exceed what remains is refused before any of it is sent.
    pub session_bytes: u64,
    /// Upload pace for one transfer. Zero means unpaced.
    pub bytes_per_second: u64,
}

impl Default for UploadBudget {
    fn default() -> Self {
        Self {
            session_bytes: 8 * 1024 * 1024 * 1024,
            bytes_per_second: 16 * 1024 * 1024,
        }
    }
}

/// Paces one transfer against its start time, so the average never exceeds
/// the configured rate.
struct Pacer {
    rate: u64,
    started: Instant,
    sent: u64,
}

impl Pacer {
    fn new(rate: u64) -> Self {
        Self {
            rate,
            started: Instant::now(),
            sent: 0,
        }
    }

    fn record(&mut self, bytes: u64) {
        self.sent += bytes;
        if self.rate == 0 {
            return;
        }
        let due = Duration::from_secs_f64(self.sent as f64 / self.rate as f64);
        let elapsed = self.started.elapsed();
        if due > elapsed {
            std::thread::sleep(due - elapsed);
        }
    }
}

/// What one session did. Carries identifiers only, never content.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SessionSummary {
    pub peer: Option<PeerKey>,
    pub served: Vec<ContentId>,
    pub refused: u32,
    pub bytes_sent: u64,
}

pub struct PeerServer {
    identity: Arc<DeviceIdentity>,
    pins: Arc<Mutex<PinStore>>,
    cache: Arc<Mutex<ContentCache>>,
    enabled: Arc<AtomicBool>,
    budget: UploadBudget,
}

/// Releases a cache pin however a transfer ends.
struct Held<'a> {
    cache: &'a Mutex<ContentCache>,
    id: ContentId,
}

impl Drop for Held<'_> {
    fn drop(&mut self) {
        if let Ok(mut cache) = self.cache.lock() {
            cache.unpin(&self.id);
        }
    }
}

impl PeerServer {
    pub fn new(
        identity: Arc<DeviceIdentity>,
        pins: Arc<Mutex<PinStore>>,
        cache: Arc<Mutex<ContentCache>>,
        enabled: Arc<AtomicBool>,
        budget: UploadBudget,
    ) -> Self {
        Self {
            identity,
            pins,
            cache,
            enabled,
            budget,
        }
    }

    /// Serves one connection until the peer says goodbye, goes idle, or breaks
    /// the protocol. With LAN mode off the connection is closed unanswered.
    pub fn serve_connection(&self, stream: TcpStream) -> Result<SessionSummary, SessionError> {
        if !self.enabled.load(Ordering::SeqCst) {
            return Err(SessionError::Refused("lan_disabled".to_owned()));
        }
        let pins = Arc::clone(&self.pins);
        let mut channel = accept_session_with(stream, &self.identity, |key| {
            pins.lock().is_ok_and(|pins| pins.is_pinned(key))
        })?;
        channel.set_read_timeout(Some(IDLE_TIMEOUT))?;

        let mut summary = SessionSummary {
            peer: Some(channel.peer()),
            ..SessionSummary::default()
        };
        let mut remaining = self.budget.session_bytes;
        loop {
            let (kind, payload) = match channel.receive() {
                Ok(frame) => frame,
                Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => break,
                Err(error) => return Err(error.into()),
            };
            match kind {
                kind::BYE => break,
                kind::GET => {
                    let Some(id) = std::str::from_utf8(&payload)
                        .ok()
                        .and_then(ContentId::parse)
                    else {
                        channel.send(kind::REFUSE, BAD_REQUEST.as_bytes())?;
                        break;
                    };
                    match self.serve_one(&mut channel, &id, remaining)? {
                        Some(bytes) => {
                            remaining -= bytes;
                            summary.bytes_sent += bytes;
                            summary.served.push(id);
                        }
                        None => summary.refused += 1,
                    }
                }
                _ => {
                    channel.send(kind::REFUSE, BAD_REQUEST.as_bytes())?;
                    break;
                }
            }
        }
        Ok(summary)
    }

    /// Returns the bytes sent, or `None` after sending a refusal.
    fn serve_one(
        &self,
        channel: &mut SecureChannel,
        id: &ContentId,
        remaining: u64,
    ) -> Result<Option<u64>, SessionError> {
        // Checked on every request, not only at the handshake: unpairing a
        // device revokes it on its next request, even within a live session.
        // A pin list that cannot be read is treated as holding nobody.
        let peer = channel.peer();
        let still_paired = self.pins.lock().is_ok_and(|pins| pins.is_pinned(&peer));
        let admitted = {
            let mut cache = self.cache.lock().map_err(|_| poisoned())?;
            // Another process on this device may have inserted or evicted
            // entries since the last request. A store that cannot be read is
            // treated as holding nothing.
            let fresh = cache.refresh().is_ok();
            match cache.lookup(id).filter(|_| fresh) {
                // Checked again here, not only at connect: turning LAN mode off
                // takes effect on the next request of a live session.
                Some(entry)
                    if still_paired
                        && self.enabled.load(Ordering::SeqCst)
                        && entry.is_shareable() =>
                {
                    if entry.bytes > remaining {
                        Err(BUDGET_EXHAUSTED)
                    } else {
                        cache
                            .pin(id)
                            .map(|path| (path, entry.bytes))
                            .ok_or(NOT_AVAILABLE)
                    }
                }
                _ => Err(NOT_AVAILABLE),
            }
        };
        let (path, bytes) = match admitted {
            Ok(found) => found,
            Err(reason) => {
                channel.send(kind::REFUSE, reason.as_bytes())?;
                return Ok(None);
            }
        };
        let _held = Held {
            cache: &self.cache,
            id: *id,
        };

        let file = File::open(&path).and_then(|file| {
            let length = file.metadata()?.len();
            Ok((file, length))
        });
        let mut file = match file {
            Ok((file, length)) if length == bytes => file,
            _ => {
                channel.send(kind::REFUSE, NOT_AVAILABLE.as_bytes())?;
                return Ok(None);
            }
        };

        channel.send(kind::OFFER, &bytes.to_be_bytes())?;
        let mut pacer = Pacer::new(self.budget.bytes_per_second);
        let mut buffer = vec![0_u8; MAX_CHUNK_BYTES];
        let mut sent = 0_u64;
        while sent < bytes {
            let want = (bytes - sent).min(MAX_CHUNK_BYTES as u64) as usize;
            let read = file.read(&mut buffer[..want])?;
            if read == 0 {
                // The entry shrank under a pin. The offer cannot be honoured,
                // so the session ends rather than sending a short object.
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "cache entry shorter than offered",
                )
                .into());
            }
            channel.send(kind::DATA, &buffer[..read])?;
            sent += read as u64;
            pacer.record(read as u64);
        }
        channel.send(kind::END, &[])?;
        Ok(Some(sent))
    }

    /// Accepts connections until `stop` is set, one thread per session, with
    /// at most `max_sessions` at once. Excess connections are closed unanswered.
    pub fn run(self: Arc<Self>, listener: TcpListener, stop: Arc<AtomicBool>, max_sessions: usize) {
        let active = Arc::new(AtomicUsize::new(0));
        for stream in listener.incoming() {
            if stop.load(Ordering::SeqCst) {
                break;
            }
            let Ok(stream) = stream else {
                continue;
            };
            if active.fetch_add(1, Ordering::SeqCst) >= max_sessions {
                active.fetch_sub(1, Ordering::SeqCst);
                continue;
            }
            let server = Arc::clone(&self);
            let active = Arc::clone(&active);
            std::thread::spawn(move || {
                let _ = server.serve_connection(stream);
                active.fetch_sub(1, Ordering::SeqCst);
            });
        }
    }
}

fn poisoned() -> SessionError {
    SessionError::Io(io::Error::other("cache lock poisoned"))
}
