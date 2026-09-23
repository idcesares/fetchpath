//! Pieces shared by pairing and sessions: the hello message, ephemeral
//! X25519, the transcript hash and key derivation.

use crate::frame::{kind, read_frame, write_frame};
use crate::identity::{PeerKey, random_bytes};
use curve25519_dalek::montgomery::MontgomeryPoint;
use hkdf::Hkdf;
use hmac::{Hmac, KeyInit, Mac};
use sha2::{Digest, Sha256};
use std::io::{self, Read, Write};
use std::time::Duration;
use zeroize::Zeroizing;

pub(crate) const PROTOCOL_VERSION: u8 = 1;
pub(crate) const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const HELLO_BYTES: usize = 1 + 32 + 32 + 32;
const MAX_REASON_BYTES: usize = 64;

type HmacSha256 = Hmac<Sha256>;

pub(crate) struct Hello {
    pub ephemeral: [u8; 32],
    pub identity: PeerKey,
    pub nonce: [u8; 32],
}

impl Hello {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(HELLO_BYTES);
        out.push(PROTOCOL_VERSION);
        out.extend_from_slice(&self.ephemeral);
        out.extend_from_slice(self.identity.as_bytes());
        out.extend_from_slice(&self.nonce);
        out
    }

    pub fn decode(payload: &[u8]) -> io::Result<Self> {
        if payload.len() != HELLO_BYTES || payload[0] != PROTOCOL_VERSION {
            return Err(invalid("malformed or unsupported hello"));
        }
        let take = |range: std::ops::Range<usize>| -> [u8; 32] {
            payload[range].try_into().expect("fixed-size slice")
        };
        Ok(Self {
            ephemeral: take(1..33),
            identity: PeerKey::from_bytes(take(33..65)),
            nonce: take(65..97),
        })
    }
}

/// A one-use X25519 key. Dropped, and wiped, once the shared secret exists.
pub(crate) struct Ephemeral {
    secret: Zeroizing<[u8; 32]>,
    pub public: [u8; 32],
}

impl Ephemeral {
    pub fn generate() -> io::Result<Self> {
        let secret = Zeroizing::new(random_bytes::<32>()?);
        let public = MontgomeryPoint::mul_base_clamped(*secret).to_bytes();
        Ok(Self { secret, public })
    }

    /// Refuses the all-zero result a small-order peer key produces, which
    /// would otherwise make every derived key a public constant.
    pub fn agree(self, peer_public: &[u8; 32]) -> io::Result<Zeroizing<[u8; 32]>> {
        let shared = Zeroizing::new(
            MontgomeryPoint(*peer_public)
                .mul_clamped(*self.secret)
                .to_bytes(),
        );
        if shared.iter().all(|byte| *byte == 0) {
            return Err(invalid("peer ephemeral key has small order"));
        }
        Ok(shared)
    }
}

/// Binds both hellos, in initiator-then-responder order, under a label that
/// separates pairing transcripts from session transcripts.
pub(crate) fn transcript(label: &[u8], initiator: &[u8], responder: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    for part in [label, initiator, responder] {
        hasher.update((part.len() as u32).to_be_bytes());
        hasher.update(part);
    }
    hasher.finalize().into()
}

pub(crate) fn derive_pair(salt: &[u8; 32], ikm: &[u8], info: &[u8]) -> Zeroizing<[u8; 64]> {
    let mut okm = Zeroizing::new([0_u8; 64]);
    Hkdf::<Sha256>::new(Some(salt), ikm)
        .expand(info, okm.as_mut_slice())
        .expect("64 bytes is a valid HKDF-SHA256 output length");
    okm
}

pub(crate) fn mac(key: &[u8], message: &[u8]) -> [u8; 32] {
    let mut mac = <HmacSha256 as KeyInit>::new_from_slice(key).expect("HMAC takes any key length");
    mac.update(message);
    mac.finalize().into_bytes().into()
}

/// Constant-time comparison of a received MAC.
pub(crate) fn mac_matches(key: &[u8], message: &[u8], received: &[u8]) -> bool {
    let mut mac = <HmacSha256 as KeyInit>::new_from_slice(key).expect("HMAC takes any key length");
    mac.update(message);
    mac.verify_slice(received).is_ok()
}

pub(crate) fn send_hello(stream: &mut impl Write, hello: &Hello) -> io::Result<Vec<u8>> {
    let encoded = hello.encode();
    write_frame(stream, kind::HELLO, &encoded)?;
    Ok(encoded)
}

/// What the other side sent where a protocol step was expected.
pub(crate) enum Received {
    Frame(Vec<u8>),
    Refused(String),
}

/// Reads one frame of the expected kind, surfacing a refusal distinctly.
pub(crate) fn expect(stream: &mut impl Read, expected: u8) -> io::Result<Received> {
    let (kind, payload) = read_frame(stream)?;
    if kind == kind::REFUSE {
        return Ok(Received::Refused(reason_text(&payload)));
    }
    if kind != expected {
        return Err(invalid("unexpected frame"));
    }
    Ok(Received::Frame(payload))
}

pub(crate) fn refuse(stream: &mut impl Write, reason: &str) {
    let reason = &reason.as_bytes()[..reason.len().min(MAX_REASON_BYTES)];
    let _ = write_frame(stream, kind::REFUSE, reason);
}

/// A refusal reason is shown to people, so only a short printable ASCII token
/// is accepted from the wire.
fn reason_text(payload: &[u8]) -> String {
    let printable = payload.len() <= MAX_REASON_BYTES
        && payload
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'_');
    if printable {
        String::from_utf8_lossy(payload).into_owned()
    } else {
        "unspecified".to_owned()
    }
}

pub(crate) fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_sides_agree_on_the_shared_secret() {
        let a = Ephemeral::generate().expect("a");
        let b = Ephemeral::generate().expect("b");
        let (a_public, b_public) = (a.public, b.public);
        assert_eq!(*a.agree(&b_public).unwrap(), *b.agree(&a_public).unwrap());
    }

    #[test]
    fn a_small_order_peer_key_is_refused() {
        let a = Ephemeral::generate().expect("a");
        assert!(a.agree(&[0_u8; 32]).is_err());
    }

    #[test]
    fn a_hello_round_trips_and_a_wrong_version_is_refused() {
        let hello = Hello {
            ephemeral: [1; 32],
            identity: PeerKey::from_bytes([2; 32]),
            nonce: [3; 32],
        };
        let mut encoded = hello.encode();
        let decoded = Hello::decode(&encoded).expect("decode");
        assert_eq!(decoded.identity, hello.identity);
        encoded[0] = 9;
        assert!(Hello::decode(&encoded).is_err());
        assert!(Hello::decode(&encoded[..40]).is_err());
    }

    #[test]
    fn the_transcript_depends_on_order_and_label() {
        let one = transcript(b"x", b"a", b"b");
        assert_ne!(one, transcript(b"x", b"b", b"a"));
        assert_ne!(one, transcript(b"y", b"a", b"b"));
        // Length prefixes stop boundary shifting from colliding.
        assert_ne!(transcript(b"x", b"ab", b"c"), transcript(b"x", b"a", b"bc"));
    }

    #[test]
    fn a_hostile_refusal_reason_is_not_echoed() {
        assert_eq!(reason_text(b"not_paired"), "not_paired");
        assert_eq!(reason_text(b"\x1b[31mred"), "unspecified");
    }
}
