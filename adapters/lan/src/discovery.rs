//! Finding paired devices on the local network (FP-034).
//!
//! A device that is sharing broadcasts a short beacon: the port it serves on,
//! the time, a fresh nonce, and an HMAC over them keyed by its own public
//! key. Someone who does not know that key sees random bytes: no name, no
//! fingerprint, nothing about the content it holds. A paired device, which
//! pinned the key, recognizes the beacon and learns where to connect.
//!
//! A beacon is a hint, never an authority. Anyone who knows a public key can
//! forge one, and a beacon can be replayed from another address; either only
//! sends a request to the wrong place, where the pinned-key session fails.

use crate::handshake::{mac, mac_matches};
use crate::identity::{DeviceIdentity, PeerKey, random_bytes};
use std::io;

/// Where beacons are sent and heard.
pub const DISCOVERY_PORT: u16 = 47633;
/// Every beacon has exactly this length; anything else is refused unread.
pub const BEACON_LEN: usize = 4 + 2 + 8 + 16 + 32;
/// How far a beacon's time may be from the receiver's.
pub const MAX_SKEW_SECS: u64 = 60;
const MAGIC: &[u8; 4] = b"FPD1";
const LABEL: &[u8] = b"fetchpath-discovery-v1";

fn signed(port: u16, secs: u64, nonce: &[u8]) -> Vec<u8> {
    let mut message = Vec::with_capacity(LABEL.len() + 2 + 8 + 16);
    message.extend_from_slice(LABEL);
    message.extend_from_slice(&port.to_be_bytes());
    message.extend_from_slice(&secs.to_be_bytes());
    message.extend_from_slice(nonce);
    message
}

/// A beacon announcing that this device serves on `port`.
pub fn beacon(identity: &DeviceIdentity, port: u16, now_secs: u64) -> io::Result<[u8; BEACON_LEN]> {
    let nonce: [u8; 16] = random_bytes()?;
    let tag = mac(
        identity.public_key().as_bytes(),
        &signed(port, now_secs, &nonce),
    );
    let mut out = [0_u8; BEACON_LEN];
    out[..4].copy_from_slice(MAGIC);
    out[4..6].copy_from_slice(&port.to_be_bytes());
    out[6..14].copy_from_slice(&now_secs.to_be_bytes());
    out[14..30].copy_from_slice(&nonce);
    out[30..].copy_from_slice(&tag);
    Ok(out)
}

/// Which pinned device sent `datagram`, and the port it serves on. `None`
/// for anything malformed, stale, or from a device not pinned.
pub fn recognize(datagram: &[u8], pinned: &[PeerKey], now_secs: u64) -> Option<(PeerKey, u16)> {
    if datagram.len() != BEACON_LEN || &datagram[..4] != MAGIC {
        return None;
    }
    let port = u16::from_be_bytes([datagram[4], datagram[5]]);
    let secs = u64::from_be_bytes(datagram[6..14].try_into().ok()?);
    if port == 0 || secs.abs_diff(now_secs) > MAX_SKEW_SECS {
        return None;
    }
    let message = signed(port, secs, &datagram[14..30]);
    pinned
        .iter()
        .find(|key| mac_matches(key.as_bytes(), &message, &datagram[30..]))
        .map(|key| (*key, port))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_paired_device_recognizes_a_beacon_and_a_stranger_learns_nothing() {
        let sender = DeviceIdentity::generate().unwrap();
        let other = DeviceIdentity::generate().unwrap();
        let now = 1_790_000_000;
        let first = beacon(&sender, 47631, now).unwrap();
        let second = beacon(&sender, 47631, now).unwrap();
        // Fresh nonce each time: two beacons from one device do not match.
        assert_ne!(first[14..], second[14..]);
        // Nothing in it is the key or its fingerprint.
        let key = sender.public_key();
        assert!(
            !first
                .windows(8)
                .any(|window| key.as_bytes().windows(8).any(|k| k == window))
        );

        assert_eq!(
            recognize(&first, &[other.public_key(), key], now + 5),
            Some((key, 47631))
        );
        assert_eq!(recognize(&first, &[other.public_key()], now), None);
    }

    #[test]
    fn malformed_stale_and_altered_beacons_are_refused() {
        let sender = DeviceIdentity::generate().unwrap();
        let key = [sender.public_key()];
        let now = 1_790_000_000;
        let good = beacon(&sender, 47631, now).unwrap();
        assert!(recognize(&good[..BEACON_LEN - 1], &key, now).is_none());
        assert!(recognize(&[good.as_slice(), &[0]].concat(), &key, now).is_none());
        assert!(recognize(&[], &key, now).is_none());
        assert!(recognize(&good, &key, now + MAX_SKEW_SECS + 1).is_none());
        for index in [0, 5, 13, 20, 40] {
            let mut altered = good;
            altered[index] ^= 1;
            assert!(recognize(&altered, &key, now).is_none(), "byte {index}");
        }
        let zero_port = beacon(&sender, 0, now).unwrap();
        assert!(recognize(&zero_port, &key, now).is_none());
    }
}
