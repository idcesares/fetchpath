//! Out-of-band pairing: a short code shown on one device and typed on the
//! other lets each side prove possession of the code while exchanging identity
//! keys, which both then pin.
//!
//! This is not a password-authenticated key exchange. A passive observer
//! learns nothing it can test the code against, because the confirmation keys
//! also depend on an X25519 secret. An *active* attacker who impersonates the
//! host to the joiner receives the joiner's confirmation and can attempt an
//! offline guess of the code. The code is therefore single-use, expires after
//! two minutes, and carries 50 bits of entropy; the host abandons a handshake
//! that stalls for more than ten seconds.

use crate::frame::{kind, write_frame};
use crate::handshake::{
    Ephemeral, HANDSHAKE_TIMEOUT, Hello, Received, derive_pair, expect, mac, mac_matches, refuse,
    send_hello, transcript,
};
use crate::identity::{DeviceIdentity, PeerKey, random_bytes};
use crate::pins::PinStore;
use std::fmt;
use std::io;
use std::net::TcpStream;
use std::time::{Duration, Instant};
use zeroize::Zeroizing;

pub const CODE_LIFETIME: Duration = Duration::from_secs(120);
/// Ten Crockford base32 characters.
pub const CODE_BITS: u32 = 50;
const CODE_CHARS: usize = 10;
const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

const TRANSCRIPT_LABEL: &[u8] = b"fetchpath-lan-pair-v1";
const CONFIRM_INFO: &[u8] = b"fetchpath-lan-pair-confirm-v1";
const JOINER: &[u8] = b"joiner";
const HOST: &[u8] = b"host";

/// The code a host shows. Single-use: the first hello consumes it whatever
/// the outcome, so it cannot be guessed at online.
pub struct PairingCode {
    normalized: Zeroizing<String>,
    issued_at: Instant,
    consumed: bool,
}

impl PairingCode {
    pub fn generate() -> io::Result<Self> {
        Self::generate_at(Instant::now())
    }

    /// Issues a code as if at `issued_at`, which is how expiry is tested.
    pub fn generate_at(issued_at: Instant) -> io::Result<Self> {
        let random = Zeroizing::new(random_bytes::<8>()?);
        let bits = u64::from_be_bytes(*random) >> (64 - CODE_BITS);
        let normalized: String = (0..CODE_CHARS)
            .rev()
            .map(|index| ALPHABET[((bits >> (index * 5)) & 0x1f) as usize] as char)
            .collect();
        Ok(Self {
            normalized: Zeroizing::new(normalized),
            issued_at,
            consumed: false,
        })
    }

    /// The code as a person should read it: `XXXXX-XXXXX`.
    pub fn display(&self) -> String {
        format!("{}-{}", &self.normalized[..5], &self.normalized[5..])
    }

    pub fn expires_at(&self) -> Instant {
        self.issued_at + CODE_LIFETIME
    }

    pub fn is_consumed(&self) -> bool {
        self.consumed
    }
}

impl fmt::Debug for PairingCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PairingCode")
            .field("consumed", &self.consumed)
            .finish_non_exhaustive()
    }
}

/// Accepts what people type: any case, with or without the dash or spaces,
/// and Crockford's substitutions of `I`/`L` for one and `O` for zero.
pub fn normalize_code(typed: &str) -> Option<Zeroizing<String>> {
    let mut out = Zeroizing::new(String::with_capacity(CODE_CHARS));
    for character in typed.chars() {
        let mapped = match character.to_ascii_uppercase() {
            '-' | ' ' => continue,
            'I' | 'L' => '1',
            'O' => '0',
            other => other,
        };
        if !mapped.is_ascii() || !ALPHABET.contains(&(mapped as u8)) {
            return None;
        }
        out.push(mapped);
    }
    (out.len() == CODE_CHARS).then_some(out)
}

#[derive(Debug)]
pub enum PairingError {
    /// The typed code is not ten characters of the code alphabet.
    InvalidCode,
    CodeExpired,
    CodeAlreadyUsed,
    /// The other side did not prove it holds the same code. Nothing was pinned.
    ConfirmationFailed,
    Refused(String),
    Io(io::Error),
}

impl fmt::Display for PairingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidCode => f.write_str("pairing.invalid_code"),
            Self::CodeExpired => f.write_str("pairing.code_expired"),
            Self::CodeAlreadyUsed => f.write_str("pairing.code_already_used"),
            Self::ConfirmationFailed => f.write_str("pairing.confirmation_failed"),
            Self::Refused(reason) => write!(f, "pairing.refused:{reason}"),
            Self::Io(error) => write!(f, "pairing.io:{error}"),
        }
    }
}

impl std::error::Error for PairingError {}

impl From<io::Error> for PairingError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

struct Keys {
    joiner: [u8; 32],
    host: [u8; 32],
}

fn confirmation_keys(th: &[u8; 32], shared: &[u8; 32], code: &str) -> Zeroizing<Keys> {
    let mut ikm = Zeroizing::new(Vec::with_capacity(32 + code.len()));
    ikm.extend_from_slice(shared);
    ikm.extend_from_slice(code.as_bytes());
    let okm = derive_pair(th, &ikm, CONFIRM_INFO);
    let mut keys = Zeroizing::new(Keys {
        joiner: [0; 32],
        host: [0; 32],
    });
    keys.joiner.copy_from_slice(&okm[..32]);
    keys.host.copy_from_slice(&okm[32..]);
    keys
}

impl zeroize::Zeroize for Keys {
    fn zeroize(&mut self) {
        self.joiner.zeroize();
        self.host.zeroize();
    }
}

fn role_message(role: &[u8], th: &[u8; 32]) -> Vec<u8> {
    let mut message = Vec::with_capacity(TRANSCRIPT_LABEL.len() + 1 + role.len() + 32);
    message.extend_from_slice(TRANSCRIPT_LABEL);
    message.push(b'/');
    message.extend_from_slice(role);
    message.extend_from_slice(th);
    message
}

fn confirmation(key: &[u8; 32], role: &[u8], th: &[u8; 32], identity: &DeviceIdentity) -> Vec<u8> {
    let message = role_message(role, th);
    let mut payload = Vec::with_capacity(96);
    payload.extend_from_slice(&mac(key, &message));
    payload.extend_from_slice(&identity.sign(&message));
    payload
}

fn confirmation_holds(
    payload: &[u8],
    key: &[u8; 32],
    role: &[u8],
    th: &[u8; 32],
    signer: &PeerKey,
) -> bool {
    if payload.len() != 96 {
        return false;
    }
    let message = role_message(role, th);
    let signature: [u8; 64] = payload[32..].try_into().expect("fixed-size slice");
    // Evaluate both so the outcome does not reveal which check failed first.
    let mac_ok = mac_matches(key, &message, &payload[..32]);
    let signature_ok = signer.verify(&message, &signature);
    mac_ok & signature_ok
}

fn with_timeouts(stream: &TcpStream) -> io::Result<()> {
    stream.set_read_timeout(Some(HANDSHAKE_TIMEOUT))?;
    stream.set_write_timeout(Some(HANDSHAKE_TIMEOUT))
}

/// Runs the host side for one incoming connection. On success the joiner's key
/// is pinned under `label` and returned.
pub fn host_pairing(
    stream: &mut TcpStream,
    identity: &DeviceIdentity,
    code: &mut PairingCode,
    now: Instant,
    pins: &mut PinStore,
    label: &str,
) -> Result<PeerKey, PairingError> {
    with_timeouts(stream)?;
    let Received::Frame(joiner_bytes) = expect(stream, kind::HELLO)? else {
        return Err(PairingError::Refused("joiner_refused".to_owned()));
    };
    let joiner = Hello::decode(&joiner_bytes)?;

    if code.consumed {
        refuse(stream, "code_used");
        return Err(PairingError::CodeAlreadyUsed);
    }
    code.consumed = true;
    if now >= code.expires_at() {
        refuse(stream, "code_expired");
        return Err(PairingError::CodeExpired);
    }

    let ephemeral = Ephemeral::generate()?;
    let host_bytes = send_hello(
        stream,
        &Hello {
            ephemeral: ephemeral.public,
            identity: identity.public_key(),
            nonce: random_bytes::<32>()?,
        },
    )?;
    let th = transcript(TRANSCRIPT_LABEL, &joiner_bytes, &host_bytes);
    let Ok(shared) = ephemeral.agree(&joiner.ephemeral) else {
        refuse(stream, "pairing_failed");
        return Err(PairingError::ConfirmationFailed);
    };
    let keys = confirmation_keys(&th, &shared, &code.normalized);

    let Received::Frame(confirm) = expect(stream, kind::CONFIRM)? else {
        return Err(PairingError::Refused("joiner_refused".to_owned()));
    };
    if !confirmation_holds(&confirm, &keys.joiner, JOINER, &th, &joiner.identity) {
        refuse(stream, "pairing_failed");
        return Err(PairingError::ConfirmationFailed);
    }

    pins.pin(joiner.identity, label)?;
    write_frame(
        stream,
        kind::CONFIRM,
        &confirmation(&keys.host, HOST, &th, identity),
    )?;
    Ok(joiner.identity)
}

/// Runs the joiner side against a host that is showing a code. On success the
/// host's key is pinned under `label` and returned.
pub fn join_pairing(
    stream: &mut TcpStream,
    identity: &DeviceIdentity,
    typed_code: &str,
    pins: &mut PinStore,
    label: &str,
) -> Result<PeerKey, PairingError> {
    // Checked before anything is sent: a malformed code never costs the host
    // its single use.
    let code = normalize_code(typed_code).ok_or(PairingError::InvalidCode)?;
    with_timeouts(stream)?;

    let ephemeral = Ephemeral::generate()?;
    let joiner_bytes = send_hello(
        stream,
        &Hello {
            ephemeral: ephemeral.public,
            identity: identity.public_key(),
            nonce: random_bytes::<32>()?,
        },
    )?;
    let host_bytes = match expect(stream, kind::HELLO)? {
        Received::Frame(bytes) => bytes,
        Received::Refused(reason) => return Err(refusal(reason)),
    };
    let host = Hello::decode(&host_bytes)?;
    let th = transcript(TRANSCRIPT_LABEL, &joiner_bytes, &host_bytes);
    let shared = ephemeral.agree(&host.ephemeral)?;
    let keys = confirmation_keys(&th, &shared, &code);

    write_frame(
        stream,
        kind::CONFIRM,
        &confirmation(&keys.joiner, JOINER, &th, identity),
    )?;
    let confirm = match expect(stream, kind::CONFIRM)? {
        Received::Frame(bytes) => bytes,
        Received::Refused(reason) => return Err(refusal(reason)),
    };
    if !confirmation_holds(&confirm, &keys.host, HOST, &th, &host.identity) {
        return Err(PairingError::ConfirmationFailed);
    }
    pins.pin(host.identity, label)?;
    Ok(host.identity)
}

fn refusal(reason: String) -> PairingError {
    match reason.as_str() {
        "code_expired" => PairingError::CodeExpired,
        "code_used" => PairingError::CodeAlreadyUsed,
        "pairing_failed" => PairingError::ConfirmationFailed,
        _ => PairingError::Refused(reason),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_code_is_ten_characters_of_the_alphabet_and_displays_with_a_dash() {
        let code = PairingCode::generate().expect("code");
        let shown = code.display();
        assert_eq!(shown.len(), 11);
        assert_eq!(&shown[5..6], "-");
        assert!(normalize_code(&shown).is_some());
    }

    #[test]
    fn typing_is_forgiving_but_not_loose() {
        assert_eq!(
            normalize_code("abcde-fghjk").as_deref().map(String::as_str),
            Some("ABCDEFGHJK")
        );
        assert_eq!(
            normalize_code("o1l2i 34567").as_deref().map(String::as_str),
            Some("0112134567")
        );
        assert!(normalize_code("ABCDE-FGHJ").is_none(), "too short");
        assert!(
            normalize_code("ABCDE-FGHJU").is_none(),
            "U is not in the alphabet"
        );
        assert!(normalize_code("ABCDE-FGHJKX").is_none(), "too long");
    }

    #[test]
    fn debug_output_never_contains_the_code() {
        let code = PairingCode::generate().expect("code");
        let shown = format!("{code:?}");
        assert!(!shown.contains(&code.normalized[..5]));
    }

    #[test]
    fn codes_differ() {
        let a = PairingCode::generate().expect("a");
        let b = PairingCode::generate().expect("b");
        assert_ne!(a.display(), b.display());
    }
}
