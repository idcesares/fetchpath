# Bounded content cache and paired LAN mode

Task: FP-020. Milestone M8. Status: design approved 22 September 2026.
Acceptance ids: A02, A05, A10.

## Purpose

Avoid re-fetching bytes the machine already holds, and let a user's own
explicitly paired devices reuse those bytes over a LAN, without ever making a
content hash act as permission to redistribute an object.

Two decisions bound everything below. Only content carrying a *trusted* digest
is eligible, so every cache hit is provable rather than assumed. Only content
whose provenance was recorded as credential-free may ever leave the machine.

## Scope

In scope: the bounded content-addressed store, its quota and eviction, reuse
inside the verified download path, device pairing, and peer serving and
fetching behind a default-off flag, with controls exposed through `apps/cli`.

Out of scope, each recorded as its own follow-up rather than silently dropped:
mDNS peer discovery, and the desktop settings surface for the quota and the LAN
flag. Deferred by explicit decision on 22 September 2026.

## 1. Units and boundaries

`crates/fetchpath-cache` owns the store, the index, quota accounting and
eviction. It performs no networking and has no knowledge of mirrors.

`adapters/lan` owns device identity, pairing, the peer session and the upload
budget. It depends on the cache crate's public interface only.

The cache is a separate crate from `fetchpath-storage` because the two own
different lifecycles. Storage owns staging, checkpoints and publication for one
destination. The cache owns a durable multi-entry store with a quota, and must
be exercisable without performing a download at all. `adapters/lan` can then
depend on the cache without inheriting staging machinery it must never touch.

## 2. Content identity

A content id encodes both algorithm and construction, because a flat digest and
a digest over a piece map identify the same bytes by different means and are
never interchangeable.

```rust
pub enum ContentId {
    FlatSha256([u8; 32]),     // "sha256:<hex>"
    PieceMapSha256([u8; 32]), // "pieces-sha256:<hex>"
}
```

`PieceMapSha256` is SHA-256 over a canonical encoding of a trusted piece map:
piece length, total size, then the ordered piece hashes. It exists because a
Metalink 4 file may supply piece hashes and no whole-file digest. That content
is still trustworthily identified, by a different construction.

A key is derived only from trusted metadata supplied by the request. An
observed local digest records what was retained; it is not an identity the
cache may key on, and it never becomes one.

## 3. The store

Layout is `cache/<algorithm>/<hex>` beneath the per-user local application data
directory. Insertion copies the artifact through a temporary file, completes the
durability barrier, and renames, reusing the no-overwrite discipline already
present in `fetchpath-storage`.

The index is a generation-numbered file written by temporary file and rename. It
is self-repairing: a truncated or malformed index is rebuilt by walking the
store directory, matching the bounded self-repairing settings file introduced in
FP-028. Quota is accounted from the index and is reconcilable against the
directory contents.

Each entry records its content id, size, insertion time, last-used time,
`VerificationLevel`, and `Provenance`.

`Provenance` is `Public` or `Credentialed`. It is decided at insertion from the
request that actually delivered the bytes: a default `RequestContext`, meaning
no cookie lines and no referer, together with a delivering mirror URL carrying
no query string, yields `Public`. Anything else yields `Credentialed`.
Provenance is immutable after insertion. An entry that ever touched credentials
cannot become shareable later, whatever a later setting says.

Eviction is least-recently-used on last-used time, run at insertion to make
room. An entry pinned by an in-flight reuse is never evicted. An entry larger
than the configured per-entry ceiling is refused outright rather than evicting
the store to accommodate one file.

Reuse re-verifies before publishing. `reuse_into` checks the cached bytes
against the same trusted digest or piece map the current request carries, then
publishes through the existing create-only fence. A cached file that fails that
check is evicted and reported as a miss. A corrupt cache must never produce a
bad publication, and must never convert into a download failure either.

## 4. Engine integration

`VerifiedDownloadRequest` gains an optional cache handle. Before any mirror is
contacted, a content id is derived from the request's trusted metadata. On a
hit the bytes are verified and published with no network access.

`VerifiedDownload` gains a delivery source of `Network`, `LocalCache`, or
`Peer { fingerprint }`.

That field is load-bearing rather than decorative. Warm-cache completion speed
is not wide-area throughput, so the benchmark harness must exclude or flag any
completion whose source is not `Network`, and the interface must state that a
file came from cache rather than reporting an implausible transfer rate.

The cache is optional at every point and fatal at none. An unwritable directory
disables it for the session after one event. A quota refusal is recorded rather
than raised. Every failure falls through to the existing mirror logic unchanged.

## 5. Pairing and peer serving

Each device holds a long-lived ed25519 identity, stored under DPAPI protection,
matching the pattern the browser extension already uses for its inbox.

Pairing is out-of-band. Device A displays a single-use code that expires after
two minutes. The user types it on device B. An X25519 exchange bound to that
code by HMAC lets each side prove possession of the code while exchanging public
keys, which both sides then pin. Later sessions authenticate against pinned keys
only, so an unpaired peer is refused by construction and there is no
trust-on-first-use window.

This is not a password-authenticated key exchange and is not described as one.
An attacker who captures a handshake can attempt an offline guess against the
code. The code is therefore single-use, expires in two minutes, and carries at
least 40 bits of entropy. This limitation belongs in the evidence document.

Serving a peer requires all of: LAN mode enabled, the requesting peer pinned,
the entry's provenance `Public`, and the entry carrying a trusted digest. Bytes
received from a peer are re-verified against the receiver's own trusted digest
before publication. A peer is a source, never an authority. Upload rate and a
per-session byte budget are enforced.

## 6. Error handling

A corrupt store entry is evicted, counted, and reported as a miss. Exceeding the
quota skips insertion and is recorded. An unwritable cache directory disables
the cache for the session with a single event. An unreachable or refusing peer
falls through to mirrors. A peer that delivers bytes failing verification is
deprioritised for the session and its bytes are discarded unpublished.

## 7. Verification

Unit coverage: content id construction distinctness; index round-trip and
rebuild from a corrupted index; quota accounting; least-recently-used ordering;
pinned entries surviving eviction.

Behavioural coverage, mapped to the acceptance line:

- Authorized offline reuse. With the cache populated and every mirror
  unreachable, the download completes from cache and the published file is
  byte-identical and verified.
- Eviction and quota. An oversize entry is refused. A quota breach evicts the
  least recently used entry and only that entry.
- No private-content leakage. A `Credentialed` entry is never served to a peer,
  and provenance cannot be mutated after insertion.
- Explicit pairing and sharing. A paired peer retrieves a public entry. An
  unpaired peer is refused before a single byte is served. A wrong code fails.
  An expired code fails. The upload budget is enforced. Malformed frames are
  refused under bounded memory.
- Corruption. A tampered store file yields a miss and an eviction, never a bad
  publication.

Commands: `cargo test --workspace --locked`,
`cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --check`,
and `node tools/tasks.mjs check` for the backlog change.

## 8. Claims this work does not make

No speed claim. Sequential peer retrieval, like mirror fan-out before it, is not
presented as acceleration. A cache hit is reuse, not throughput.

No publisher-authenticity claim. A trusted digest supplied by metadata
establishes representation identity, not who published the object.

No durability claim beyond what is tested. Process-level tests do not establish
power-loss behaviour.

## Evidence

`docs/development/CACHE-AND-LAN.md` and `docs/development/evidence/cache`.
