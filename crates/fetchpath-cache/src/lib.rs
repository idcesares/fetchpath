//! A bounded, content-addressed store for content carrying a trusted digest.
//!
//! The cache keys only on identity supplied by trusted metadata. An observed
//! local digest records what was retained; it is never an identity this crate
//! keys on. Nothing here decides trust: callers supply the check that is run
//! before cached bytes are reused.
//!
//! Completion from this store is reuse, not throughput. It is never evidence
//! of transfer speed, and it is never evidence of publisher authenticity.

mod entry;
mod id;
mod index;

pub use entry::{CacheEntry, CachedVerification, Provenance};
pub use id::ContentId;
pub use index::CacheIndex;

use std::collections::BTreeSet;
use std::fs::{self, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const INDEX_FILE: &str = "index";
const TEMP_FILE: &str = "index.writing";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CacheConfig {
    pub quota_bytes: u64,
    pub max_entry_bytes: u64,
}

impl CacheConfig {
    pub fn new(quota_bytes: u64, max_entry_bytes: u64) -> Self {
        Self {
            quota_bytes,
            max_entry_bytes,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InsertOutcome {
    Inserted,
    AlreadyPresent,
    /// Larger than the per-entry ceiling, or larger than the whole quota. The
    /// store is never emptied to accommodate one file.
    RefusedOversize,
    /// Room could not be freed because every other entry is pinned.
    RefusedQuota,
}

/// A caller-supplied check that cached bytes are the bytes that were promised.
///
/// The cache deliberately cannot perform this itself: the trusted digest or
/// piece map belongs to the request, not to the store. Keeping the policy
/// outside means the store can never be talked into trusting itself.
pub trait TrustedCheck {
    fn verify(&self, path: &Path) -> io::Result<bool>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Acquired {
    /// Verified. The entry is pinned until `release` is called.
    Hit(PathBuf),
    /// Not present, or its bytes are gone. Not an error.
    Miss,
    /// Present but it failed the trusted check. The entry has been evicted.
    FailedCheck,
}

pub struct ContentCache {
    root: PathBuf,
    config: CacheConfig,
    index: CacheIndex,
    pinned: BTreeSet<ContentId>,
}

impl ContentCache {
    pub fn open(root: &Path, config: CacheConfig) -> io::Result<Self> {
        fs::create_dir_all(root)?;
        let decoded = match fs::read_to_string(root.join(INDEX_FILE)) {
            Ok(text) => CacheIndex::decode(&text),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Some(CacheIndex::default()),
            Err(error) => return Err(error),
        };
        let needs_rebuild = decoded.is_none();
        let mut cache = Self {
            root: root.to_path_buf(),
            config,
            index: decoded.unwrap_or_default(),
            pinned: BTreeSet::new(),
        };
        if needs_rebuild {
            cache.rebuild_index()?;
        }
        Ok(cache)
    }

    pub fn lookup(&self, id: &ContentId) -> Option<CacheEntry> {
        self.index.get(id).cloned()
    }

    pub fn total_bytes(&self) -> u64 {
        self.index.total_bytes()
    }

    pub fn entries(&self) -> Vec<CacheEntry> {
        self.index.entries()
    }

    pub fn path_for(&self, id: &ContentId) -> PathBuf {
        self.root.join(id.algorithm_label()).join(id.hex())
    }

    pub fn insert(
        &mut self,
        id: &ContentId,
        source: &Path,
        verification: CachedVerification,
        provenance: Provenance,
    ) -> io::Result<InsertOutcome> {
        if self.index.get(id).is_some() {
            return Ok(InsertOutcome::AlreadyPresent);
        }
        let bytes = fs::metadata(source)?.len();
        if bytes > self.config.max_entry_bytes || bytes > self.config.quota_bytes {
            return Ok(InsertOutcome::RefusedOversize);
        }
        if !self.make_room_for(bytes)? {
            return Ok(InsertOutcome::RefusedQuota);
        }

        let destination = self.path_for(id);
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
        }
        let temporary = destination.with_extension("writing");
        let _ = fs::remove_file(&temporary);
        fs::copy(source, &temporary)?;
        sync_file(&temporary)?;
        // The store is content-addressed, so an existing file at this path is
        // the same content. Replacing it keeps the store coherent. This is not
        // the destination-publication fence, which stays create-only.
        fs::rename(&temporary, &destination)?;

        let now = now_secs();
        self.index.insert(CacheEntry {
            id: *id,
            bytes,
            inserted_at_secs: now,
            last_used_at_secs: now,
            verification,
            provenance,
        });
        self.persist_index()?;
        Ok(InsertOutcome::Inserted)
    }

    /// Marks an entry as in use and returns its stored path.
    ///
    /// A pinned entry is never chosen for eviction, so a reuse in flight cannot
    /// have its bytes deleted underneath it. Pinning also counts as a use,
    /// which is what moves the entry away from the eviction front.
    pub fn pin(&mut self, id: &ContentId) -> Option<PathBuf> {
        self.index.get(id)?;
        self.index.touch(id, now_secs());
        let _ = self.persist_index();
        self.pinned.insert(*id);
        Some(self.path_for(id))
    }

    pub fn unpin(&mut self, id: &ContentId) {
        self.pinned.remove(id);
    }

    pub fn is_pinned(&self, id: &ContentId) -> bool {
        self.pinned.contains(id)
    }

    /// Looks up an identity, re-verifies the stored bytes, and pins them.
    ///
    /// A cached file that fails the check is evicted and reported as a failed
    /// check. It is never published and it is never raised as a download
    /// failure: the caller falls through to the network.
    pub fn acquire_verified(
        &mut self,
        id: &ContentId,
        check: &dyn TrustedCheck,
    ) -> io::Result<Acquired> {
        if self.index.get(id).is_none() {
            return Ok(Acquired::Miss);
        }
        let path = self.path_for(id);
        if !path.exists() {
            self.drop_entry(id)?;
            return Ok(Acquired::Miss);
        }
        if !check.verify(&path)? {
            self.drop_entry(id)?;
            let _ = fs::remove_file(&path);
            return Ok(Acquired::FailedCheck);
        }
        self.index.touch(id, now_secs());
        self.persist_index()?;
        self.pinned.insert(*id);
        Ok(Acquired::Hit(path))
    }

    pub fn release(&mut self, id: &ContentId) {
        self.pinned.remove(id);
    }

    pub fn evict(&mut self, id: &ContentId) -> io::Result<bool> {
        if self.pinned.contains(id) {
            return Ok(false);
        }
        if self.index.remove(id).is_none() {
            return Ok(false);
        }
        match fs::remove_file(self.path_for(id)) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        self.persist_index()?;
        Ok(true)
    }

    /// Evicts least-recently-used entries until `bytes` will fit. Returns false
    /// when everything that remains is pinned.
    fn make_room_for(&mut self, bytes: u64) -> io::Result<bool> {
        while self.index.total_bytes() + bytes > self.config.quota_bytes {
            let Some(victim) = self.index.least_recently_used(&self.pinned) else {
                return Ok(false);
            };
            self.evict(&victim)?;
        }
        Ok(true)
    }

    /// Removes an entry from accounting regardless of pinning. Used only when
    /// the entry is already known to be unusable.
    fn drop_entry(&mut self, id: &ContentId) -> io::Result<()> {
        self.pinned.remove(id);
        self.index.remove(id);
        self.persist_index()
    }

    fn persist_index(&self) -> io::Result<()> {
        let temporary = self.root.join(TEMP_FILE);
        fs::write(&temporary, self.index.encode())?;
        sync_file(&temporary)?;
        fs::rename(&temporary, self.root.join(INDEX_FILE))
    }

    /// Rebuilds accounting by walking the store directory.
    ///
    /// Size is recoverable from the files; provenance and verification are not.
    /// A rebuilt entry is therefore recorded as `Credentialed`, which is never
    /// shareable. Failing closed here is the difference between a rebuild and a
    /// leak.
    fn rebuild_index(&mut self) -> io::Result<()> {
        let mut rebuilt = CacheIndex::default();
        let now = now_secs();
        for algorithm in fs::read_dir(&self.root)?.flatten() {
            if !algorithm.file_type()?.is_dir() {
                continue;
            }
            let Some(label) = algorithm.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            for file in fs::read_dir(algorithm.path())?.flatten() {
                if !file.file_type()?.is_file() {
                    continue;
                }
                let Some(name) = file.file_name().to_str().map(str::to_owned) else {
                    continue;
                };
                let Some(id) = ContentId::parse(&format!("{label}:{name}")) else {
                    let _ = fs::remove_file(file.path());
                    continue;
                };
                rebuilt.insert(CacheEntry {
                    id,
                    bytes: file.metadata()?.len(),
                    inserted_at_secs: now,
                    last_used_at_secs: now,
                    verification: CachedVerification::FinalHashOnly,
                    provenance: Provenance::Credentialed,
                });
            }
        }
        self.index = rebuilt;
        self.persist_index()
    }
}

/// Completes the durability barrier for a file just written.
///
/// The handle must carry write access: a read-only handle cannot be flushed on
/// Windows and fails with a permission error.
fn sync_file(path: &Path) -> io::Result<()> {
    OpenOptions::new().write(true).open(path)?.sync_all()
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_secs())
        .unwrap_or_default()
}
