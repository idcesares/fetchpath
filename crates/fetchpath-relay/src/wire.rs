//! What an engine and the relay say to each other before bytes are piped.
//!
//! After TLS the relay sends a random challenge. The engine answers with its
//! role, a name and `HMAC-SHA256(enrollment key, "fpr1" || role || name ||
//! challenge)`. The key never crosses the wire and a failed check closes the
//! connection without a reason.

use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;
use std::fmt;
use std::io;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// TLS ALPN an engine offers; a browser offers `http/1.1`.
pub const ALPN: &[u8] = b"fetchpath-relay/1";
pub const NONCE_LEN: usize = 32;
pub const MAX_NAME: usize = 64;
/// The longest ticket id an engine may announce or a visitor may ask for.
pub const MAX_ID: usize = 64;
pub const MAX_IDS: usize = 256;
pub const TAG_PING: u8 = b'P';
pub const TAG_ANNOUNCE: u8 = b'A';
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Role {
    /// The one long-lived connection: announces ids and pings.
    Control,
    /// An idle connection the relay may hand a visitor.
    Work,
}

impl Role {
    fn byte(self) -> u8 {
        match self {
            Role::Control => b'C',
            Role::Work => b'W',
        }
    }

    fn from_byte(byte: u8) -> Option<Self> {
        match byte {
            b'C' => Some(Role::Control),
            b'W' => Some(Role::Work),
            _ => None,
        }
    }
}

/// 32 random bytes the relay's owner creates once and gives to each engine.
#[derive(Clone)]
pub struct EnrollmentKey([u8; 32]);

impl fmt::Debug for EnrollmentKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("EnrollmentKey(..)")
    }
}

impl EnrollmentKey {
    pub fn generate() -> io::Result<Self> {
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes).map_err(|error| io::Error::other(error.to_string()))?;
        Ok(Self(bytes))
    }

    pub fn to_hex(&self) -> String {
        self.0.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    pub fn from_hex(text: &str) -> Option<Self> {
        let text = text.trim();
        if text.len() != 64 || !text.is_ascii() {
            return None;
        }
        let mut bytes = [0u8; 32];
        for (slot, pair) in bytes.iter_mut().zip(text.as_bytes().chunks(2)) {
            *slot = u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok()?;
        }
        Some(Self(bytes))
    }

    fn mac(&self, role: Role, name: &str, nonce: &[u8; NONCE_LEN]) -> Hmac<Sha256> {
        let mut mac =
            <Hmac<Sha256> as KeyInit>::new_from_slice(&self.0).expect("HMAC takes any key length");
        mac.update(b"fpr1");
        mac.update(&[role.byte(), name.len() as u8]);
        mac.update(name.as_bytes());
        mac.update(nonce);
        mac
    }
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_NAME
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

/// Relay side. `Err` means: close the connection, say nothing.
pub(crate) async fn accept<S>(stream: &mut S, key: &EnrollmentKey) -> io::Result<(Role, String)>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let denied = || io::Error::new(io::ErrorKind::PermissionDenied, "not enrolled");
    let work = async {
        let mut nonce = [0u8; NONCE_LEN];
        getrandom::fill(&mut nonce).map_err(|error| io::Error::other(error.to_string()))?;
        stream.write_all(&nonce).await?;
        stream.flush().await?;
        let mut head = [0u8; 2];
        stream.read_exact(&mut head).await?;
        let role = Role::from_byte(head[0]).ok_or_else(denied)?;
        let length = usize::from(head[1]);
        if length == 0 || length > MAX_NAME {
            return Err(denied());
        }
        let mut name = vec![0u8; length];
        stream.read_exact(&mut name).await?;
        let mut received = [0u8; 32];
        stream.read_exact(&mut received).await?;
        let name = String::from_utf8(name).map_err(|_| denied())?;
        if !valid_name(&name)
            || key
                .mac(role, &name, &nonce)
                .verify_slice(&received)
                .is_err()
        {
            return Err(denied());
        }
        stream.write_all(&[1]).await?;
        stream.flush().await?;
        Ok((role, name))
    };
    tokio::time::timeout(HANDSHAKE_TIMEOUT, work)
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "enrollment timed out"))?
}

/// Engine side.
pub(crate) async fn enroll<S>(
    stream: &mut S,
    key: &EnrollmentKey,
    role: Role,
    name: &str,
) -> io::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    if !valid_name(name) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the engine name is not allowed",
        ));
    }
    let refused = || {
        io::Error::new(
            io::ErrorKind::PermissionDenied,
            "the relay refused this engine",
        )
    };
    let work = async {
        let mut nonce = [0u8; NONCE_LEN];
        stream.read_exact(&mut nonce).await?;
        let proof = key.mac(role, name, &nonce).finalize().into_bytes();
        let mut reply = Vec::with_capacity(2 + name.len() + 32);
        reply.extend_from_slice(&[role.byte(), name.len() as u8]);
        reply.extend_from_slice(name.as_bytes());
        reply.extend_from_slice(&proof);
        stream.write_all(&reply).await?;
        stream.flush().await?;
        let mut accepted = [0u8; 1];
        stream.read_exact(&mut accepted).await.map_err(|error| {
            // A relay that closes without the accept byte refused us; any
            // other failure is the network, not the key.
            if matches!(
                error.kind(),
                io::ErrorKind::UnexpectedEof | io::ErrorKind::ConnectionReset
            ) {
                refused()
            } else {
                error
            }
        })?;
        if accepted[0] == 1 {
            Ok(())
        } else {
            Err(refused())
        }
    };
    tokio::time::timeout(HANDSHAKE_TIMEOUT, work)
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "enrollment timed out"))?
}

/// A ticket id as the share handler spells it.
pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_ID
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_round_trips_and_debug_hides_the_key() {
        let key = EnrollmentKey::generate().unwrap();
        let again = EnrollmentKey::from_hex(&key.to_hex()).unwrap();
        assert_eq!(key.0, again.0);
        assert!(!format!("{key:?}").contains(&key.to_hex()));
        assert!(EnrollmentKey::from_hex("zz").is_none());
    }
}
