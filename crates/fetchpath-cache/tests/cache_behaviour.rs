use fetchpath_cache::{
    Acquired, CacheConfig, CachedVerification, ContentCache, ContentId, InsertOutcome, Provenance,
    TrustedCheck,
};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

static COUNTER: AtomicU32 = AtomicU32::new(0);

struct TempDir(PathBuf);

impl TempDir {
    fn new(label: &str) -> Self {
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "fetchpath-cache-{label}-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("temp dir");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn source_file(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, bytes).expect("write source");
    path
}

fn id(tag: u8) -> ContentId {
    ContentId::FlatSha256([tag; 32])
}

struct AlwaysValid;
impl TrustedCheck for AlwaysValid {
    fn verify(&self, _path: &Path) -> io::Result<bool> {
        Ok(true)
    }
}

struct AlwaysInvalid;
impl TrustedCheck for AlwaysInvalid {
    fn verify(&self, _path: &Path) -> io::Result<bool> {
        Ok(false)
    }
}

/// Verifies by exact content, which is what a real trusted check does.
struct ExpectBytes(&'static [u8]);
impl TrustedCheck for ExpectBytes {
    fn verify(&self, path: &Path) -> io::Result<bool> {
        Ok(fs::read(path)? == self.0)
    }
}

#[test]
fn inserted_bytes_are_retrievable_and_accounted() {
    let dir = TempDir::new("insert");
    let source = source_file(dir.path(), "payload.bin", b"hello cache");
    let mut cache =
        ContentCache::open(&dir.path().join("store"), CacheConfig::new(1024, 1024)).expect("open");

    let outcome = cache
        .insert(
            &id(1),
            &source,
            CachedVerification::FinalHashOnly,
            Provenance::Public,
        )
        .expect("insert");

    assert_eq!(outcome, InsertOutcome::Inserted);
    assert_eq!(cache.total_bytes(), 11);

    let entry = cache.lookup(&id(1)).expect("present");
    assert_eq!(entry.bytes, 11);
    assert_eq!(entry.provenance, Provenance::Public);
    assert_eq!(entry.verification, CachedVerification::FinalHashOnly);
    assert!(cache.lookup(&id(2)).is_none());
}

#[test]
fn the_source_file_is_left_alone_by_insertion() {
    let dir = TempDir::new("source-intact");
    let source = source_file(dir.path(), "payload.bin", b"hello cache");
    let mut cache =
        ContentCache::open(&dir.path().join("store"), CacheConfig::new(1024, 1024)).expect("open");

    cache
        .insert(
            &id(1),
            &source,
            CachedVerification::FinalHashOnly,
            Provenance::Public,
        )
        .expect("insert");

    assert_eq!(
        fs::read(&source).expect("source still readable"),
        b"hello cache"
    );
}

#[test]
fn inserting_the_same_identity_twice_does_not_double_count() {
    let dir = TempDir::new("idempotent");
    let source = source_file(dir.path(), "payload.bin", b"hello cache");
    let mut cache =
        ContentCache::open(&dir.path().join("store"), CacheConfig::new(1024, 1024)).expect("open");

    cache
        .insert(
            &id(1),
            &source,
            CachedVerification::FinalHashOnly,
            Provenance::Public,
        )
        .expect("first insert");
    let second = cache
        .insert(
            &id(1),
            &source,
            CachedVerification::FinalHashOnly,
            Provenance::Public,
        )
        .expect("second insert");

    assert_eq!(second, InsertOutcome::AlreadyPresent);
    assert_eq!(cache.total_bytes(), 11);
}

#[test]
fn provenance_recorded_at_insertion_survives_reopening() {
    let dir = TempDir::new("provenance-durable");
    let store = dir.path().join("store");
    let source = source_file(dir.path(), "payload.bin", b"private bytes");

    {
        let mut cache = ContentCache::open(&store, CacheConfig::new(1024, 1024)).expect("open");
        cache
            .insert(
                &id(1),
                &source,
                CachedVerification::PieceHashes,
                Provenance::Credentialed,
            )
            .expect("insert");
    }

    let reopened = ContentCache::open(&store, CacheConfig::new(1024, 1024)).expect("reopen");
    let entry = reopened.lookup(&id(1)).expect("present");
    assert_eq!(entry.provenance, Provenance::Credentialed);
    assert!(!entry.is_shareable());
}

#[test]
fn a_corrupted_index_is_rebuilt_from_the_store_directory() {
    let dir = TempDir::new("self-repair");
    let store = dir.path().join("store");
    let source = source_file(dir.path(), "payload.bin", b"hello cache");

    {
        let mut cache = ContentCache::open(&store, CacheConfig::new(1024, 1024)).expect("open");
        cache
            .insert(
                &id(1),
                &source,
                CachedVerification::FinalHashOnly,
                Provenance::Public,
            )
            .expect("insert");
    }

    fs::write(store.join("index"), b"total garbage").expect("corrupt the index");

    let reopened = ContentCache::open(&store, CacheConfig::new(1024, 1024)).expect("reopen");
    let entry = reopened.lookup(&id(1)).expect("rebuilt from directory");
    assert_eq!(entry.bytes, 11);
    assert_eq!(reopened.total_bytes(), 11);
    // Provenance cannot be recovered from bytes alone, so a rebuilt entry is
    // assumed credentialed and is never shareable.
    assert_eq!(entry.provenance, Provenance::Credentialed);
    assert!(!entry.is_shareable());
}

#[test]
fn an_entry_larger_than_the_ceiling_is_refused_without_emptying_the_store() {
    let dir = TempDir::new("oversize");
    let small = source_file(dir.path(), "small.bin", b"12345");
    let big = source_file(dir.path(), "big.bin", &[7_u8; 500]);
    let mut cache =
        ContentCache::open(&dir.path().join("store"), CacheConfig::new(1000, 100)).expect("open");

    cache
        .insert(
            &id(1),
            &small,
            CachedVerification::FinalHashOnly,
            Provenance::Public,
        )
        .expect("insert small");

    let outcome = cache
        .insert(
            &id(2),
            &big,
            CachedVerification::FinalHashOnly,
            Provenance::Public,
        )
        .expect("insert big");

    assert_eq!(outcome, InsertOutcome::RefusedOversize);
    assert!(cache.lookup(&id(1)).is_some(), "existing entry survived");
    assert!(cache.lookup(&id(2)).is_none());
    assert_eq!(cache.total_bytes(), 5);
}

#[test]
fn an_entry_larger_than_the_whole_quota_is_refused_as_oversize() {
    let dir = TempDir::new("over-quota");
    let big = source_file(dir.path(), "big.bin", &[7_u8; 500]);
    let mut cache =
        ContentCache::open(&dir.path().join("store"), CacheConfig::new(100, 1000)).expect("open");

    let outcome = cache
        .insert(
            &id(1),
            &big,
            CachedVerification::FinalHashOnly,
            Provenance::Public,
        )
        .expect("insert");

    assert_eq!(outcome, InsertOutcome::RefusedOversize);
    assert_eq!(cache.total_bytes(), 0);
}

#[test]
fn a_quota_breach_evicts_the_least_recently_used_entry_and_only_that_one() {
    let dir = TempDir::new("evict-lru");
    let forty = source_file(dir.path(), "forty.bin", &[1_u8; 40]);
    let mut cache =
        ContentCache::open(&dir.path().join("store"), CacheConfig::new(100, 100)).expect("open");

    // Distinct last-used times so "least recently used" is unambiguous rather
    // than decided by the content-id tiebreak.
    for tag in [1_u8, 2] {
        cache
            .insert(
                &id(tag),
                &forty,
                CachedVerification::FinalHashOnly,
                Provenance::Public,
            )
            .expect("insert");
        std::thread::sleep(std::time::Duration::from_millis(1100));
    }
    assert_eq!(cache.total_bytes(), 80);

    // Touch entry 1 so entry 2 becomes the least recently used.
    cache.pin(&id(1)).expect("pin");
    cache.unpin(&id(1));

    cache
        .insert(
            &id(3),
            &forty,
            CachedVerification::FinalHashOnly,
            Provenance::Public,
        )
        .expect("insert third");

    assert_eq!(cache.total_bytes(), 80, "quota held");
    assert!(cache.lookup(&id(3)).is_some(), "new entry stored");
    assert!(
        cache.lookup(&id(2)).is_none(),
        "least recently used evicted"
    );
    assert!(cache.lookup(&id(1)).is_some(), "only one entry evicted");
}

#[test]
fn a_pinned_entry_is_never_evicted_to_make_room() {
    let dir = TempDir::new("pin-protects");
    let sixty = source_file(dir.path(), "sixty.bin", &[1_u8; 60]);
    let mut cache =
        ContentCache::open(&dir.path().join("store"), CacheConfig::new(100, 100)).expect("open");

    cache
        .insert(
            &id(1),
            &sixty,
            CachedVerification::FinalHashOnly,
            Provenance::Public,
        )
        .expect("insert");
    let pinned_path = cache.pin(&id(1)).expect("pin returns the stored path");
    assert!(pinned_path.exists());

    let outcome = cache
        .insert(
            &id(2),
            &sixty,
            CachedVerification::FinalHashOnly,
            Provenance::Public,
        )
        .expect("insert second");

    assert_eq!(outcome, InsertOutcome::RefusedQuota);
    assert!(cache.lookup(&id(1)).is_some(), "pinned entry survived");
    assert!(pinned_path.exists(), "pinned bytes still on disk");

    cache.unpin(&id(1));
    let after = cache
        .insert(
            &id(2),
            &sixty,
            CachedVerification::FinalHashOnly,
            Provenance::Public,
        )
        .expect("insert after unpin");
    assert_eq!(after, InsertOutcome::Inserted);
    assert!(cache.lookup(&id(1)).is_none(), "now evictable");
}

#[test]
fn pinning_an_absent_entry_yields_nothing() {
    let dir = TempDir::new("pin-absent");
    let mut cache =
        ContentCache::open(&dir.path().join("store"), CacheConfig::new(100, 100)).expect("open");

    assert!(cache.pin(&id(9)).is_none());
    assert!(!cache.is_pinned(&id(9)));
}

#[test]
fn a_verified_hit_returns_pinned_bytes() {
    let dir = TempDir::new("acquire-hit");
    let source = source_file(dir.path(), "payload.bin", b"hello cache");
    let mut cache =
        ContentCache::open(&dir.path().join("store"), CacheConfig::new(1024, 1024)).expect("open");
    cache
        .insert(
            &id(1),
            &source,
            CachedVerification::FinalHashOnly,
            Provenance::Public,
        )
        .expect("insert");

    let acquired = cache
        .acquire_verified(&id(1), &ExpectBytes(b"hello cache"))
        .expect("acquire");

    let Acquired::Hit(path) = acquired else {
        panic!("expected a hit, got {acquired:?}");
    };
    assert_eq!(fs::read(&path).expect("read"), b"hello cache");
    assert!(cache.is_pinned(&id(1)), "a hit is pinned until released");

    cache.release(&id(1));
    assert!(!cache.is_pinned(&id(1)));
}

#[test]
fn an_absent_identity_is_a_miss_and_is_not_an_error() {
    let dir = TempDir::new("acquire-miss");
    let mut cache =
        ContentCache::open(&dir.path().join("store"), CacheConfig::new(1024, 1024)).expect("open");

    assert_eq!(
        cache
            .acquire_verified(&id(1), &AlwaysValid)
            .expect("acquire"),
        Acquired::Miss
    );
}

#[test]
fn a_tampered_entry_is_evicted_and_never_returned() {
    let dir = TempDir::new("tampered");
    let store = dir.path().join("store");
    let source = source_file(dir.path(), "payload.bin", b"hello cache");
    let mut cache = ContentCache::open(&store, CacheConfig::new(1024, 1024)).expect("open");
    cache
        .insert(
            &id(1),
            &source,
            CachedVerification::FinalHashOnly,
            Provenance::Public,
        )
        .expect("insert");

    let stored = cache.path_for(&id(1));
    fs::write(&stored, b"tampered!!!").expect("tamper with the stored bytes");

    let acquired = cache
        .acquire_verified(&id(1), &ExpectBytes(b"hello cache"))
        .expect("acquire");

    assert_eq!(acquired, Acquired::FailedCheck);
    assert!(cache.lookup(&id(1)).is_none(), "entry evicted");
    assert!(!stored.exists(), "tampered bytes removed");
    assert_eq!(cache.total_bytes(), 0, "accounting corrected");
    assert!(
        !cache.is_pinned(&id(1)),
        "a failed check leaves nothing pinned"
    );
}

#[test]
fn an_entry_whose_file_vanished_is_a_miss_rather_than_an_error() {
    let dir = TempDir::new("vanished");
    let store = dir.path().join("store");
    let source = source_file(dir.path(), "payload.bin", b"hello cache");
    let mut cache = ContentCache::open(&store, CacheConfig::new(1024, 1024)).expect("open");
    cache
        .insert(
            &id(1),
            &source,
            CachedVerification::FinalHashOnly,
            Provenance::Public,
        )
        .expect("insert");

    fs::remove_file(cache.path_for(&id(1))).expect("remove stored bytes");

    assert_eq!(
        cache
            .acquire_verified(&id(1), &AlwaysValid)
            .expect("acquire"),
        Acquired::Miss
    );
    assert!(cache.lookup(&id(1)).is_none(), "stale entry dropped");
}

#[test]
fn a_failed_check_never_leaves_usable_bytes_behind() {
    let dir = TempDir::new("failed-check");
    let source = source_file(dir.path(), "payload.bin", b"hello cache");
    let mut cache =
        ContentCache::open(&dir.path().join("store"), CacheConfig::new(1024, 1024)).expect("open");
    cache
        .insert(
            &id(1),
            &source,
            CachedVerification::PieceHashes,
            Provenance::Public,
        )
        .expect("insert");

    assert_eq!(
        cache
            .acquire_verified(&id(1), &AlwaysInvalid)
            .expect("acquire"),
        Acquired::FailedCheck
    );
    assert_eq!(
        cache
            .acquire_verified(&id(1), &AlwaysValid)
            .expect("acquire"),
        Acquired::Miss,
        "a second attempt cannot resurrect the evicted entry"
    );
}
