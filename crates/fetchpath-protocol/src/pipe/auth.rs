//! The per-install secret and the mutual challenge-response handshake.
//!
//! Each side sends a fresh 32-byte nonce and proves it holds the secret with
//! an HMAC-SHA256 over both nonces. The two directions use different labels,
//! so a proof cannot be reflected back, and each proof covers the other
//! side's fresh nonce, so a recorded one cannot be replayed. The secret itself
//! never crosses the pipe.

use crate::error::{Action, ErrorCode, ErrorScope, ProtocolError};
use hmac::{Hmac, KeyInit, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::fmt;
use std::io::{self, Read, Write};
use std::path::Path;

pub const SECRET_BYTES: usize = 32;
pub const NONCE_BYTES: usize = 32;
const SERVER_LABEL: &[u8] = b"fetchpath-pipe-v1 server proof";
const CLIENT_LABEL: &[u8] = b"fetchpath-pipe-v1 client proof";
/// Handshake transport name and version, independent of the protocol's.
pub const TRANSPORT: &str = "fetchpath-pipe";
pub const TRANSPORT_VERSION: u32 = 1;

pub fn auth_error(code: &'static str, message: impl Into<String>) -> ProtocolError {
    ProtocolError::new(
        ErrorCode::try_from(code.to_owned()).expect("a valid built-in code"),
        ErrorScope::Connection,
        message,
    )
}

/// The engine's per-install secret. Never printed, and wiped on drop.
pub struct EngineSecret([u8; SECRET_BYTES]);

impl EngineSecret {
    pub fn from_bytes(bytes: [u8; SECRET_BYTES]) -> Self {
        Self(bytes)
    }

    pub fn random() -> Result<Self, ProtocolError> {
        let mut bytes = [0_u8; SECRET_BYTES];
        getrandom::fill(&mut bytes).map_err(|error| {
            auth_error(
                "internal.random_unavailable",
                format!("No secure random source: {error}"),
            )
        })?;
        Ok(Self(bytes))
    }

    /// Reads the secret a client needs. A missing file means the engine has
    /// never run for this user.
    pub fn load(path: &Path) -> Result<Self, ProtocolError> {
        let mut file = match std::fs::File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Err(auth_error(
                    "auth.engine_secret_missing",
                    "The Fetchpath engine has not been set up for this user yet. Start it once.",
                )
                .with_action(Action::UpdateSoftware));
            }
            Err(error) => {
                return Err(auth_error(
                    "auth.engine_secret_unreadable",
                    format!("The engine secret could not be read: {error}"),
                ));
            }
        };
        let mut bytes = [0_u8; SECRET_BYTES];
        let mut extra = [0_u8; 1];
        let complete = file.read_exact(&mut bytes).is_ok();
        let trailing = file.read(&mut extra).map(|read| read > 0).unwrap_or(true);
        if !complete || trailing {
            bytes.fill(0);
            return Err(auth_error(
                "auth.engine_secret_invalid",
                "The engine secret file is damaged. Stop the engine and remove it to create a new one.",
            ));
        }
        Ok(Self(bytes))
    }

    /// Loads a secret another process may still be writing. It holds the
    /// file without sharing until it is complete, so a sharing violation
    /// means "wait", for up to two seconds.
    #[cfg(windows)]
    fn load_when_released(path: &Path) -> Result<Self, ProtocolError> {
        const ERROR_SHARING_VIOLATION: i32 = 32;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            match std::fs::File::open(path) {
                Err(error)
                    if error.raw_os_error() == Some(ERROR_SHARING_VIOLATION)
                        && std::time::Instant::now() < deadline =>
                {
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                _ => return Self::load(path),
            }
        }
    }

    /// The engine's side: reads the secret, or creates it readable only by
    /// this user and not by lower-integrity processes.
    #[cfg(windows)]
    pub fn load_or_create(path: &Path, user_sid: &str) -> Result<Self, ProtocolError> {
        match Self::load(path) {
            Err(error) if error.code.as_str() == "auth.engine_secret_missing" => {}
            other => return other,
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| {
                auth_error(
                    "auth.engine_secret_unwritable",
                    format!("The engine data folder could not be created: {error}"),
                )
            })?;
        }
        let secret = Self::random()?;
        // Owner-only access, no inheritance, and a medium label that also
        // forbids reading up, so a sandboxed low-integrity process of the
        // same user cannot read it.
        let security = super::ffi::SecurityDescriptor::from_sddl(&private_file_sddl(user_sid))
            .map_err(|error| {
                auth_error(
                    "auth.engine_secret_unwritable",
                    format!("The engine secret's access list could not be built: {error}"),
                )
            })?;
        match super::ffi::create_new_file(path, &security) {
            Ok(handle) => {
                use std::os::windows::io::FromRawHandle;
                let raw = handle.raw();
                std::mem::forget(handle);
                // SAFETY: ownership of the handle moves into the File.
                let mut file = unsafe { std::fs::File::from_raw_handle(raw) };
                let written = file.write_all(&secret.0).and_then(|()| file.sync_all());
                // Closed before any removal: the file was opened without
                // sharing, so deleting it while open would fail and leave a
                // damaged secret behind.
                drop(file);
                written.map_err(|error| {
                    let _ = std::fs::remove_file(path);
                    auth_error(
                        "auth.engine_secret_unwritable",
                        format!("The engine secret could not be written: {error}"),
                    )
                })?;
                Ok(secret)
            }
            // Another engine start created it first; use that one once its
            // creator has finished writing and closed it.
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                Self::load_when_released(path)
            }
            Err(error) => Err(auth_error(
                "auth.engine_secret_unwritable",
                format!("The engine secret could not be created: {error}"),
            )),
        }
    }

    fn mac(
        &self,
        label: &[u8],
        first: &[u8; NONCE_BYTES],
        second: &[u8; NONCE_BYTES],
    ) -> Hmac<Sha256> {
        let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(&self.0)
            .expect("HMAC accepts a key of any length");
        mac.update(label);
        mac.update(first);
        mac.update(second);
        mac
    }

    pub fn server_proof(&self, client_nonce: &Nonce, server_nonce: &Nonce) -> Vec<u8> {
        self.mac(SERVER_LABEL, &client_nonce.0, &server_nonce.0)
            .finalize()
            .into_bytes()
            .to_vec()
    }

    pub fn client_proof(&self, client_nonce: &Nonce, server_nonce: &Nonce) -> Vec<u8> {
        self.mac(CLIENT_LABEL, &server_nonce.0, &client_nonce.0)
            .finalize()
            .into_bytes()
            .to_vec()
    }

    /// Constant-time check of the server's proof.
    pub fn verify_server(&self, client_nonce: &Nonce, server_nonce: &Nonce, proof: &[u8]) -> bool {
        self.mac(SERVER_LABEL, &client_nonce.0, &server_nonce.0)
            .verify_slice(proof)
            .is_ok()
    }

    /// Constant-time check of the client's proof.
    pub fn verify_client(&self, client_nonce: &Nonce, server_nonce: &Nonce, proof: &[u8]) -> bool {
        self.mac(CLIENT_LABEL, &server_nonce.0, &client_nonce.0)
            .verify_slice(proof)
            .is_ok()
    }
}

impl fmt::Debug for EngineSecret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("EngineSecret(…)")
    }
}

impl Drop for EngineSecret {
    fn drop(&mut self) {
        for byte in &mut self.0 {
            // SAFETY: a valid, aligned pointer into our own array; volatile so
            // the wipe is not optimized away.
            unsafe { std::ptr::write_volatile(byte, 0) };
        }
    }
}

/// A fresh random handshake nonce.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Nonce(pub [u8; NONCE_BYTES]);

impl Nonce {
    pub fn random() -> Result<Self, ProtocolError> {
        let mut bytes = [0_u8; NONCE_BYTES];
        getrandom::fill(&mut bytes).map_err(|error| {
            auth_error(
                "internal.random_unavailable",
                format!("No secure random source: {error}"),
            )
        })?;
        Ok(Self(bytes))
    }

    pub fn to_hex(&self) -> String {
        to_hex(&self.0)
    }

    pub fn from_hex(text: &str) -> Option<Self> {
        let bytes = from_hex(text)?;
        Some(Self(bytes.try_into().ok()?))
    }
}

pub fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub fn from_hex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) || text.len() > 256 {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(text.get(index..index + 2)?, 16).ok())
        .collect()
}

/// Handshake messages, sent as ordinary frames before any protocol message.
/// They belong to the transport, not to the protocol schema.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "handshake", rename_all = "snake_case")]
pub enum Handshake {
    Hello {
        transport: String,
        version: u32,
        client_nonce: String,
    },
    Challenge {
        server_nonce: String,
        server_proof: String,
    },
    Proof {
        client_proof: String,
    },
    Welcome,
}

/// Owner-only access, nothing inherited, and a medium label that also
/// forbids reading up, so a sandboxed low-integrity process of the same user
/// cannot read the file. Used for the secret and the endpoint.
pub fn private_file_sddl(user_sid: &str) -> String {
    format!("D:P(A;;FA;;;{user_sid})S:(ML;;NRNWNX;;;ME)")
}

/// The longest handshake frame either side accepts.
pub const MAX_HANDSHAKE_BYTES: usize = 1_024;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hmac_matches_rfc_4231_test_case_2() {
        let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(b"Jefe").unwrap();
        mac.update(b"what do ya want for nothing?");
        assert_eq!(
            to_hex(&mac.finalize().into_bytes()),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }

    #[test]
    fn proofs_verify_only_with_the_same_secret_nonces_and_direction() {
        let secret = EngineSecret::from_bytes([7; SECRET_BYTES]);
        let other = EngineSecret::from_bytes([8; SECRET_BYTES]);
        let (client, server) = (Nonce([1; NONCE_BYTES]), Nonce([2; NONCE_BYTES]));
        let server_proof = secret.server_proof(&client, &server);
        let client_proof = secret.client_proof(&client, &server);
        assert!(secret.verify_server(&client, &server, &server_proof));
        assert!(secret.verify_client(&client, &server, &client_proof));
        // Wrong secret.
        assert!(!other.verify_server(&client, &server, &server_proof));
        // Reflection: a server proof is not a client proof.
        assert!(!secret.verify_client(&client, &server, &server_proof));
        // Replay under a new nonce.
        let fresh = Nonce([3; NONCE_BYTES]);
        assert!(!secret.verify_client(&client, &fresh, &client_proof));
        assert!(!secret.verify_server(&fresh, &server, &server_proof));
        // Truncated proof.
        assert!(!secret.verify_server(&client, &server, &server_proof[..16]));
    }

    #[test]
    fn hex_round_trips_and_rejects_bad_input() {
        let nonce = Nonce([0xab; NONCE_BYTES]);
        assert_eq!(Nonce::from_hex(&nonce.to_hex()), Some(nonce));
        assert_eq!(Nonce::from_hex("abc"), None);
        assert_eq!(Nonce::from_hex("zz"), None);
        assert_eq!(Nonce::from_hex(&"ab".repeat(31)), None);
        assert_eq!(from_hex("é1"), None);
    }

    #[test]
    fn the_secret_never_prints() {
        let secret = EngineSecret::from_bytes([0x5a; SECRET_BYTES]);
        assert_eq!(format!("{secret:?}"), "EngineSecret(…)");
    }
}
