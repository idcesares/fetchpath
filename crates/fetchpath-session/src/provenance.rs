//! Provenance that travels with the bytes (contract D8, FP-121).
//!
//! A download that used a cookie, a stored credential, a referer or link
//! credentials marks its output twice when it finishes: a named stream on the
//! file (survives a rename, a move and an Explorer copy), and the file's
//! SHA-256 in a small persisted set (survives a copy that drops streams, and
//! the removal of the queue row). A path offer consults both.

use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

pub(crate) const FILE: &str = "provenance-v1.json";

/// The stream's name and its constant content. It carries no link, no
/// credential reference and no path.
#[cfg(windows)]
const STREAM: &str = ":fetchpath.credentialed";
#[cfg(windows)]
const STREAM_BODY: &[u8] = b"1";

#[derive(serde::Serialize, serde::Deserialize)]
struct Persisted {
    version: u32,
    /// Lowercase hex SHA-256 of every credentialed output.
    hashes: BTreeSet<String>,
}

#[derive(Default)]
pub(crate) struct Provenance {
    path: Option<PathBuf>,
    hashes: BTreeSet<String>,
    /// The file exists but could not be read. Nothing is known about the
    /// outputs it listed, so agents offer nothing by path, and the file is
    /// never overwritten with a smaller set.
    unreadable: bool,
}

impl Provenance {
    pub(crate) fn load(dir: Option<&Path>) -> Self {
        let path = dir.map(|dir| dir.join(FILE));
        let mut loaded = Self {
            path: path.clone(),
            ..Self::default()
        };
        let Some(path) = path else { return loaded };
        match std::fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice::<Persisted>(&bytes) {
                Ok(saved) if saved.version == 1 => loaded.hashes = saved.hashes,
                _ => loaded.unreadable = true,
            },
            Err(failure) if failure.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => loaded.unreadable = true,
        }
        loaded
    }

    pub(crate) fn unreadable(&self) -> bool {
        self.unreadable
    }

    pub(crate) fn contains(&self, sha256: &str) -> bool {
        self.hashes.contains(sha256)
    }

    /// Adds a hash and writes the set before returning, so a caller that sees
    /// `Ok` may rely on the disk. Memory is left as it was on failure.
    pub(crate) fn remember(&mut self, sha256: &str) -> Result<(), String> {
        if self.unreadable {
            return Err("The provenance file could not be read, so it is left alone.".into());
        }
        if self.hashes.contains(sha256) {
            return Ok(());
        }
        let mut next = self.hashes.clone();
        next.insert(sha256.to_owned());
        if let Some(path) = &self.path {
            let bytes = serde_json::to_vec(&Persisted {
                version: 1,
                hashes: next.clone(),
            })
            .map_err(|failure| failure.to_string())?;
            let temporary = path.with_extension("tmp");
            let write = std::fs::File::create(&temporary).and_then(|mut file| {
                file.write_all(&bytes)?;
                file.sync_all()
            });
            write
                .and_then(|()| std::fs::rename(&temporary, path))
                .map_err(|failure| format!("The provenance file could not be saved: {failure}"))?;
        }
        self.hashes = next;
        Ok(())
    }
}

/// What the file itself says about where it came from.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Marker {
    Credentialed,
    /// No marker: the engine never marked it, or the volume has no streams.
    Absent,
    /// The marker could not be read for a reason other than being absent.
    Unknown,
}

#[cfg(windows)]
fn stream_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(STREAM);
    PathBuf::from(name)
}

/// Marks the file. A volume without named streams (FAT, exFAT) refuses; the
/// caller then relies on the hash.
#[cfg(windows)]
pub(crate) fn mark_file(path: &Path) -> std::io::Result<()> {
    std::fs::write(stream_path(path), STREAM_BODY)
}

#[cfg(not(windows))]
pub(crate) fn mark_file(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

/// Writes the stream unless the file already carries it. True when the file
/// carries it afterwards.
pub(crate) fn mark_if_needed(path: &Path) -> bool {
    read_marker(path) == Marker::Credentialed || mark_file(path).is_ok()
}

#[cfg(windows)]
pub(crate) fn read_marker(path: &Path) -> Marker {
    use std::io::ErrorKind;
    match std::fs::read(stream_path(path)) {
        Ok(body) if body == STREAM_BODY => Marker::Credentialed,
        // Something else is in the stream: not ours, but not plain either.
        Ok(_) => Marker::Unknown,
        Err(failure) => match failure.kind() {
            ErrorKind::NotFound => Marker::Absent,
            // 50 not supported, 87 invalid parameter, 123 invalid name: the
            // volume does not do named streams, so the file cannot carry one.
            _ if matches!(failure.raw_os_error(), Some(50 | 87 | 123)) => Marker::Absent,
            _ => Marker::Unknown,
        },
    }
}

#[cfg(not(windows))]
pub(crate) fn read_marker(_path: &Path) -> Marker {
    Marker::Absent
}

/// SHA-256 of a whole file, lowercase hex.
pub(crate) fn hash_file(path: &Path) -> std::io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = vec![0u8; 256 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            return Ok(format!("{:x}", digest.finalize()));
        }
        digest.update(&buffer[..read]);
    }
}

/// Every regular file under `root` (the file itself, or a torrent's folder).
pub(crate) fn files_under(root: &Path) -> std::io::Result<Vec<PathBuf>> {
    let meta = std::fs::symlink_metadata(root)?;
    if meta.is_file() {
        return Ok(vec![root.to_path_buf()]);
    }
    let mut found = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(folder) = pending.pop() {
        for entry in std::fs::read_dir(&folder)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_dir() {
                pending.push(entry.path());
            } else if kind.is_file() {
                found.push(entry.path());
            }
        }
    }
    Ok(found)
}
