//! Long-lived device identity and the keys peers pin.

use crate::protect::SecretProtector;
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use sha2::{Digest, Sha256};
use std::fmt;
use std::fs;
use std::io;
use std::path::Path;
use zeroize::Zeroizing;

/// Fills a buffer from the operating system's generator.
pub(crate) fn random_bytes<const N: usize>() -> io::Result<[u8; N]> {
    let mut bytes = [0_u8; N];
    getrandom::fill(&mut bytes).map_err(|error| io::Error::other(error.to_string()))?;
    Ok(bytes)
}

/// This device's ed25519 identity. The signing key never leaves the process
/// except sealed by a [`SecretProtector`], and `Debug` shows only the
/// fingerprint.
pub struct DeviceIdentity {
    signing: SigningKey,
}

impl DeviceIdentity {
    pub fn generate() -> io::Result<Self> {
        let seed = Zeroizing::new(random_bytes::<32>()?);
        Ok(Self {
            signing: SigningKey::from_bytes(&seed),
        })
    }

    /// Loads the sealed identity, or creates and seals a new one.
    ///
    /// A sealed file that cannot be unsealed is an error, not a reason to
    /// generate a replacement: every device that pinned the old key would
    /// silently stop recognising this one.
    pub fn load_or_create(path: &Path, protector: &dyn SecretProtector) -> io::Result<Self> {
        match fs::read(path) {
            Ok(sealed) => {
                let seed = Zeroizing::new(protector.unprotect(&sealed)?);
                let seed: &[u8; 32] = seed.as_slice().try_into().map_err(|_| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        "identity key has the wrong length",
                    )
                })?;
                Ok(Self {
                    signing: SigningKey::from_bytes(seed),
                })
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let identity = Self::generate()?;
                let seed = Zeroizing::new(identity.signing.to_bytes());
                let sealed = protector.protect(seed.as_slice())?;
                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent)?;
                }
                let temporary = path.with_extension("writing");
                fs::write(&temporary, sealed)?;
                fs::OpenOptions::new()
                    .write(true)
                    .open(&temporary)?
                    .sync_all()?;
                fs::rename(&temporary, path)?;
                Ok(identity)
            }
            Err(error) => Err(error),
        }
    }

    pub fn public_key(&self) -> PeerKey {
        PeerKey(self.signing.verifying_key().to_bytes())
    }

    pub fn fingerprint(&self) -> Fingerprint {
        self.public_key().fingerprint()
    }

    pub(crate) fn sign(&self, message: &[u8]) -> [u8; 64] {
        self.signing.sign(message).to_bytes()
    }
}

impl fmt::Debug for DeviceIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DeviceIdentity")
            .field("fingerprint", &self.fingerprint())
            .finish_non_exhaustive()
    }
}

/// A peer's ed25519 public key, as pinned.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PeerKey([u8; 32]);

impl PeerKey {
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    pub fn fingerprint(&self) -> Fingerprint {
        Fingerprint(Sha256::digest(self.0).into())
    }

    pub fn hex(&self) -> String {
        hex(&self.0)
    }

    pub fn parse_hex(text: &str) -> Option<Self> {
        parse_hex32(text).map(Self)
    }

    /// Strict verification: rejects small-order keys and non-canonical
    /// signatures, so a signature cannot be replayed under a malleated form.
    pub(crate) fn verify(&self, message: &[u8], signature: &[u8; 64]) -> bool {
        let Ok(key) = VerifyingKey::from_bytes(&self.0) else {
            return false;
        };
        key.verify_strict(message, &Signature::from_bytes(signature))
            .is_ok()
    }
}

/// SHA-256 of a peer's public key. What people compare and what reports carry.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Fingerprint(pub [u8; 32]);

impl Fingerprint {
    pub fn hex(&self) -> String {
        hex(&self.0)
    }
}

impl fmt::Display for Fingerprint {
    /// The first 80 bits, grouped for reading aloud.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = hex(&self.0[..10]);
        let groups: Vec<&str> = (0..text.len())
            .step_by(4)
            .map(|start| &text[start..start + 4])
            .collect();
        f.write_str(&groups.join("-"))
    }
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn parse_hex32(text: &str) -> Option<[u8; 32]> {
    if text.len() != 64 || !text.is_ascii() {
        return None;
    }
    let mut out = [0_u8; 32];
    for (index, slot) in out.iter_mut().enumerate() {
        *slot = u8::from_str_radix(&text[index * 2..index * 2 + 2], 16).ok()?;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_generated_identities_differ() {
        let a = DeviceIdentity::generate().expect("a");
        let b = DeviceIdentity::generate().expect("b");
        assert_ne!(a.public_key(), b.public_key());
    }

    #[test]
    fn a_signature_verifies_only_for_its_message_and_key() {
        let a = DeviceIdentity::generate().expect("a");
        let b = DeviceIdentity::generate().expect("b");
        let signature = a.sign(b"transcript");
        assert!(a.public_key().verify(b"transcript", &signature));
        assert!(!a.public_key().verify(b"transcrip7", &signature));
        assert!(!b.public_key().verify(b"transcript", &signature));
    }

    #[test]
    fn debug_output_never_contains_the_signing_key() {
        let identity = DeviceIdentity::generate().expect("identity");
        let secret = hex(&identity.signing.to_bytes());
        let shown = format!("{identity:?}");
        assert!(!shown.contains(&secret));
        assert!(shown.contains("fingerprint"));
    }

    #[test]
    fn a_public_key_round_trips_through_hex() {
        let key = DeviceIdentity::generate().expect("identity").public_key();
        assert_eq!(PeerKey::parse_hex(&key.hex()), Some(key));
        assert_eq!(PeerKey::parse_hex("zz"), None);
    }

    #[test]
    fn the_display_fingerprint_is_grouped() {
        let shown = Fingerprint([0xab; 32]).to_string();
        assert_eq!(shown, "abab-abab-abab-abab-abab");
    }
}
