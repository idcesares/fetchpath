//! Launch tickets and browser sessions of the web UI (FP-104).
//!
//! A ticket is made when the person asks to open the web UI, lives in memory
//! only, and is good once for a minute. Redeeming it makes a session: a
//! cookie the browser keeps and an id for the device it stands for. Sessions
//! live in memory only, as a SHA-256 of each cookie, and end with the
//! listener.

use fetchpath_protocol::principal::DeviceId;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Where an earlier build kept sessions; only ever deleted now.
const LEGACY_FILE: &str = "web-sessions.json";
pub(super) const COOKIE_NAME: &str = "fp_session";
pub(super) const TICKET_TTL: Duration = Duration::from_secs(60);

/// Tickets waiting to be used at once; a flood of requests cannot grow it.
const MAX_TICKETS: usize = 16;
/// A session unused this long is gone.
const SESSION_TTL_MS: u64 = 30 * 24 * 3_600_000;
const MAX_SESSIONS: usize = 32;
/// 256 random bits, as hex.
const SECRET_HEX_LENGTH: usize = 64;

pub(super) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(0))
}

pub(super) fn random_hex(bytes: usize) -> Result<String, String> {
    let mut buffer = vec![0_u8; bytes];
    getrandom::fill(&mut buffer)
        .map_err(|error| format!("The system could not make a random value: {error}"))?;
    Ok(buffer.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn digest(text: &str) -> String {
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Single-use sign-in links, kept as hashes so nothing in memory reads as a
/// ticket either.
pub(super) struct Tickets {
    ttl: Duration,
    live: HashMap<String, Instant>,
}

impl Tickets {
    pub(super) fn new(ttl: Duration) -> Self {
        Self {
            ttl,
            live: HashMap::new(),
        }
    }

    pub(super) fn issue(&mut self) -> Result<String, String> {
        let ttl = self.ttl;
        self.live.retain(|_, issued| issued.elapsed() < ttl);
        if self.live.len() >= MAX_TICKETS
            && let Some(oldest) = self
                .live
                .iter()
                .min_by_key(|(_, issued)| **issued)
                .map(|(key, _)| key.clone())
        {
            self.live.remove(&oldest);
        }
        let ticket = random_hex(32)?;
        self.live.insert(digest(&ticket), Instant::now());
        Ok(ticket)
    }

    /// Whether `ticket` was issued, is unused and has not expired. It never
    /// works a second time.
    pub(super) fn redeem(&mut self, ticket: &str) -> bool {
        self.live
            .remove(&digest(ticket))
            .is_some_and(|issued| issued.elapsed() < self.ttl)
    }

    pub(super) fn clear(&mut self) {
        self.live.clear();
    }

    #[cfg(test)]
    pub(super) fn is_empty(&self) -> bool {
        self.live.is_empty()
    }
}

struct Record {
    /// SHA-256 of the cookie, hex.
    hash: String,
    device: String,
    created_ms: u64,
    last_used_ms: u64,
}

pub(super) struct Created {
    pub(super) cookie: String,
    /// Devices dropped to stay within the cap, whose sockets must close.
    pub(super) evicted: Vec<String>,
}

/// Browser sessions, kept in memory only: they end with the listener, so
/// with the engine.
#[derive(Default)]
pub(super) struct Sessions {
    records: Vec<Record>,
}

impl Sessions {
    pub(super) fn create(&mut self, now: u64) -> Result<Created, String> {
        let cookie = random_hex(SECRET_HEX_LENGTH / 2)?;
        let device = DeviceId::try_from(random_hex(10)?)?;
        let mut evicted = Vec::new();
        while self.records.len() >= MAX_SESSIONS {
            let oldest = (0..self.records.len())
                .min_by_key(|&index| self.records[index].created_ms)
                .expect("the list is not empty");
            evicted.push(self.records.remove(oldest).device);
        }
        self.records.push(Record {
            hash: digest(&cookie),
            device: device.to_string(),
            created_ms: now,
            last_used_ms: now,
        });
        Ok(Created { cookie, evicted })
    }

    /// The device `cookie` stands for, when it is a live session.
    pub(super) fn check(&mut self, cookie: &str, now: u64) -> Option<DeviceId> {
        if cookie.len() != SECRET_HEX_LENGTH || !cookie.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let hash = digest(&cookie.to_ascii_lowercase());
        let index = self.records.iter().position(|record| record.hash == hash)?;
        if now.saturating_sub(self.records[index].last_used_ms) > SESSION_TTL_MS {
            self.records.remove(index);
            return None;
        }
        self.records[index].last_used_ms = now;
        DeviceId::try_from(self.records[index].device.clone()).ok()
    }

    /// Whether `device` still has a session.
    pub(super) fn has_device(&self, device: &DeviceId) -> bool {
        let device = device.to_string();
        self.records.iter().any(|record| record.device == device)
    }

    /// Ends every session.
    pub(super) fn clear(&mut self) {
        self.records.clear();
    }
}

/// Deletes the sessions file an earlier build kept: sessions are no longer
/// written to disk.
pub(super) fn forget_legacy_file(dir: &std::path::Path) {
    let _ = std::fs::remove_file(dir.join(LEGACY_FILE));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_idle_session_expires_and_only_a_hash_is_kept() {
        let mut sessions = Sessions::default();
        let first = sessions.create(1_000).unwrap();
        assert!(sessions.records.iter().all(|r| r.hash != first.cookie));
        assert!(sessions.check(&first.cookie, 2_000).is_some());
        let late = 2_000 + SESSION_TTL_MS + 1;
        assert!(sessions.check(&first.cookie, late).is_none());
    }

    #[test]
    fn the_oldest_session_goes_past_the_cap() {
        let mut sessions = Sessions::default();
        let first = sessions.create(1).unwrap();
        for at in 2..=(MAX_SESSIONS as u64 + 1) {
            sessions.create(at).unwrap();
        }
        assert!(sessions.check(&first.cookie, 100).is_none());
        assert_eq!(sessions.records.len(), MAX_SESSIONS);
    }
}
