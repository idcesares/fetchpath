//! The relay enrollment key at rest (FP-118).
//!
//! Sealed with the same DPAPI helper as the LAN identity and kept in its own
//! file under the LAN directory. The key never appears in the sharing setup,
//! a log or the UI; enrolling again replaces it.

use fetchpath_lan::SecretProtector;
use fetchpath_relay::wire::EnrollmentKey;
use fetchpath_session::share::RelayKeyStore;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use zeroize::Zeroizing;

/// Marks the plaintext so a blob sealed for another purpose with the same
/// entropy label is not mistaken for a relay key.
const MAGIC: &[u8] = b"fpr-key1:";

/// `Ok(None)` when no key was ever stored. A file that cannot be unsealed or
/// parsed is an error: the person must enroll again on purpose.
pub(crate) fn load(
    path: &Path,
    protector: &dyn SecretProtector,
) -> io::Result<Option<EnrollmentKey>> {
    let sealed = match fs::read(path) {
        Ok(sealed) => sealed,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let plain = Zeroizing::new(protector.unprotect(&sealed)?);
    let invalid = || io::Error::new(io::ErrorKind::InvalidData, "the relay key file is damaged");
    let hex = plain.strip_prefix(MAGIC).ok_or_else(invalid)?;
    let hex = std::str::from_utf8(hex).map_err(|_| invalid())?;
    EnrollmentKey::from_hex(hex).map(Some).ok_or_else(invalid)
}

/// Seals and writes the key, replacing any earlier one.
pub(crate) fn store(
    path: &Path,
    key: &EnrollmentKey,
    protector: &dyn SecretProtector,
) -> io::Result<()> {
    let mut plain = Zeroizing::new(MAGIC.to_vec());
    plain.extend_from_slice(Zeroizing::new(key.to_hex()).as_bytes());
    let sealed = protector.protect(&plain)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension("writing");
    fs::write(&temporary, &sealed)?;
    fs::OpenOptions::new()
        .write(true)
        .open(&temporary)?
        .sync_all()?;
    fs::rename(&temporary, path)
}

/// The engine host's key file: what the session may ask is only whether a
/// key is stored; the relay connection loads it itself.
pub(crate) struct KeyFile {
    path: PathBuf,
    protector: Box<dyn SecretProtector + Send + Sync>,
}

impl KeyFile {
    pub(crate) fn new(path: PathBuf, protector: Box<dyn SecretProtector + Send + Sync>) -> Self {
        Self { path, protector }
    }

    pub(crate) fn load(&self) -> io::Result<Option<EnrollmentKey>> {
        load(&self.path, self.protector.as_ref())
    }
}

impl RelayKeyStore for KeyFile {
    fn present(&self) -> bool {
        self.path.is_file()
    }

    fn store(&self, key: &str) -> Result<(), String> {
        let key = EnrollmentKey::from_hex(key).ok_or("the key is not 64 hex characters")?;
        store(&self.path, &key, self.protector.as_ref()).map_err(|error| error.to_string())
    }
}

/// Reversible stand-in for DPAPI in tests: the point is that the file is not
/// the plaintext and that damage is an error.
#[cfg(test)]
pub(crate) struct TestProtector;

#[cfg(test)]
impl SecretProtector for TestProtector {
    fn protect(&self, plaintext: &[u8]) -> io::Result<Vec<u8>> {
        Ok(plaintext.iter().map(|byte| byte ^ 0x5a).collect())
    }
    fn unprotect(&self, protected: &[u8]) -> io::Result<Vec<u8>> {
        self.protect(protected)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use TestProtector as Xor;

    #[test]
    fn a_stored_key_survives_a_restart_and_is_not_written_in_the_clear() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("relay-key");
        assert!(load(&path, &Xor).unwrap().is_none());
        let key = EnrollmentKey::generate().unwrap();
        store(&path, &key, &Xor).unwrap();
        let on_disk = fs::read(&path).unwrap();
        assert!(!String::from_utf8_lossy(&on_disk).contains(&key.to_hex()));
        let loaded = load(&path, &Xor).unwrap().unwrap();
        assert_eq!(loaded.to_hex(), key.to_hex());
        fs::write(&path, Xor.protect(b"something else").unwrap()).unwrap();
        assert!(load(&path, &Xor).is_err());
    }
}
