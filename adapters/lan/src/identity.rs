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
            Ok(sealed) => Self::unseal(&sealed, protector),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let identity = Self::generate()?;
                let seed = Zeroizing::new(identity.signing.to_bytes());
                let sealed = protector.protect(seed.as_slice())?;
                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent)?;
                }
                // A uniquely named temporary, published with a hard link, which
                // fails if the file already exists. Two processes creating an
                // identity at once therefore agree on one key: the loser loads
                // the winner's instead of each keeping a key the other
                // overwrote on disk.
                let temporary =
                    path.with_extension(format!("writing-{}", hex(&random_bytes::<8>()?)));
                let published = fs::write(&temporary, &sealed)
                    .and_then(|()| {
                        fs::OpenOptions::new()
                            .write(true)
                            .open(&temporary)?
                            .sync_all()
                    })
                    .and_then(|()| fs::hard_link(&temporary, path));
                let _ = fs::remove_file(&temporary);
                match published {
                    Ok(()) => Ok(identity),
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                        Self::unseal(&fs::read(path)?, protector)
                    }
                    Err(error) => Err(error),
                }
            }
            Err(error) => Err(error),
        }
    }

    fn unseal(sealed: &[u8], protector: &dyn SecretProtector) -> io::Result<Self> {
        let seed = Zeroizing::new(protector.unprotect(sealed)?);
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

    /// Reversible and machine-independent, so the race below runs anywhere.
    struct Xor;
    impl SecretProtector for Xor {
        fn protect(&self, plaintext: &[u8]) -> io::Result<Vec<u8>> {
            Ok(plaintext.iter().map(|byte| byte ^ 0x5a).collect())
        }
        fn unprotect(&self, protected: &[u8]) -> io::Result<Vec<u8>> {
            self.protect(protected)
        }
    }

    #[test]
    fn concurrent_first_launches_agree_on_one_identity() {
        for round in 0..20 {
            let dir = std::env::temp_dir().join(format!(
                "fetchpath-lan-identity-race-{}-{round}",
                std::process::id()
            ));
            let _ = fs::remove_dir_all(&dir);
            let path = dir.join("identity");
            let keys: Vec<PeerKey> = std::thread::scope(|scope| {
                let handles: Vec<_> = (0..6)
                    .map(|_| scope.spawn(|| DeviceIdentity::load_or_create(&path, &Xor).unwrap()))
                    .collect();
                handles
                    .into_iter()
                    .map(|handle| handle.join().unwrap().public_key())
                    .collect()
            });
            let on_disk = DeviceIdentity::load_or_create(&path, &Xor)
                .unwrap()
                .public_key();
            assert!(
                keys.iter().all(|key| *key == on_disk),
                "every caller holds the key that was persisted"
            );
            assert_eq!(
                fs::read_dir(&dir).unwrap().count(),
                1,
                "no temporaries left"
            );
            let _ = fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn the_display_fingerprint_is_grouped() {
        let shown = Fingerprint([0xab; 32]).to_string();
        assert_eq!(shown, "abab-abab-abab-abab-abab");
    }
}
