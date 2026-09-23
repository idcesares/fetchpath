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

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const INDEX_FILE: &str = "index";
const TEMP_FILE: &str = "index.writing";
const LOCK_FILE: &str = "lock";
/// Incoming copies live at the store root, which a rebuild never walks, so a
/// rebuild in another process cannot delete a copy that is still being made.
const INCOMING_PREFIX: &str = "incoming-";
const INCOMING_SUFFIX: &str = ".part";
/// An incoming copy older than this belongs to a process that died mid-copy.
const ABANDONED_AFTER: Duration = Duration::from_secs(24 * 60 * 60);

static INCOMING: AtomicU64 = AtomicU64::new(0);

/// Removes a file on drop unless it has already been moved away.
struct Incoming(PathBuf);

impl Drop for Incoming {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

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
    /// Holders per entry. A local reuse and a peer upload may hold the same
    /// entry at once, so release by one must not unpin it for the other.
    pinned: BTreeMap<ContentId, u32>,
}

impl ContentCache {
    /// Opens a store that other handles, in this process or others, may share.
    ///
    /// Every mutation takes an exclusive lock on the store, reloads the index
    /// from disk, applies its change and persists before releasing, so no
    /// handle can overwrite another's entries with a stale view. Reads use the
    /// view as of the last operation; call [`ContentCache::refresh`] first when
    /// another handle may have written since.
    ///
    /// Pins are per handle. A pin keeps this handle from evicting an entry, but
    /// another handle may still evict it; a reader that loses its file that
    /// way sees a miss, never wrong bytes.
    pub fn open(root: &Path, config: CacheConfig) -> io::Result<Self> {
        fs::create_dir_all(root)?;
        let mut cache = Self {
            root: root.to_path_buf(),
            config,
            index: CacheIndex::default(),
            pinned: BTreeMap::new(),
        };
        let _lock = cache.exclusive()?;
        cache.remove_abandoned_incoming();
        Ok(cache)
    }

    /// Takes the store lock and reloads the index. The lock is released when
    /// the returned handle is dropped.
    fn exclusive(&mut self) -> io::Result<File> {
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(self.root.join(LOCK_FILE))?;
        lock.lock()?;
        self.reload()?;
        Ok(lock)
    }

    fn reload(&mut self) -> io::Result<()> {
        match fs::read_to_string(self.root.join(INDEX_FILE)) {
            Ok(text) => match CacheIndex::decode(&text) {
                Some(index) => self.index = index,
                None => self.rebuild_index()?,
            },
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                self.index = CacheIndex::default();
            }
            Err(error) => return Err(error),
        }
        Ok(())
    }

    /// Brings this handle's view up to date with what every handle has written.
    pub fn refresh(&mut self) -> io::Result<()> {
        self.exclusive().map(drop)
    }

    fn remove_abandoned_incoming(&self) {
        let Ok(listing) = fs::read_dir(&self.root) else {
            return;
        };
        for file in listing.flatten() {
            let name = file.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            let abandoned = file
                .metadata()
                .and_then(|meta| meta.modified())
                .ok()
                .and_then(|modified| modified.elapsed().ok())
                .is_some_and(|age| age > ABANDONED_AFTER);
            if name.starts_with(INCOMING_PREFIX) && name.ends_with(INCOMING_SUFFIX) && abandoned {
                let _ = fs::remove_file(file.path());
            }
        }
    }

    pub fn config(&self) -> CacheConfig {
        self.config
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
        drop(self.exclusive()?);
        if self.index.get(id).is_some() {
            return Ok(InsertOutcome::AlreadyPresent);
        }
        let bytes = fs::metadata(source)?.len();
        if bytes > self.config.max_entry_bytes || bytes > self.config.quota_bytes {
            return Ok(InsertOutcome::RefusedOversize);
        }

        // The copy can be large, so it is made without holding the lock.
        let incoming = Incoming(self.root.join(format!(
            "{INCOMING_PREFIX}{}-{}{INCOMING_SUFFIX}",
            std::process::id(),
            INCOMING.fetch_add(1, Ordering::Relaxed)
        )));
        fs::copy(source, &incoming.0)?;
        sync_file(&incoming.0)?;

        let _lock = self.exclusive()?;
        if self.index.get(id).is_some() {
            return Ok(InsertOutcome::AlreadyPresent);
        }
        if !self.make_room_for(bytes)? {
            return Ok(InsertOutcome::RefusedQuota);
        }
        let destination = self.path_for(id);
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
        }
        // The store is content-addressed, so an existing file at this path is
        // the same content. Replacing it keeps the store coherent. This is not
        // the destination-publication fence, which stays create-only.
        fs::rename(&incoming.0, &destination)?;

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
    /// A pinned entry is never chosen for eviction by this handle, so a reuse
    /// in flight cannot have its bytes deleted underneath it by the same
    /// process. Pinning also counts as a use, which is what moves the entry
    /// away from the eviction front for every handle.
    pub fn pin(&mut self, id: &ContentId) -> Option<PathBuf> {
        let _lock = self.exclusive().ok()?;
        self.index.get(id)?;
        self.index.touch(id, now_secs());
        let _ = self.persist_index();
        self.hold(id);
        Some(self.path_for(id))
    }

    pub fn unpin(&mut self, id: &ContentId) {
        if let Some(holders) = self.pinned.get_mut(id) {
            *holders -= 1;
            if *holders == 0 {
                self.pinned.remove(id);
            }
        }
    }

    fn hold(&mut self, id: &ContentId) {
        *self.pinned.entry(*id).or_insert(0) += 1;
    }

    pub fn is_pinned(&self, id: &ContentId) -> bool {
        self.pinned.contains_key(id)
    }

    /// Looks up an identity, re-verifies the stored bytes, and pins them.
    ///
    /// A cached file that fails the check is evicted and reported as a failed
    /// check. It is never published and it is never raised as a download
    /// failure: the caller falls through to the network. Verification reads the
    /// whole file, so it runs without holding the store lock.
    pub fn acquire_verified(
        &mut self,
        id: &ContentId,
        check: &dyn TrustedCheck,
    ) -> io::Result<Acquired> {
        drop(self.exclusive()?);
        if self.index.get(id).is_none() {
            return Ok(Acquired::Miss);
        }
        let path = self.path_for(id);
        if !path.exists() {
            let _lock = self.exclusive()?;
            self.drop_entry(id)?;
            return Ok(Acquired::Miss);
        }
        let verified = check.verify(&path)?;

        let _lock = self.exclusive()?;
        if self.index.get(id).is_none() {
            // Another handle evicted it while it was being checked.
            return Ok(Acquired::Miss);
        }
        if !verified {
            self.drop_entry(id)?;
            let _ = fs::remove_file(&path);
            return Ok(Acquired::FailedCheck);
        }
        self.index.touch(id, now_secs());
        self.persist_index()?;
        self.hold(id);
        Ok(Acquired::Hit(path))
    }

    pub fn release(&mut self, id: &ContentId) {
        self.unpin(id);
    }

    pub fn evict(&mut self, id: &ContentId) -> io::Result<bool> {
        let _lock = self.exclusive()?;
        self.evict_locked(id)
    }

    fn evict_locked(&mut self, id: &ContentId) -> io::Result<bool> {
        if self.pinned.contains_key(id) {
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
    /// when everything that remains is pinned. Called with the lock held.
    fn make_room_for(&mut self, bytes: u64) -> io::Result<bool> {
        while self.index.total_bytes() + bytes > self.config.quota_bytes {
            let held: BTreeSet<ContentId> = self.pinned.keys().copied().collect();
            let Some(victim) = self.index.least_recently_used(&held) else {
                return Ok(false);
            };
            self.evict_locked(&victim)?;
        }
        Ok(true)
    }

    /// Removes an entry from accounting regardless of pinning. Used only when
    /// the entry is already known to be unusable, with the lock held.
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
