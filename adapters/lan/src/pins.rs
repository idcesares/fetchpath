//! The set of peers this device has explicitly paired with.
//!
//! Pinned keys are public, so the file is not sealed. It is still written by
//! temporary file and rename, and a malformed file is refused rather than
//! partly read: silently dropping lines could hide a tampered entry.

use crate::identity::PeerKey;
use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

const MAX_LABEL_CHARS: usize = 64;

#[derive(Debug)]
pub struct PinStore {
    path: Option<PathBuf>,
    peers: BTreeMap<PeerKey, String>,
}

impl PinStore {
    /// A store that is never written to disk. For tests and one-shot tools.
    pub fn in_memory() -> Self {
        Self {
            path: None,
            peers: BTreeMap::new(),
        }
    }

    pub fn open(path: &Path) -> io::Result<Self> {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == io::ErrorKind::NotFound => String::new(),
            Err(error) => return Err(error),
        };
        let mut peers = BTreeMap::new();
        for line in text.lines().filter(|line| !line.is_empty()) {
            let (key, label) = line.split_once(' ').unwrap_or((line, ""));
            let key = PeerKey::parse_hex(key).ok_or_else(|| malformed(path))?;
            if !valid_label(label) {
                return Err(malformed(path));
            }
            peers.insert(key, label.to_owned());
        }
        Ok(Self {
            path: Some(path.to_path_buf()),
            peers,
        })
    }

    pub fn is_pinned(&self, key: &PeerKey) -> bool {
        self.peers.contains_key(key)
    }

    pub fn label(&self, key: &PeerKey) -> Option<&str> {
        self.peers.get(key).map(String::as_str)
    }

    pub fn peers(&self) -> Vec<(PeerKey, String)> {
        self.peers
            .iter()
            .map(|(key, label)| (*key, label.clone()))
            .collect()
    }

    pub fn pin(&mut self, key: PeerKey, label: &str) -> io::Result<()> {
        let label = sanitize_label(label);
        self.peers.insert(key, label);
        self.persist()
    }

    pub fn unpin(&mut self, key: &PeerKey) -> io::Result<bool> {
        let removed = self.peers.remove(key).is_some();
        if removed {
            self.persist()?;
        }
        Ok(removed)
    }

    fn persist(&self) -> io::Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let mut text = String::new();
        for (key, label) in &self.peers {
            text.push_str(&key.hex());
            if !label.is_empty() {
                text.push(' ');
                text.push_str(label);
            }
            text.push('\n');
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let temporary = path.with_extension("writing");
        fs::write(&temporary, text)?;
        fs::OpenOptions::new()
            .write(true)
            .open(&temporary)?
            .sync_all()?;
        fs::rename(&temporary, path)
    }
}

fn valid_label(label: &str) -> bool {
    label.chars().count() <= MAX_LABEL_CHARS && !label.chars().any(char::is_control)
}

fn sanitize_label(label: &str) -> String {
    label
        .chars()
        .filter(|character| !character.is_control())
        .take(MAX_LABEL_CHARS)
        .collect::<String>()
        .trim()
        .to_owned()
}

fn malformed(path: &Path) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("paired-device list is malformed: {}", path.display()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::DeviceIdentity;

    fn temp_path(label: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("fetchpath-lan-pins-{label}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir.join("peers")
    }

    #[test]
    fn pins_survive_reopening() {
        let path = temp_path("reopen");
        let key = DeviceIdentity::generate().expect("identity").public_key();
        let mut store = PinStore::open(&path).expect("open");
        store.pin(key, "laptop\nevil").expect("pin");
        let reopened = PinStore::open(&path).expect("reopen");
        assert!(reopened.is_pinned(&key));
        assert_eq!(reopened.label(&key), Some("laptopevil"));
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn unpinning_is_persisted() {
        let path = temp_path("unpin");
        let key = DeviceIdentity::generate().expect("identity").public_key();
        let mut store = PinStore::open(&path).expect("open");
        store.pin(key, "desk").expect("pin");
        assert!(store.unpin(&key).expect("unpin"));
        assert!(!PinStore::open(&path).expect("reopen").is_pinned(&key));
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn a_malformed_file_is_refused_rather_than_partly_read() {
        let path = temp_path("malformed");
        fs::create_dir_all(path.parent().unwrap()).expect("dir");
        let key = DeviceIdentity::generate().expect("identity").public_key();
        fs::write(&path, format!("{} ok\nnot-a-key\n", key.hex())).expect("write");
        let error = PinStore::open(&path).expect_err("refused");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }
}
