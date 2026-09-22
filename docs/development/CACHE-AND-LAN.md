# Bounded content cache and paired LAN mode

Task FP-020. Recorded 22 September 2026. Status: the cache half is implemented
and tested; the paired LAN half is **not implemented yet**.

Design: [content cache and paired LAN design](../superpowers/specs/2026-09-22-content-cache-and-paired-lan-design.md).
Plan: [bounded content cache plan](../superpowers/plans/2026-09-22-bounded-content-cache.md).

## What exists

`crates/fetchpath-cache` is a bounded, content-addressed store for content that
carries a trusted digest. `crates/fetchpath-core` consults it before contacting
any mirror and inserts into it after a verified network completion, through
`download_verified_cached`.

Only content with a trusted digest is eligible. A download that completed at
`VerificationLevel::Unverified` is never inserted, so every cache hit is
provable rather than assumed.

Identity encodes construction as well as algorithm. `ContentId::FlatSha256`
comes from a whole-file digest stated by trusted metadata; `ContentId::PieceMapSha256`
is a digest over the canonical encoding of a trusted piece map, which exists
because a Metalink 4 file may supply piece hashes and no whole-file digest. The
two are distinct on disk and in the index and are never interchangeable.

Reuse re-verifies before publishing. The cache holds no trust policy of its
own: the caller supplies a `TrustedCheck` built from exactly the digests the
request carries. A cached file that fails is evicted and reported as a miss.
Publication still goes through the unchanged create-only fence in
`fetchpath-storage`.

`VerifiedDownload` now carries a `DeliverySource` of `Network` or `LocalCache`.

## Commands actually run

```
cargo test --workspace --offline
cargo clippy --workspace --all-targets --offline -- -D warnings
cargo fmt --check
node tools/tasks.mjs check
```

Results on 22 September 2026, Windows 11 Pro 26200, x64:

- `cargo test --workspace` — 158 passed, 0 failed. The recorded baseline before
  this work was 120 passed, 0 failed.
- `fetchpath-cache` — 29 tests (14 unit, 15 behavioural).
- `fetchpath-core` — 45 tests, up from 36.
- `cargo clippy --workspace --all-targets -- -D warnings` — clean.
- `cargo fmt --check` — clean.
- `node tools/tasks.mjs check` — PASS.

Per-test outcomes: [cache matrix](evidence/cache/fp020-cache-matrix.json).

## Two behaviours worth stating plainly

**A rebuilt index fails closed.** If the index file cannot be read, accounting
is rebuilt by walking the store directory. Entry size is recoverable from the
files; provenance is not. Recording a rebuilt entry as `Public` would be a
leak, so rebuilt entries are recorded `Credentialed` and can never be shared.
Covered by `a_corrupted_index_is_rebuilt_from_the_store_directory`.

**Provenance is read from the original mirror URL.** `MirrorReport::redacted_url`
already has its query string stripped, so a check against the report cannot
tell a signed URL from a plain one. Provenance is therefore decided from the
request's own mirror list. This was verified adversarially: reverting to the
report-based check makes `a_download_from_a_signed_url_is_cached_as_credentialed`
fail, confirming the test detects the leak rather than merely passing.

## Claims this work does not make

**No speed claim.** A cache hit is reuse, not throughput. Warm-cache completion
time is not wide-area throughput, which is what `DeliverySource` exists to make
visible. No benchmark in this repository reports a cache-served completion as a
transfer rate.

**No publisher-authenticity claim.** A trusted digest supplied by metadata
establishes representation identity, not who published the object. An observed
local digest records what was retained.

**No durability claim beyond what was tested.** Insertion and index writes use
temporary file, durability barrier, then rename. No power-loss testing was
performed; process-level tests cannot establish power-loss behaviour.

## Known limitations

1. **Paired LAN mode is not implemented.** Device identity, pairing, peer
   serving and the upload budget do not exist yet. `Provenance` and
   `CacheEntry::is_shareable` are in place and tested, but nothing currently
   reads them to serve a peer, because there is no peer path. FP-020 stays
   `in_progress` for this reason.
2. **mDNS discovery is deferred** to its own task by decision on
   22 September 2026.
3. **The desktop settings surface is deferred.** Quota and the LAN flag have no
   user-facing control yet; `CacheConfig` is set by the caller. CLI controls
   were planned for this task and have not landed.
4. **The store is single-process.** `ContentCache` takes `&mut self` and
   performs no cross-process or cross-thread locking. Two processes sharing one
   store directory could race on the index. Not exercised and not safe to
   assume.
5. **Eviction resolution is one second.** `last_used_at_secs` has second
   granularity; entries used within the same second are ordered by content id.
6. **No cache-hit path for partial content.** A hit is whole-artifact only. A
   partially downloaded file is not repaired from cache.
