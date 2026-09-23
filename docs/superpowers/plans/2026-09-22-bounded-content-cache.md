# Bounded Content Cache Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a bounded, content-addressed local cache that lets a verified download complete from disk with no network access, under an enforced quota with least-recently-used eviction, and never retains or reuses bytes it cannot prove.

**Architecture:** A new `crates/fetchpath-cache` crate owns a store keyed by a *trusted* content id, a self-repairing index, quota accounting and eviction. It performs no networking and holds no trust policy of its own: the caller supplies a `TrustedCheck` that re-verifies cached bytes before they are ever published. `crates/fetchpath-core` consults the cache before contacting any mirror and inserts into it after a verified completion.

**Tech Stack:** Rust 2024 edition, `sha2`, `fetchpath-metalink` (for `PieceMap`), `fetchpath-storage` (for the existing create-only publication fence). No new third-party dependencies.

**Spec:** `docs/superpowers/specs/2026-09-22-content-cache-and-paired-lan-design.md`

**Backlog task:** FP-020 (`docs/tasks/backlog.json`), owner `cache_lan`.

## Global Constraints

- Rust edition 2024, `rust-version = "1.98"`, resolver 3. Copy these from the workspace `[workspace.package]` table; do not restate different values.
- **No new third-party dependencies.** Every crate used must already appear in `Cargo.lock`. `sha2` is already a direct dependency of `fetchpath-storage` and `fetchpath-metalink`.
- `cargo clippy --workspace --all-targets -- -D warnings` must pass. Warnings are errors.
- `cargo fmt --check` must pass.
- `cargo test --workspace --locked` must pass. Baseline before this work: 120 tests, exit 0.
- **No speed claim anywhere.** A cache hit is reuse, not throughput. Do not write "faster", "accelerated", or a transfer rate for a non-network completion, in code, comments, docs or test names.
- **No publisher-authenticity claim.** A trusted digest establishes representation identity, not who published the object. Existing comments in `verified.rs` use this wording; match it.
- Only content carrying a trusted digest is eligible for the cache. `VerificationLevel::Unverified` content is never inserted.
- `Provenance` is decided at insertion and is immutable thereafter.
- Secrets never reach logs, errors or evidence. `RequestContext` is deliberately not `Debug`; keep it that way.
- Target platform is Windows 11 x64. Paths are `PathBuf`; do not assume `/`-separated strings.

---

## File Structure

**Created:**

| Path | Responsibility |
|---|---|
| `crates/fetchpath-cache/Cargo.toml` | Crate manifest |
| `crates/fetchpath-cache/src/lib.rs` | `ContentCache`, `CacheConfig`, outcomes, module wiring |
| `crates/fetchpath-cache/src/id.rs` | `ContentId` — construction-distinct identity and its rendering |
| `crates/fetchpath-cache/src/entry.rs` | `CacheEntry`, `Provenance`, `CachedVerification` |
| `crates/fetchpath-cache/src/index.rs` | Index encode/decode, generation write, self-repair rebuild |
| `crates/fetchpath-cache/tests/cache_behaviour.rs` | Behavioural tests for the store |

**Modified:**

| Path | Change |
|---|---|
| `Cargo.toml` | Add `crates/fetchpath-cache` to `workspace.members` |
| `crates/fetchpath-core/Cargo.toml` | Add `fetchpath-cache` path dependency |
| `crates/fetchpath-core/src/lib.rs` | `RequestContext::is_credential_free`; re-export cache types |
| `crates/fetchpath-core/src/verified.rs` | `DeliverySource`; cache lookup before mirrors; insert after success; `publish` takes a url instead of `&Mirror` |

Phase C (`adapters/lan`) and the CLI controls are **not** in this plan. They are covered by a follow-up plan written after this one lands.

---

### Task 1: Content identity

Creates the cache crate and the one type everything else is keyed on. `ContentId` must encode algorithm *and* construction, because a flat SHA-256 and a digest over a piece map name the same bytes by different means and must never be interchangeable.

**Files:**
- Create: `crates/fetchpath-cache/Cargo.toml`
- Create: `crates/fetchpath-cache/src/lib.rs`
- Create: `crates/fetchpath-cache/src/id.rs`
- Modify: `Cargo.toml` (workspace members)

**Interfaces:**
- Consumes: `fetchpath_metalink::PieceMap` — `piece_length() -> u64`, `total_size() -> u64`, `piece_count() -> usize`, `piece_hash(usize) -> Option<&[u8; 32]>`.
- Produces:
  - `ContentId::FlatSha256([u8; 32])`, `ContentId::PieceMapSha256([u8; 32])`
  - `ContentId::from_expected_sha256(&str) -> Option<ContentId>`
  - `ContentId::from_piece_map(&PieceMap) -> ContentId`
  - `ContentId::render(&self) -> String`
  - `ContentId::parse(&str) -> Option<ContentId>`
  - `ContentId::algorithm_label(&self) -> &'static str`
  - `ContentId::hex(&self) -> String`

- [ ] **Step 1: Write the failing test**

Create `crates/fetchpath-cache/src/id.rs` with only the test module for now:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn map(piece_length: u64, total: u64, hashes: Vec<[u8; 32]>) -> PieceMap {
        PieceMap::new(piece_length, total, hashes).expect("valid piece map")
    }

    #[test]
    fn a_flat_digest_and_a_piece_map_digest_are_never_the_same_identity() {
        let raw = [7_u8; 32];
        let flat = ContentId::FlatSha256(raw);
        let pieces = ContentId::PieceMapSha256(raw);

        assert_ne!(flat, pieces);
        assert_ne!(flat.render(), pieces.render());
        assert_eq!(flat.algorithm_label(), "sha256");
        assert_eq!(pieces.algorithm_label(), "pieces-sha256");
        assert_eq!(flat.hex(), pieces.hex());
    }

    #[test]
    fn rendering_round_trips_through_parsing() {
        let id = ContentId::FlatSha256([3_u8; 32]);
        assert_eq!(ContentId::parse(&id.render()), Some(id));

        let pieces = ContentId::PieceMapSha256([9_u8; 32]);
        assert_eq!(ContentId::parse(&pieces.render()), Some(pieces));
    }

    #[test]
    fn malformed_identity_text_is_refused_rather_than_guessed() {
        for text in [
            "",
            "sha256:",
            "sha256:zz",
            "md5:0000000000000000000000000000000000000000000000000000000000000000",
            "0000000000000000000000000000000000000000000000000000000000000000",
            "sha256:00000000000000000000000000000000000000000000000000000000000000",
        ] {
            assert_eq!(ContentId::parse(text), None, "accepted {text:?}");
        }
    }

    #[test]
    fn an_expected_whole_file_digest_becomes_a_flat_identity() {
        let hex = "a".repeat(64);
        let id = ContentId::from_expected_sha256(&hex).expect("valid digest");
        assert_eq!(id, ContentId::FlatSha256([0xaa; 32]));

        assert_eq!(ContentId::from_expected_sha256("a"), None);
        assert_eq!(ContentId::from_expected_sha256(&"g".repeat(64)), None);
    }

    #[test]
    fn piece_map_identity_changes_when_any_trusted_input_changes() {
        let base = ContentId::from_piece_map(&map(4, 8, vec![[1_u8; 32], [2_u8; 32]]));

        let reordered = ContentId::from_piece_map(&map(4, 8, vec![[2_u8; 32], [1_u8; 32]]));
        assert_ne!(base, reordered, "piece order must change identity");

        let different_length = ContentId::from_piece_map(&map(8, 16, vec![[1_u8; 32], [2_u8; 32]]));
        assert_ne!(base, different_length, "piece length must change identity");

        let same = ContentId::from_piece_map(&map(4, 8, vec![[1_u8; 32], [2_u8; 32]]));
        assert_eq!(base, same, "identical trusted input must be stable");
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

First add the crate so it compiles at all.

`crates/fetchpath-cache/Cargo.toml`:

```toml
[package]
name = "fetchpath-cache"
version = "0.1.0"
edition.workspace = true
license.workspace = true
rust-version.workspace = true

[dependencies]
sha2 = "0.10"
fetchpath-metalink = { path = "../fetchpath-metalink" }
```

Check the exact `sha2` version pinned in `Cargo.lock` and use the same major/minor so `--locked` does not fail.

`crates/fetchpath-cache/src/lib.rs`:

```rust
//! A bounded, content-addressed store for content carrying a trusted digest.
//!
//! The cache keys only on identity supplied by trusted metadata. An observed
//! local digest records what was retained; it is never an identity this crate
//! keys on. Nothing here verifies trust on its own: callers supply the check.

mod id;

pub use id::ContentId;
```

Add `"crates/fetchpath-cache"` to `workspace.members` in the root `Cargo.toml`.

Run: `cargo test -p fetchpath-cache --locked`
Expected: FAIL to compile — `ContentId` has no variants, methods, or `PieceMap` import.

- [ ] **Step 3: Write minimal implementation**

At the top of `crates/fetchpath-cache/src/id.rs`, above the test module:

```rust
use fetchpath_metalink::PieceMap;
use sha2::{Digest, Sha256};

/// Domain separation for the piece-map construction. Changing this string
/// changes every piece-map identity, so it is versioned deliberately.
const PIECE_MAP_DOMAIN: &[u8] = b"fetchpath-piecemap-v1";

/// A trusted content identity.
///
/// The variant is part of the identity. A flat digest over the bytes and a
/// digest over a trusted piece map describe the same bytes by different
/// constructions and are never interchangeable.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum ContentId {
    /// SHA-256 over the whole artifact, as stated by trusted metadata.
    FlatSha256([u8; 32]),
    /// SHA-256 over the canonical encoding of a trusted piece map.
    PieceMapSha256([u8; 32]),
}

impl ContentId {
    pub fn algorithm_label(&self) -> &'static str {
        match self {
            Self::FlatSha256(_) => "sha256",
            Self::PieceMapSha256(_) => "pieces-sha256",
        }
    }

    fn digest(&self) -> &[u8; 32] {
        match self {
            Self::FlatSha256(bytes) | Self::PieceMapSha256(bytes) => bytes,
        }
    }

    pub fn hex(&self) -> String {
        let mut out = String::with_capacity(64);
        for byte in self.digest() {
            use std::fmt::Write;
            let _ = write!(out, "{byte:02x}");
        }
        out
    }

    pub fn render(&self) -> String {
        format!("{}:{}", self.algorithm_label(), self.hex())
    }

    pub fn parse(text: &str) -> Option<Self> {
        let (label, hex) = text.split_once(':')?;
        let digest = decode_hex32(hex)?;
        match label {
            "sha256" => Some(Self::FlatSha256(digest)),
            "pieces-sha256" => Some(Self::PieceMapSha256(digest)),
            _ => None,
        }
    }

    /// Builds a flat identity from a whole-file digest supplied by trusted
    /// metadata. Returns `None` for anything that is not 64 hex characters.
    pub fn from_expected_sha256(hex: &str) -> Option<Self> {
        decode_hex32(hex).map(Self::FlatSha256)
    }

    /// Builds an identity over a trusted piece map. Piece length, total size,
    /// piece count and the ordered piece hashes all contribute, so any change
    /// to the trusted input produces a different identity.
    pub fn from_piece_map(map: &PieceMap) -> Self {
        let mut digest = Sha256::new();
        digest.update(PIECE_MAP_DOMAIN);
        digest.update(map.piece_length().to_le_bytes());
        digest.update(map.total_size().to_le_bytes());
        digest.update((map.piece_count() as u64).to_le_bytes());
        for index in 0..map.piece_count() {
            let hash = map.piece_hash(index).expect("index below piece count");
            digest.update(hash);
        }
        Self::PieceMapSha256(digest.finalize().into())
    }
}

fn decode_hex32(hex: &str) -> Option<[u8; 32]> {
    if hex.len() != 64 {
        return None;
    }
    let bytes = hex.as_bytes();
    let mut out = [0_u8; 32];
    for (index, slot) in out.iter_mut().enumerate() {
        let high = (bytes[index * 2] as char).to_digit(16)?;
        let low = (bytes[index * 2 + 1] as char).to_digit(16)?;
        *slot = ((high << 4) | low) as u8;
    }
    Some(out)
}
```

Confirm `PieceMap::new` is public and takes `(piece_length, total_size, hashes)` in that order by reading `crates/fetchpath-metalink/src/pieces.rs:29`. If the signature differs, adjust the test helper, not the production code.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p fetchpath-cache --locked`
Expected: PASS, 5 tests.

Run: `cargo clippy -p fetchpath-cache --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock crates/fetchpath-cache
git commit -m "feat(cache): add construction-distinct content identity"
```

---

### Task 2: Entry model and credential-free provenance

The store records what an entry is and where it came from. Provenance is the whole privacy story, so it is decided once at insertion and can never be changed.

**Files:**
- Create: `crates/fetchpath-cache/src/entry.rs`
- Modify: `crates/fetchpath-cache/src/lib.rs` (add `mod entry;` and re-exports)
- Modify: `crates/fetchpath-core/src/lib.rs:110-147` (add `is_credential_free`)

**Interfaces:**
- Consumes: `ContentId` from Task 1.
- Produces:
  - `Provenance::{Public, Credentialed}` with `Provenance::label(&self) -> &'static str` and `Provenance::parse(&str) -> Option<Provenance>`
  - `CachedVerification::{PieceHashes, FinalHashOnly}` with `label` / `parse` of the same shape
  - `CacheEntry { id: ContentId, bytes: u64, inserted_at_secs: u64, last_used_at_secs: u64, verification: CachedVerification, provenance: Provenance }`
  - `CacheEntry::is_shareable(&self) -> bool`
  - `fetchpath_core::RequestContext::is_credential_free(&self) -> bool`

- [ ] **Step 1: Write the failing test**

In `crates/fetchpath-cache/src/entry.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn entry(provenance: Provenance) -> CacheEntry {
        CacheEntry {
            id: ContentId::FlatSha256([1_u8; 32]),
            bytes: 10,
            inserted_at_secs: 100,
            last_used_at_secs: 100,
            verification: CachedVerification::FinalHashOnly,
            provenance,
        }
    }

    #[test]
    fn only_credential_free_entries_are_ever_shareable() {
        assert!(entry(Provenance::Public).is_shareable());
        assert!(!entry(Provenance::Credentialed).is_shareable());
    }

    #[test]
    fn provenance_labels_round_trip_and_reject_anything_else() {
        for value in [Provenance::Public, Provenance::Credentialed] {
            assert_eq!(Provenance::parse(value.label()), Some(value));
        }
        assert_eq!(Provenance::parse("shareable"), None);
        assert_eq!(Provenance::parse(""), None);
    }

    #[test]
    fn verification_labels_round_trip_and_reject_unverified() {
        for value in [
            CachedVerification::PieceHashes,
            CachedVerification::FinalHashOnly,
        ] {
            assert_eq!(CachedVerification::parse(value.label()), Some(value));
        }
        // Unverified content is not eligible for the cache, so it has no label.
        assert_eq!(CachedVerification::parse("unverified"), None);
    }
}
```

In `crates/fetchpath-core/src/lib.rs`, add to the existing `RequestContext` test coverage (append a test to the `mod tests` block at line 501):

```rust
#[test]
fn a_default_request_context_is_credential_free_and_a_populated_one_is_not() {
    assert!(RequestContext::default().is_credential_free());

    let with_cookie = RequestContext::new(vec!["a=b".to_owned()], None).expect("valid");
    assert!(!with_cookie.is_credential_free());

    let with_referer =
        RequestContext::new(Vec::new(), Some("https://example.test/".to_owned())).expect("valid");
    assert!(!with_referer.is_credential_free());
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p fetchpath-cache --locked` and `cargo test -p fetchpath-core --locked a_default_request_context`
Expected: FAIL to compile — `Provenance`, `CachedVerification`, `CacheEntry`, `is_credential_free` are all undefined.

- [ ] **Step 3: Write minimal implementation**

Above the test module in `crates/fetchpath-cache/src/entry.rs`:

```rust
use crate::ContentId;

/// Where an entry's bytes came from, decided once at insertion.
///
/// This is the whole basis on which content may leave the machine, so it is
/// never recomputed and never mutated. An entry that touched credentials stays
/// `Credentialed` for as long as it exists.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Provenance {
    /// Fetched with no cookies, no referer, and a delivering URL carrying no
    /// query string.
    Public,
    /// Anything else.
    Credentialed,
}

impl Provenance {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Public => "public",
            Self::Credentialed => "credentialed",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "public" => Some(Self::Public),
            "credentialed" => Some(Self::Credentialed),
            _ => None,
        }
    }
}

/// How the bytes were proven when they were inserted.
///
/// There is deliberately no `Unverified` variant: content without a trusted
/// digest is not eligible for this cache, so it cannot be represented here.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CachedVerification {
    PieceHashes,
    FinalHashOnly,
}

impl CachedVerification {
    pub fn label(&self) -> &'static str {
        match self {
            Self::PieceHashes => "piece_hashes",
            Self::FinalHashOnly => "final_hash_only",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "piece_hashes" => Some(Self::PieceHashes),
            "final_hash_only" => Some(Self::FinalHashOnly),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CacheEntry {
    pub id: ContentId,
    pub bytes: u64,
    pub inserted_at_secs: u64,
    pub last_used_at_secs: u64,
    pub verification: CachedVerification,
    pub provenance: Provenance,
}

impl CacheEntry {
    /// True only for credential-free entries. A trusted digest alone is not
    /// permission to redistribute an object.
    pub fn is_shareable(&self) -> bool {
        matches!(self.provenance, Provenance::Public)
    }
}
```

In `crates/fetchpath-cache/src/lib.rs`:

```rust
mod entry;
mod id;

pub use entry::{CacheEntry, CachedVerification, Provenance};
pub use id::ContentId;
```

In `crates/fetchpath-core/src/lib.rs`, inside `impl RequestContext` (next to the private `fingerprint`):

```rust
/// True when no credential-bearing context is attached. Used to decide
/// cache provenance; it must stay consistent with `fingerprint`.
pub fn is_credential_free(&self) -> bool {
    self.cookie_lines.is_empty() && self.referer.is_none()
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p fetchpath-cache -p fetchpath-core --locked`
Expected: PASS. `fetchpath-cache` now has 8 tests; `fetchpath-core` gains 1.

- [ ] **Step 5: Commit**

```bash
git add crates/fetchpath-cache crates/fetchpath-core/src/lib.rs
git commit -m "feat(cache): add entry model with immutable provenance"
```

---

### Task 3: Self-repairing index

The index is the quota's source of truth. It follows the generation-numbered temp-and-rename discipline already used by `CheckpointStore`, and rebuilds itself from the store directory when it cannot be read, matching the self-repairing settings file from FP-028.

**Files:**
- Create: `crates/fetchpath-cache/src/index.rs`
- Modify: `crates/fetchpath-cache/src/lib.rs` (add `mod index;`)

**Interfaces:**
- Consumes: `ContentId`, `CacheEntry`, `CachedVerification`, `Provenance`.
- Produces:
  - `CacheIndex::default() -> CacheIndex`
  - `CacheIndex::encode(&self) -> String`
  - `CacheIndex::decode(&str) -> Option<CacheIndex>`
  - `CacheIndex::insert(&mut self, CacheEntry)`
  - `CacheIndex::remove(&mut self, &ContentId) -> Option<CacheEntry>`
  - `CacheIndex::get(&self, &ContentId) -> Option<&CacheEntry>`
  - `CacheIndex::touch(&mut self, &ContentId, now_secs: u64)`
  - `CacheIndex::total_bytes(&self) -> u64`
  - `CacheIndex::entries(&self) -> Vec<CacheEntry>` (sorted by `ContentId` for determinism)
  - `CacheIndex::least_recently_used(&self, skip: &BTreeSet<ContentId>) -> Option<ContentId>`

- [ ] **Step 1: Write the failing test**

In `crates/fetchpath-cache/src/index.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn entry(tag: u8, bytes: u64, last_used: u64) -> CacheEntry {
        CacheEntry {
            id: ContentId::FlatSha256([tag; 32]),
            bytes,
            inserted_at_secs: 1,
            last_used_at_secs: last_used,
            verification: CachedVerification::PieceHashes,
            provenance: Provenance::Public,
        }
    }

    #[test]
    fn an_index_round_trips_through_its_encoding() {
        let mut index = CacheIndex::default();
        index.insert(entry(1, 10, 100));
        index.insert(entry(2, 20, 200));

        let decoded = CacheIndex::decode(&index.encode()).expect("decodes");
        assert_eq!(decoded.entries(), index.entries());
        assert_eq!(decoded.total_bytes(), 30);
    }

    #[test]
    fn a_malformed_index_is_refused_so_the_caller_can_rebuild_it() {
        for text in [
            "",
            "garbage",
            "fetchpath-cache-index 99\n",
            "fetchpath-cache-index 1\nsha256:zz 10 1 1 piece_hashes public\n",
            "fetchpath-cache-index 1\nsha256:aa 10 1 1 unverified public\n",
        ] {
            assert!(CacheIndex::decode(text).is_none(), "accepted {text:?}");
        }
    }

    #[test]
    fn a_truncated_final_line_does_not_silently_lose_bytes() {
        let mut index = CacheIndex::default();
        index.insert(entry(1, 10, 100));
        index.insert(entry(2, 20, 200));
        let encoded = index.encode();
        let truncated = &encoded[..encoded.len() - 5];

        assert!(
            CacheIndex::decode(truncated).is_none(),
            "a truncated index must be refused, not partially trusted"
        );
    }

    #[test]
    fn touching_an_entry_updates_only_its_last_used_time() {
        let mut index = CacheIndex::default();
        index.insert(entry(1, 10, 100));
        index.touch(&ContentId::FlatSha256([1; 32]), 500);

        let stored = index.get(&ContentId::FlatSha256([1; 32])).expect("present");
        assert_eq!(stored.last_used_at_secs, 500);
        assert_eq!(stored.inserted_at_secs, 1);
        assert_eq!(stored.bytes, 10);
    }

    #[test]
    fn least_recently_used_skips_the_entries_it_is_told_to_skip() {
        let mut index = CacheIndex::default();
        index.insert(entry(1, 10, 100));
        index.insert(entry(2, 20, 200));
        index.insert(entry(3, 30, 300));

        let none_skipped = BTreeSet::new();
        assert_eq!(
            index.least_recently_used(&none_skipped),
            Some(ContentId::FlatSha256([1; 32]))
        );

        let mut skip = BTreeSet::new();
        skip.insert(ContentId::FlatSha256([1; 32]));
        assert_eq!(
            index.least_recently_used(&skip),
            Some(ContentId::FlatSha256([2; 32]))
        );

        skip.insert(ContentId::FlatSha256([2; 32]));
        skip.insert(ContentId::FlatSha256([3; 32]));
        assert_eq!(index.least_recently_used(&skip), None);
    }

    #[test]
    fn removing_an_entry_returns_it_and_reduces_the_accounted_total() {
        let mut index = CacheIndex::default();
        index.insert(entry(1, 10, 100));
        index.insert(entry(2, 20, 200));

        let removed = index.remove(&ContentId::FlatSha256([1; 32])).expect("present");
        assert_eq!(removed.bytes, 10);
        assert_eq!(index.total_bytes(), 20);
        assert_eq!(index.remove(&ContentId::FlatSha256([1; 32])), None);
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p fetchpath-cache --locked`
Expected: FAIL to compile — `CacheIndex` is undefined.

- [ ] **Step 3: Write minimal implementation**

Above the test module in `crates/fetchpath-cache/src/index.rs`:

```rust
use crate::{CacheEntry, CachedVerification, ContentId, Provenance};
use std::collections::{BTreeMap, BTreeSet};

const INDEX_HEADER: &str = "fetchpath-cache-index";
const INDEX_VERSION: u32 = 1;

/// The accounted contents of the store.
///
/// Decoding is all-or-nothing on purpose. A partially readable index would
/// under-report the accounted total, which would quietly let the store grow
/// past its quota, so a malformed index is refused and the caller rebuilds it
/// from the directory instead.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CacheIndex {
    entries: BTreeMap<ContentId, CacheEntry>,
}

impl CacheIndex {
    pub fn insert(&mut self, entry: CacheEntry) {
        self.entries.insert(entry.id, entry);
    }

    pub fn remove(&mut self, id: &ContentId) -> Option<CacheEntry> {
        self.entries.remove(id)
    }

    pub fn get(&self, id: &ContentId) -> Option<&CacheEntry> {
        self.entries.get(id)
    }

    pub fn touch(&mut self, id: &ContentId, now_secs: u64) {
        if let Some(entry) = self.entries.get_mut(id) {
            entry.last_used_at_secs = now_secs;
        }
    }

    pub fn total_bytes(&self) -> u64 {
        self.entries.values().map(|entry| entry.bytes).sum()
    }

    pub fn entries(&self) -> Vec<CacheEntry> {
        self.entries.values().cloned().collect()
    }

    /// The eviction candidate: oldest last-used time, with the content id
    /// breaking ties so the choice is deterministic across runs.
    pub fn least_recently_used(&self, skip: &BTreeSet<ContentId>) -> Option<ContentId> {
        self.entries
            .values()
            .filter(|entry| !skip.contains(&entry.id))
            .min_by_key(|entry| (entry.last_used_at_secs, entry.id))
            .map(|entry| entry.id)
    }

    pub fn encode(&self) -> String {
        let mut out = format!("{INDEX_HEADER} {INDEX_VERSION}\n");
        for entry in self.entries.values() {
            out.push_str(&format!(
                "{} {} {} {} {} {}\n",
                entry.id.render(),
                entry.bytes,
                entry.inserted_at_secs,
                entry.last_used_at_secs,
                entry.verification.label(),
                entry.provenance.label(),
            ));
        }
        out
    }

    pub fn decode(text: &str) -> Option<Self> {
        let mut lines = text.lines();
        let header = lines.next()?;
        let (name, version) = header.split_once(' ')?;
        if name != INDEX_HEADER || version.parse::<u32>().ok()? != INDEX_VERSION {
            return None;
        }

        // A trailing newline is required on every record, so a truncated final
        // line is detectable rather than silently dropped.
        if !text.ends_with('\n') {
            return None;
        }

        let mut index = Self::default();
        for line in lines {
            if line.is_empty() {
                continue;
            }
            let mut fields = line.split(' ');
            let id = ContentId::parse(fields.next()?)?;
            let bytes = fields.next()?.parse::<u64>().ok()?;
            let inserted_at_secs = fields.next()?.parse::<u64>().ok()?;
            let last_used_at_secs = fields.next()?.parse::<u64>().ok()?;
            let verification = CachedVerification::parse(fields.next()?)?;
            let provenance = Provenance::parse(fields.next()?)?;
            if fields.next().is_some() {
                return None;
            }
            index.insert(CacheEntry {
                id,
                bytes,
                inserted_at_secs,
                last_used_at_secs,
                verification,
                provenance,
            });
        }
        Some(index)
    }
}
```

Add `mod index;` to `crates/fetchpath-cache/src/lib.rs` and re-export `pub use index::CacheIndex;`.

Note the `ContentId` derives `Ord` and `Hash` in Task 1 specifically so it can key a `BTreeMap` and a `BTreeSet` here.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p fetchpath-cache --locked`
Expected: PASS, 14 tests.

Run: `cargo clippy -p fetchpath-cache --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 5: Commit**

```bash
git add crates/fetchpath-cache
git commit -m "feat(cache): add all-or-nothing accounted index"
```

---

### Task 4: The store — open, insert, look up

Puts real bytes on disk under the identity from Task 1, accounted by the index from Task 3, and proves the index rebuilds itself when it is unreadable.

**Files:**
- Modify: `crates/fetchpath-cache/src/lib.rs`
- Create: `crates/fetchpath-cache/tests/cache_behaviour.rs`

**Interfaces:**
- Consumes: everything from Tasks 1–3.
- Produces:
  - `CacheConfig { quota_bytes: u64, max_entry_bytes: u64 }` with `CacheConfig::new(quota_bytes, max_entry_bytes)`
  - `ContentCache::open(root: &Path, config: CacheConfig) -> io::Result<ContentCache>`
  - `ContentCache::insert(&mut self, id: &ContentId, source: &Path, verification: CachedVerification, provenance: Provenance) -> io::Result<InsertOutcome>`
  - `ContentCache::lookup(&self, id: &ContentId) -> Option<CacheEntry>`
  - `ContentCache::total_bytes(&self) -> u64`
  - `ContentCache::entries(&self) -> Vec<CacheEntry>`
  - `InsertOutcome::{Inserted, AlreadyPresent, RefusedOversize, RefusedQuota}`

- [ ] **Step 1: Write the failing test**

In `crates/fetchpath-cache/tests/cache_behaviour.rs`:

```rust
use fetchpath_cache::{
    CacheConfig, CachedVerification, ContentCache, ContentId, InsertOutcome, Provenance,
};
use std::fs;
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

    assert_eq!(fs::read(&source).expect("source still readable"), b"hello cache");
}

#[test]
fn inserting_the_same_identity_twice_does_not_double_count() {
    let dir = TempDir::new("idempotent");
    let source = source_file(dir.path(), "payload.bin", b"hello cache");
    let mut cache =
        ContentCache::open(&dir.path().join("store"), CacheConfig::new(1024, 1024)).expect("open");

    cache
        .insert(&id(1), &source, CachedVerification::FinalHashOnly, Provenance::Public)
        .expect("first insert");
    let second = cache
        .insert(&id(1), &source, CachedVerification::FinalHashOnly, Provenance::Public)
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
            .insert(&id(1), &source, CachedVerification::FinalHashOnly, Provenance::Public)
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
```

That last assertion is the important one: a rebuild recovers *size*, but it cannot recover *provenance*, and guessing `Public` would leak. It must fail closed.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p fetchpath-cache --test cache_behaviour --locked`
Expected: FAIL to compile — `ContentCache`, `CacheConfig`, `InsertOutcome` are undefined.

- [ ] **Step 3: Write minimal implementation**

Replace `crates/fetchpath-cache/src/lib.rs` with:

```rust
//! A bounded, content-addressed store for content carrying a trusted digest.
//!
//! The cache keys only on identity supplied by trusted metadata. An observed
//! local digest records what was retained; it is never an identity this crate
//! keys on. Nothing here decides trust: callers supply the check that is run
//! before cached bytes are reused.

mod entry;
mod id;
mod index;

pub use entry::{CacheEntry, CachedVerification, Provenance};
pub use id::ContentId;
pub use index::CacheIndex;

use std::collections::BTreeSet;
use std::fs::{self, File};
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

pub struct ContentCache {
    root: PathBuf,
    config: CacheConfig,
    index: CacheIndex,
    pinned: BTreeSet<ContentId>,
}

impl ContentCache {
    pub fn open(root: &Path, config: CacheConfig) -> io::Result<Self> {
        fs::create_dir_all(root)?;
        let index = match fs::read_to_string(root.join(INDEX_FILE)) {
            Ok(text) => CacheIndex::decode(&text),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Some(CacheIndex::default()),
            Err(error) => return Err(error),
        };
        let mut cache = Self {
            root: root.to_path_buf(),
            config,
            index: index.unwrap_or_default(),
            pinned: BTreeSet::new(),
        };
        if index.is_none() {
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
        File::open(&temporary)?.sync_all()?;
        // The store is content-addressed, so an existing file at this path is
        // the same content. Replacing it is safe and keeps the store coherent.
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

    pub fn evict(&mut self, id: &ContentId) -> io::Result<bool> {
        if self.pinned.contains(id) {
            return Ok(false);
        }
        let Some(_) = self.index.remove(id) else {
            return Ok(false);
        };
        match fs::remove_file(self.path_for(id)) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        self.persist_index()?;
        Ok(true)
    }

    fn persist_index(&self) -> io::Result<()> {
        let temporary = self.root.join(TEMP_FILE);
        fs::write(&temporary, self.index.encode())?;
        File::open(&temporary)?.sync_all()?;
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
            for file in fs::read_dir(algorithm.path())?.flatten() {
                if !file.file_type()?.is_file() {
                    continue;
                }
                let Some(name) = file.file_name().to_str().map(str::to_owned) else {
                    continue;
                };
                let Some(label) = algorithm.file_name().to_str().map(str::to_owned) else {
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

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_secs())
        .unwrap_or_default()
}
```

Note `fs::rename` replaces an existing file on Windows, which is correct here because the path is content-addressed. This is *not* the destination-publication fence, which stays create-only via `hard_link` in `fetchpath-storage`.

Add `InsertOutcome`, `CacheConfig`, `ContentCache` to the crate's public exports (they are defined directly in `lib.rs`, so they are already public).

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p fetchpath-cache --locked`
Expected: PASS, 19 tests.

- [ ] **Step 5: Commit**

```bash
git add crates/fetchpath-cache
git commit -m "feat(cache): add content-addressed store with self-repairing index"
```

---

### Task 5: Quota, eviction and pinning

The quota acceptance item. Proves that a breach evicts the least recently used entry and only that entry, that oversize entries are refused outright, and that a pinned entry is never evicted out from under a reuse in flight.

**Files:**
- Modify: `crates/fetchpath-cache/src/lib.rs` (add pin/unpin)
- Modify: `crates/fetchpath-cache/tests/cache_behaviour.rs`

**Interfaces:**
- Consumes: Task 4's `ContentCache`.
- Produces:
  - `ContentCache::pin(&mut self, &ContentId) -> Option<PathBuf>`
  - `ContentCache::unpin(&mut self, &ContentId)`
  - `ContentCache::is_pinned(&self, &ContentId) -> bool`

- [ ] **Step 1: Write the failing test**

Append to `crates/fetchpath-cache/tests/cache_behaviour.rs`:

```rust
#[test]
fn an_entry_larger_than_the_ceiling_is_refused_without_emptying_the_store() {
    let dir = TempDir::new("oversize");
    let small = source_file(dir.path(), "small.bin", b"12345");
    let big = source_file(dir.path(), "big.bin", &vec![7_u8; 500]);
    let mut cache =
        ContentCache::open(&dir.path().join("store"), CacheConfig::new(1000, 100)).expect("open");

    cache
        .insert(&id(1), &small, CachedVerification::FinalHashOnly, Provenance::Public)
        .expect("insert small");

    let outcome = cache
        .insert(&id(2), &big, CachedVerification::FinalHashOnly, Provenance::Public)
        .expect("insert big");

    assert_eq!(outcome, InsertOutcome::RefusedOversize);
    assert!(cache.lookup(&id(1)).is_some(), "existing entry survived");
    assert!(cache.lookup(&id(2)).is_none());
    assert_eq!(cache.total_bytes(), 5);
}

#[test]
fn an_entry_larger_than_the_whole_quota_is_refused_as_oversize() {
    let dir = TempDir::new("over-quota");
    let big = source_file(dir.path(), "big.bin", &vec![7_u8; 500]);
    let mut cache =
        ContentCache::open(&dir.path().join("store"), CacheConfig::new(100, 1000)).expect("open");

    let outcome = cache
        .insert(&id(1), &big, CachedVerification::FinalHashOnly, Provenance::Public)
        .expect("insert");

    assert_eq!(outcome, InsertOutcome::RefusedOversize);
    assert_eq!(cache.total_bytes(), 0);
}

#[test]
fn a_quota_breach_evicts_the_least_recently_used_entry_and_only_that_one() {
    let dir = TempDir::new("evict-lru");
    let forty = source_file(dir.path(), "forty.bin", &vec![1_u8; 40]);
    let mut cache =
        ContentCache::open(&dir.path().join("store"), CacheConfig::new(100, 100)).expect("open");

    // Distinct last-used times so "least recently used" is unambiguous.
    for (tag, _) in [(1_u8, ()), (2, ()), (3, ())].iter().take(2) {
        cache
            .insert(&id(*tag), &forty, CachedVerification::FinalHashOnly, Provenance::Public)
            .expect("insert");
        std::thread::sleep(std::time::Duration::from_millis(1100));
    }
    assert_eq!(cache.total_bytes(), 80);

    // Touch entry 1 so entry 2 becomes the least recently used.
    cache.pin(&id(1)).expect("pin");
    cache.unpin(&id(1));

    cache
        .insert(&id(3), &forty, CachedVerification::FinalHashOnly, Provenance::Public)
        .expect("insert third");

    assert_eq!(cache.total_bytes(), 80, "quota held");
    assert!(cache.lookup(&id(3)).is_some(), "new entry stored");
    assert!(cache.lookup(&id(2)).is_none(), "least recently used evicted");
    assert!(cache.lookup(&id(1)).is_some(), "only one entry evicted");
}

#[test]
fn a_pinned_entry_is_never_evicted_to_make_room() {
    let dir = TempDir::new("pin-protects");
    let sixty = source_file(dir.path(), "sixty.bin", &vec![1_u8; 60]);
    let mut cache =
        ContentCache::open(&dir.path().join("store"), CacheConfig::new(100, 100)).expect("open");

    cache
        .insert(&id(1), &sixty, CachedVerification::FinalHashOnly, Provenance::Public)
        .expect("insert");
    let pinned_path = cache.pin(&id(1)).expect("pin returns the stored path");
    assert!(pinned_path.exists());

    let outcome = cache
        .insert(&id(2), &sixty, CachedVerification::FinalHashOnly, Provenance::Public)
        .expect("insert second");

    assert_eq!(outcome, InsertOutcome::RefusedQuota);
    assert!(cache.lookup(&id(1)).is_some(), "pinned entry survived");
    assert!(pinned_path.exists(), "pinned bytes still on disk");

    cache.unpin(&id(1));
    let after = cache
        .insert(&id(2), &sixty, CachedVerification::FinalHashOnly, Provenance::Public)
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
```

The `sleep` in the eviction test exists because `last_used_at_secs` has one-second resolution; without distinct times the least-recently-used choice would be decided by the content-id tiebreak instead of by recency, and the test would not prove what it claims.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p fetchpath-cache --test cache_behaviour --locked`
Expected: FAIL to compile — `pin`, `unpin`, `is_pinned` are undefined.

- [ ] **Step 3: Write minimal implementation**

Add to `impl ContentCache` in `crates/fetchpath-cache/src/lib.rs`:

```rust
/// Marks an entry as in use and returns its stored path.
///
/// A pinned entry is never chosen for eviction, so a reuse in flight cannot
/// have its bytes deleted underneath it. Pinning also counts as a use, which
/// is what moves the entry away from the eviction front.
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
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p fetchpath-cache --locked`
Expected: PASS, 24 tests. The eviction test takes about 2.2 seconds because of the sleeps.

Run: `cargo clippy -p fetchpath-cache --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 5: Commit**

```bash
git add crates/fetchpath-cache
git commit -m "feat(cache): enforce quota with least-recently-used eviction and pinning"
```

---

### Task 6: Verified reuse

Nothing leaves the cache unchecked. The cache holds no trust policy of its own: the caller supplies a `TrustedCheck`, and a cached file that fails it is evicted and reported as a miss rather than published or raised as an error.

**Files:**
- Modify: `crates/fetchpath-cache/src/lib.rs`
- Modify: `crates/fetchpath-cache/tests/cache_behaviour.rs`

**Interfaces:**
- Consumes: Tasks 4–5.
- Produces:
  - `trait TrustedCheck { fn verify(&self, path: &Path) -> io::Result<bool>; }`
  - `ContentCache::acquire_verified(&mut self, id: &ContentId, check: &dyn TrustedCheck) -> io::Result<Acquired>`
  - `ContentCache::release(&mut self, id: &ContentId)`
  - `Acquired::{Hit(PathBuf), Miss, FailedCheck}`

- [ ] **Step 1: Write the failing test**

Append to `crates/fetchpath-cache/tests/cache_behaviour.rs`:

```rust
use fetchpath_cache::{Acquired, TrustedCheck};
use std::io;

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
fn a_verified_hit_returns_pinned_bytes() {
    let dir = TempDir::new("acquire-hit");
    let source = source_file(dir.path(), "payload.bin", b"hello cache");
    let mut cache =
        ContentCache::open(&dir.path().join("store"), CacheConfig::new(1024, 1024)).expect("open");
    cache
        .insert(&id(1), &source, CachedVerification::FinalHashOnly, Provenance::Public)
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
        cache.acquire_verified(&id(1), &AlwaysValid).expect("acquire"),
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
        .insert(&id(1), &source, CachedVerification::FinalHashOnly, Provenance::Public)
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
    assert!(!cache.is_pinned(&id(1)), "a failed check leaves nothing pinned");
}

#[test]
fn an_entry_whose_file_vanished_is_a_miss_rather_than_an_error() {
    let dir = TempDir::new("vanished");
    let store = dir.path().join("store");
    let source = source_file(dir.path(), "payload.bin", b"hello cache");
    let mut cache = ContentCache::open(&store, CacheConfig::new(1024, 1024)).expect("open");
    cache
        .insert(&id(1), &source, CachedVerification::FinalHashOnly, Provenance::Public)
        .expect("insert");

    fs::remove_file(cache.path_for(&id(1))).expect("remove stored bytes");

    assert_eq!(
        cache.acquire_verified(&id(1), &AlwaysValid).expect("acquire"),
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
        .insert(&id(1), &source, CachedVerification::PieceHashes, Provenance::Public)
        .expect("insert");

    assert_eq!(
        cache.acquire_verified(&id(1), &AlwaysInvalid).expect("acquire"),
        Acquired::FailedCheck
    );
    assert_eq!(
        cache.acquire_verified(&id(1), &AlwaysValid).expect("acquire"),
        Acquired::Miss,
        "a second attempt cannot resurrect the evicted entry"
    );
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p fetchpath-cache --test cache_behaviour --locked`
Expected: FAIL to compile — `Acquired`, `TrustedCheck`, `acquire_verified`, `release` are undefined.

- [ ] **Step 3: Write minimal implementation**

Add to `crates/fetchpath-cache/src/lib.rs`:

```rust
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

impl ContentCache {
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

    /// Removes an entry from accounting regardless of pinning. Used only when
    /// the entry is already known to be unusable.
    fn drop_entry(&mut self, id: &ContentId) -> io::Result<()> {
        self.pinned.remove(id);
        self.index.remove(id);
        self.persist_index()
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p fetchpath-cache --locked`
Expected: PASS, 29 tests.

Run: `cargo clippy -p fetchpath-cache --all-targets -- -D warnings` and `cargo fmt --check`
Expected: clean.

- [ ] **Step 5: Commit**

```bash
git add crates/fetchpath-cache
git commit -m "feat(cache): re-verify cached bytes before reuse and evict failures"
```

---

### Task 7: Delivery source on the verified download

Makes "where did these bytes come from" a first-class part of the result, before any cache is wired in. This is a small, self-contained refactor that later tasks depend on, and it is what stops a cache hit from ever being reported as a transfer rate.

**Files:**
- Modify: `crates/fetchpath-core/src/verified.rs:136-150` (the `VerifiedDownload` struct)
- Modify: `crates/fetchpath-core/src/verified.rs:1026-1042` (the `publish` helper)
- Modify: `crates/fetchpath-core/src/verified.rs:421-441` (the success return)

**Interfaces:**
- Consumes: nothing new.
- Produces:
  - `DeliverySource::{Network, LocalCache}` (the `Peer` variant arrives with the LAN plan)
  - `VerifiedDownload::source: DeliverySource`
  - `publish(request, store, url: &str, key, total, observed, faults)` — takes a `&str` url instead of a `&Mirror`

- [ ] **Step 1: Write the failing test**

Append to the `mod tests` block at the end of `crates/fetchpath-core/src/verified.rs`:

```rust
#[test]
fn a_mirror_delivered_download_reports_a_network_source() {
    let dir = temp_dir("delivery-source");
    let body = payload();
    let server = mirror(body.clone(), body.len(), None);
    let destination = dir.join("out.bin");

    let mut request = request(vec![MirrorSource::new(server.url())], destination);
    request.expected_sha256 = Some(hex(&sha2::Sha256::digest(&body)));

    let result = download_verified(request).expect("download succeeds");

    assert_eq!(result.source, DeliverySource::Network);
    assert_eq!(result.verification, VerificationLevel::FinalHashOnly);
}
```

Read the existing helpers at `crates/fetchpath-core/src/verified.rs:1106-1260` before writing this — `temp_dir`, `payload`, `hex`, `mirror` and `request` already exist with those exact names, and `MirrorServer::url()` is how the other tests reach the fixture. Match their real signatures rather than the sketch above.

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p fetchpath-core --locked a_mirror_delivered_download_reports_a_network_source`
Expected: FAIL to compile — no `DeliverySource`, and `VerifiedDownload` has no `source` field.

- [ ] **Step 3: Write minimal implementation**

Add near `VerificationLevel` in `crates/fetchpath-core/src/verified.rs`:

```rust
/// Where the published bytes actually came from.
///
/// This is not a performance label. Completion from a local source is reuse,
/// not throughput, and a benchmark must exclude or flag any completion whose
/// source is not `Network`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeliverySource {
    Network,
    LocalCache,
}
```

Add the field to `VerifiedDownload`:

```rust
pub struct VerifiedDownload {
    pub destination: PathBuf,
    pub bytes: u64,
    /// An observed local digest, not publisher-authenticity evidence.
    pub observed_sha256: String,
    pub verification: VerificationLevel,
    /// Where the bytes came from. Reuse is not throughput.
    pub source: DeliverySource,
    pub repaired_pieces: Vec<usize>,
    pub conservative_restarts: u32,
    pub mirrors: Vec<MirrorReport>,
    pub staging_cleanup_pending: Option<PathBuf>,
}
```

Set `source: DeliverySource::Network` in the existing success return at line 434.

Change `publish` to take the url directly, so the cache path can reuse it without inventing a `Mirror`:

```rust
fn publish(
    request: &VerifiedDownloadRequest,
    store: &CheckpointStore,
    url: &str,
    key: String,
    total: u64,
    observed: String,
    faults: &dyn FaultInjector,
) -> Result<crate::DownloadedFile, VerifiedDownloadError> {
    let mut intent = CheckpointRecord::downloading(key, total, observed, None, Some(total));
    intent.phase = CheckpointPhase::PublicationIntent;
    let intent = store
        .commit(intent, faults)
        .map_err(|error| storage(store, error))?;
    let download_request = as_download_request(request, url);
    transfer::publish(&download_request, store, &intent, faults).map_err(from_download_error)
}
```

Update the one existing call site to pass `&mirrors[selected].url` instead of `&mirrors[selected]`.

Re-export from `crates/fetchpath-core/src/lib.rs` wherever `VerificationLevel` is already re-exported: add `DeliverySource`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p fetchpath-core --locked`
Expected: PASS. Every existing `verified.rs` test still passes; one new test.

- [ ] **Step 5: Commit**

```bash
git add crates/fetchpath-core
git commit -m "feat(core): report where verified bytes were delivered from"
```

---

### Task 8: Offline reuse through the verified path

The acceptance item. With the cache populated and every mirror unreachable, a download completes from cache, and the published file is byte-identical and verified.

**Files:**
- Modify: `crates/fetchpath-core/Cargo.toml` (add the cache dependency)
- Modify: `crates/fetchpath-core/src/verified.rs`
- Modify: `crates/fetchpath-core/src/lib.rs` (re-exports)

**Interfaces:**
- Consumes: `fetchpath_cache::{Acquired, ContentCache, ContentId, CachedVerification, Provenance, TrustedCheck}`; `publish(.., url: &str, ..)` from Task 7.
- Produces:
  - `VerifiedDownloadRequest::cache: Option<&mut ContentCache>` — modelled as a separate parameter, not a struct field, because `VerifiedDownloadRequest` is `Clone` and `ContentCache` is not.
  - `download_verified_cached(request, cache: &mut ContentCache) -> Result<VerifiedDownload, VerifiedDownloadError>`
  - `VerifiedDownloadRequest::content_id(&self) -> Option<ContentId>`

- [ ] **Step 1: Write the failing test**

Append to the `mod tests` block in `crates/fetchpath-core/src/verified.rs`:

```rust
#[test]
fn a_populated_cache_completes_the_download_with_every_mirror_unreachable() {
    let dir = temp_dir("offline-reuse");
    let body = payload();
    let digest = hex(&sha2::Sha256::digest(&body));

    // Seed the cache from a file that is not the destination.
    let seed = dir.join("seed.bin");
    std::fs::write(&seed, &body).expect("seed written");
    let mut cache = fetchpath_cache::ContentCache::open(
        &dir.join("cache"),
        fetchpath_cache::CacheConfig::new(1 << 20, 1 << 20),
    )
    .expect("cache opens");
    let id = fetchpath_cache::ContentId::from_expected_sha256(&digest).expect("valid digest");
    cache
        .insert(
            &id,
            &seed,
            fetchpath_cache::CachedVerification::FinalHashOnly,
            fetchpath_cache::Provenance::Public,
        )
        .expect("seeded");

    // Every mirror refuses connections.
    let destination = dir.join("out.bin");
    let mut request = request(vec![MirrorSource::new(offline_url())], destination.clone());
    request.expected_sha256 = Some(digest.clone());

    let result = download_verified_cached(request, &mut cache).expect("completes from cache");

    assert_eq!(result.source, DeliverySource::LocalCache);
    assert_eq!(result.verification, VerificationLevel::FinalHashOnly);
    assert_eq!(result.observed_sha256, digest);
    assert_eq!(std::fs::read(&destination).expect("published"), body);
    assert!(
        result.mirrors.iter().all(|report| report.attempts == 0),
        "no mirror was contacted"
    );
}

#[test]
fn a_verified_network_download_populates_the_cache_for_next_time() {
    let dir = temp_dir("cache-populate");
    let body = payload();
    let digest = hex(&sha2::Sha256::digest(&body));
    let server = mirror(body.clone(), body.len(), None);

    let mut cache = fetchpath_cache::ContentCache::open(
        &dir.join("cache"),
        fetchpath_cache::CacheConfig::new(1 << 20, 1 << 20),
    )
    .expect("cache opens");

    let mut request = request(vec![MirrorSource::new(server.url())], dir.join("out.bin"));
    request.expected_sha256 = Some(digest.clone());

    let result = download_verified_cached(request, &mut cache).expect("downloads");
    assert_eq!(result.source, DeliverySource::Network);

    let id = fetchpath_cache::ContentId::from_expected_sha256(&digest).expect("valid");
    let entry = cache.lookup(&id).expect("cached after a verified completion");
    assert_eq!(entry.bytes, body.len() as u64);
    assert_eq!(entry.provenance, fetchpath_cache::Provenance::Public);
}

#[test]
fn a_download_carrying_credentials_is_cached_as_credentialed() {
    let dir = temp_dir("cache-credentialed");
    let body = payload();
    let digest = hex(&sha2::Sha256::digest(&body));
    let server = mirror(body.clone(), body.len(), None);

    let mut cache = fetchpath_cache::ContentCache::open(
        &dir.join("cache"),
        fetchpath_cache::CacheConfig::new(1 << 20, 1 << 20),
    )
    .expect("cache opens");

    let mut request = request(vec![MirrorSource::new(server.url())], dir.join("out.bin"));
    request.expected_sha256 = Some(digest.clone());
    request.context =
        RequestContext::new(vec!["session=secret".to_owned()], None).expect("valid context");

    download_verified_cached(request, &mut cache).expect("downloads");

    let id = fetchpath_cache::ContentId::from_expected_sha256(&digest).expect("valid");
    let entry = cache.lookup(&id).expect("cached");
    assert_eq!(entry.provenance, fetchpath_cache::Provenance::Credentialed);
    assert!(!entry.is_shareable(), "credentialed bytes never become shareable");
}

#[test]
fn an_unverified_download_is_never_cached() {
    let dir = temp_dir("cache-unverified");
    let body = payload();
    let server = mirror(body.clone(), body.len(), None);

    let mut cache = fetchpath_cache::ContentCache::open(
        &dir.join("cache"),
        fetchpath_cache::CacheConfig::new(1 << 20, 1 << 20),
    )
    .expect("cache opens");

    // No expected digest and no piece map: nothing trusted to key on.
    let request = request(vec![MirrorSource::new(server.url())], dir.join("out.bin"));

    let result = download_verified_cached(request, &mut cache).expect("downloads");
    assert_eq!(result.verification, VerificationLevel::Unverified);
    assert_eq!(result.source, DeliverySource::Network);
    assert_eq!(cache.entries().len(), 0, "unverified bytes are not eligible");
}

#[test]
fn a_tampered_cache_entry_falls_through_to_the_network_rather_than_failing() {
    let dir = temp_dir("cache-tampered-fallthrough");
    let body = payload();
    let digest = hex(&sha2::Sha256::digest(&body));
    let server = mirror(body.clone(), body.len(), None);

    let seed = dir.join("seed.bin");
    std::fs::write(&seed, &body).expect("seed written");
    let mut cache = fetchpath_cache::ContentCache::open(
        &dir.join("cache"),
        fetchpath_cache::CacheConfig::new(1 << 20, 1 << 20),
    )
    .expect("cache opens");
    let id = fetchpath_cache::ContentId::from_expected_sha256(&digest).expect("valid");
    cache
        .insert(
            &id,
            &seed,
            fetchpath_cache::CachedVerification::FinalHashOnly,
            fetchpath_cache::Provenance::Public,
        )
        .expect("seeded");

    std::fs::write(cache.path_for(&id), b"not the promised bytes").expect("tamper");

    let destination = dir.join("out.bin");
    let mut request = request(vec![MirrorSource::new(server.url())], destination.clone());
    request.expected_sha256 = Some(digest.clone());

    let result = download_verified_cached(request, &mut cache).expect("falls through and succeeds");

    assert_eq!(result.source, DeliverySource::Network);
    assert_eq!(std::fs::read(&destination).expect("published"), body);
}
```

`offline_url()` already exists at `crates/fetchpath-core/src/verified.rs:1190` and returns a URL whose port refuses connections; reuse it rather than inventing one.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p fetchpath-core --locked`
Expected: FAIL to compile — `download_verified_cached` does not exist and `fetchpath_cache` is not a dependency.

- [ ] **Step 3: Write minimal implementation**

Add to `crates/fetchpath-core/Cargo.toml` under `[dependencies]`:

```toml
fetchpath-cache = { path = "../fetchpath-cache" }
```

Add to `crates/fetchpath-core/src/verified.rs`:

```rust
use fetchpath_cache::{
    Acquired, CachedVerification, ContentCache, ContentId, Provenance, TrustedCheck,
};

impl VerifiedDownloadRequest {
    /// The trusted identity of this request's content, if it has one.
    ///
    /// Piece hashes take precedence over a whole-file digest because they are
    /// the stronger construction: they localize damage, and a request carrying
    /// both is still fundamentally piece-identified.
    pub fn content_id(&self) -> Option<ContentId> {
        if let Some(pieces) = &self.pieces {
            return Some(ContentId::from_piece_map(pieces));
        }
        self.expected_sha256
            .as_deref()
            .and_then(ContentId::from_expected_sha256)
    }
}

/// The trusted check the cache runs before reuse: exactly the digests this
/// request carries, applied to the cached file.
struct RequestCheck<'a> {
    pieces: Option<&'a PieceMap>,
    expected_sha256: Option<&'a str>,
}

impl TrustedCheck for RequestCheck<'_> {
    fn verify(&self, path: &Path) -> io::Result<bool> {
        if let Some(pieces) = self.pieces
            && !pieces.verify_file(path)?.is_complete()
        {
            return Ok(false);
        }
        if let Some(expected) = self.expected_sha256
            && !sha256_file(path)?.eq_ignore_ascii_case(expected)
        {
            return Ok(false);
        }
        Ok(true)
    }
}

/// A verified download that may complete from the cache instead of the network.
///
/// A cache hit is reuse, not throughput: the result's `source` says so, and
/// callers that report rates must honour it.
pub fn download_verified_cached(
    request: VerifiedDownloadRequest,
    cache: &mut ContentCache,
) -> Result<VerifiedDownload, VerifiedDownloadError> {
    if let Some(id) = request.content_id()
        && let Some(result) = reuse_from_cache(&request, cache, &id)?
    {
        return Ok(result);
    }

    let result = download_verified(request.clone())?;
    insert_into_cache(&request, cache, &result);
    Ok(result)
}

fn reuse_from_cache(
    request: &VerifiedDownloadRequest,
    cache: &mut ContentCache,
    id: &ContentId,
) -> Result<Option<VerifiedDownload>, VerifiedDownloadError> {
    if request.destination.exists() {
        return Err(VerifiedDownloadError::DestinationExists {
            destination: request.destination.clone(),
            staging: None,
        });
    }
    let check = RequestCheck {
        pieces: request.pieces.as_ref(),
        expected_sha256: request.expected_sha256.as_deref(),
    };
    // A cache that cannot be read is not a download failure.
    let Ok(acquired) = cache.acquire_verified(id, &check) else {
        return Ok(None);
    };
    let Acquired::Hit(cached) = acquired else {
        return Ok(None);
    };

    let published = publish_from_cache(request, &cached);
    cache.release(id);
    published
}

fn publish_from_cache(
    request: &VerifiedDownloadRequest,
    cached: &Path,
) -> Result<Option<VerifiedDownload>, VerifiedDownloadError> {
    let key = source_key_with_context("cache", &request.context.fingerprint());
    let Ok(store) = CheckpointStore::new(&request.destination, &key) else {
        return Ok(None);
    };
    // Stage a copy on the destination volume, then publish through the same
    // create-only fence the network path uses.
    let Ok(_) = store.reset() else {
        return Ok(None);
    };
    if std::fs::copy(cached, store.staging()).is_err() {
        let _ = store.remove_all();
        return Ok(None);
    }
    let Ok(observed) = sha256_file(store.staging()) else {
        let _ = store.remove_all();
        return Ok(None);
    };
    let Ok(total) = std::fs::metadata(store.staging()).map(|meta| meta.len()) else {
        let _ = store.remove_all();
        return Ok(None);
    };

    let verification = match (&request.pieces, &request.expected_sha256) {
        (Some(_), _) => VerificationLevel::PieceHashes,
        (None, Some(_)) => VerificationLevel::FinalHashOnly,
        (None, None) => VerificationLevel::Unverified,
    };

    let published = publish(
        request,
        &store,
        "cache",
        key,
        total,
        observed.clone(),
        &NoFaults,
    )?;

    Ok(Some(VerifiedDownload {
        destination: published.destination,
        bytes: published.bytes,
        observed_sha256: published.observed_sha256,
        verification,
        source: DeliverySource::LocalCache,
        repaired_pieces: Vec::new(),
        conservative_restarts: 0,
        mirrors: reports(&build_mirrors(&request.mirrors)),
        staging_cleanup_pending: published.staging_cleanup_pending,
    }))
}

/// Inserts a completed download. Failure to cache is never a download failure.
fn insert_into_cache(
    request: &VerifiedDownloadRequest,
    cache: &mut ContentCache,
    result: &VerifiedDownload,
) {
    if result.source != DeliverySource::Network {
        return;
    }
    let verification = match result.verification {
        VerificationLevel::PieceHashes => CachedVerification::PieceHashes,
        VerificationLevel::FinalHashOnly => CachedVerification::FinalHashOnly,
        // Not eligible: nothing trusted identifies these bytes.
        VerificationLevel::Unverified => return,
    };
    let Some(id) = request.content_id() else {
        return;
    };
    let delivering = result
        .mirrors
        .iter()
        .find(|report| report.outcome == MirrorOutcome::Delivered);
    let provenance = if request.context.is_credential_free()
        && delivering.is_some_and(|report| !report.redacted_url.contains('?'))
    {
        Provenance::Public
    } else {
        Provenance::Credentialed
    };
    let _ = cache.insert(&id, &result.destination, verification, provenance);
}
```

`MirrorReport::redacted_url` already has its query string stripped (`redact` at `verified.rs:573`), so the `contains('?')` check is a belt-and-braces guard rather than the primary defence. Read `redact` and confirm before relying on it; if it strips the query unconditionally, base provenance on whether the *original* mirror url had one and add that as a field on `MirrorReport`.

Re-export `download_verified_cached` and `DeliverySource` from `crates/fetchpath-core/src/lib.rs`, plus `pub use fetchpath_cache;` so callers do not need a second dependency line.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p fetchpath-core --locked`
Expected: PASS, including all five new tests.

Run: `cargo test --workspace --locked`
Expected: PASS. Baseline was 120 tests; this plan adds roughly 35.

Run: `cargo clippy --workspace --all-targets -- -D warnings` and `cargo fmt --check`
Expected: clean.

- [ ] **Step 5: Commit**

```bash
git add crates/fetchpath-core Cargo.lock
git commit -m "feat(core): complete verified downloads from the bounded cache"
```

---

### Task 9: Evidence and backlog

Records what was actually run and what remains unproven. The project's rule is that evidence must be reachable and claims must be earned, so this task is not optional paperwork.

**Files:**
- Create: `docs/development/CACHE-AND-LAN.md`
- Create: `docs/development/evidence/cache/fp020-cache-matrix.json`
- Modify: `docs/tasks/backlog.json` (FP-020 evidence; status stays `in_progress` until the LAN plan lands)
- Modify: `README.md` ("What it does today")
- Modify: `PROJECT.md` (current delivery)

- [ ] **Step 1: Capture the real test output**

```bash
cargo test --workspace --locked 2>&1 | tee /tmp/fp020-tests.log
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -5
cargo fmt --check && echo "fmt clean"
node tools/tasks.mjs check
```

Record the actual counts. Do not write a number you did not read from the output.

- [ ] **Step 2: Write the evidence document**

`docs/development/CACHE-AND-LAN.md` must state, in the project's existing register:

- What was tested and the exact commands.
- That reuse is reuse and not acceleration, and that no speed claim is made.
- That a trusted digest establishes representation identity, not publisher authenticity.
- That a rebuilt index fails closed: recovered entries are recorded `Credentialed` and are never shareable, because provenance is not recoverable from bytes.
- Known limitations: no power-loss testing; single-process store with no cross-process locking; LAN mode not yet implemented; mDNS discovery and desktop settings deferred to follow-up tasks.

- [ ] **Step 3: Write the evidence matrix**

`docs/development/evidence/cache/fp020-cache-matrix.json` — one record per behavioural test with its name, what it proves, and its result. Follow the shape of `docs/development/evidence/metalink/fp019-mirror-matrix.json`.

- [ ] **Step 4: Update the backlog**

Add the two evidence paths to FP-020's `evidence` array and write the `verification` field from the real results. Leave `status` as `in_progress`: the LAN half of the acceptance is not done, and marking a whole task done because half of it works is exactly what the project's rules forbid.

Run: `node tools/tasks.mjs check`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add docs README.md PROJECT.md
git commit -m "docs: record bounded content cache evidence"
```

---

## Self-Review

**Spec coverage.** Spec §1 units → Tasks 1–8 (cache crate; `adapters/lan` is the follow-up plan). §2 content identity → Task 1. §3 store, index, provenance, eviction, reuse → Tasks 2–6. §4 engine integration → Tasks 7–8. §5 pairing → **follow-up plan, declared out of scope in this plan's header**. §6 error handling → Tasks 6 and 8 (corrupt entry evicted, quota refusal recorded, unreadable cache falls through). §7 verification → tests throughout plus Task 9. §8 claims not made → Global Constraints and Task 9.

**Placeholder scan.** No TBDs. Every code step carries real code. Three steps deliberately instruct the implementer to read existing source before matching a signature (Tasks 1, 7, 8) — these name the exact file and line and say what to do if it differs, which is verification, not a placeholder.

**Type consistency.** `ContentId` (Task 1) is used identically in 2–8. `CachedVerification` and `Provenance` (Task 2) match their use in 3, 4 and 8. `CacheIndex` methods declared in Task 3 match their call sites in 4–6. `InsertOutcome` (Task 4) matches Task 5's assertions. `Acquired` and `TrustedCheck` (Task 6) match Task 8's use. `publish` changes to `url: &str` in Task 7 and is called that way in Task 8. `ContentId` derives `Ord`/`Hash` in Task 1 because Task 3 keys a `BTreeMap`/`BTreeSet` on it.

**One risk flagged rather than hidden.** Task 8's provenance rule reads `MirrorReport::redacted_url`, whose query string is already stripped. The step says so and gives the fallback (carry a `had_query` flag on `MirrorReport`). The implementer must confirm this rather than assume.
