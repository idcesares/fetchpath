# Bounded content cache and paired LAN mode

Task FP-020. Cache half recorded 22 September 2026; paired LAN half recorded
23 September 2026. Status: both halves are implemented and tested. An
independent strong-model review of `adapters/lan`, which AGENTS.md requires for
credential boundaries and FFI, has **not** been performed yet, so FP-020 stays
`in_progress`.

Design: [content cache and paired LAN design](../superpowers/specs/2026-09-22-content-cache-and-paired-lan-design.md).
Plans: [bounded content cache](../superpowers/plans/2026-09-22-bounded-content-cache.md),
[paired LAN](../superpowers/plans/2026-09-22-paired-lan.md).

## What exists: the cache

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

`VerifiedDownload` now carries a `DeliverySource` of `Network`, `LocalCache` or
`Peer { fingerprint }`.

Pins are reference-counted. A local reuse and a peer upload can hold the same
entry at once, so one holder's release no longer unpins it for the other.

## What exists: paired LAN mode

`adapters/lan` (crate `fetchpath-lan`) depends on `fetchpath-cache` only. Core
defines a `PeerSource` trait and `download_verified_shared`; `apps/cli` adapts
the LAN client to that trait. The order is local cache, then paired peers in
the order given, then mirrors. No crate was downloaded: every primitive
(`ed25519-dalek`, `curve25519-dalek`, `hkdf`, `hmac`, `sha2`, `aes-gcm`,
`getrandom`, `zeroize`) was already in `Cargo.lock` through `russh`, and the
lockfile gained only the new package entry.

**Identity.** Each device holds a long-lived ed25519 key, sealed with DPAPI
under the entropy label `fetchpath-lan-identity-v1`, distinct from the browser
inbox's label. A sealed file that cannot be unsealed is an error, never a
reason to generate a replacement, because every device that pinned the old key
would silently stop recognising this one. `Debug` output shows only the
fingerprint.

**Pairing.** The host shows a ten-character Crockford base32 code (50 bits)
that expires after two minutes and is consumed by the first hello whatever the
outcome. Each side sends an X25519 ephemeral key, its identity key and a nonce.
Confirmation keys are `HKDF-SHA256(salt = transcript hash, ikm = X25519 shared
secret || code)`. The joiner confirms first with an HMAC and a signature over
the transcript; the host checks both before it pins the joiner and replies in
kind. A mistyped code is rejected before anything is sent, so it never costs
the host its single use.

**Sessions.** Authentication is against pinned keys only; there is no
trust-on-first-use path. The server checks the client's key against its pins on
the first frame and sends one refusal and nothing else if it is not pinned.
Both sides sign the transcript; a client that copied a pinned public key
without its private key fails there. Traffic is AES-256-GCM, with one key per
direction from HKDF over the X25519 secret and a per-direction counter nonce
that refuses to wrap. The frame kind is associated data. The spec asked for
authentication; encryption was added because `aes-gcm` was already locked.

**Serving.** An entry leaves the machine only when LAN mode is on, the peer is
pinned and authenticated, and the entry's provenance is `Public`. An absent
entry, a `Credentialed` entry and a request made while LAN mode is off all get
the same `not_available` refusal, so a paired peer cannot learn that private
content exists. The flag is checked per request as well as per connection. A
per-session byte budget is checked against the offered size before anything is
sent, and a per-transfer pacer holds the average upload rate to the configured
limit.

**Receiving.** The receiver never reads more than the size its own trusted
metadata declares. Peer bytes land in staging on the destination volume and are
checked against the request's own digests before publication through the
unchanged create-only fence. A peer whose bytes fail is reported `Corrupt` and
not asked again for that download. Peer bytes are **not** inserted into the
receiver's cache: the receiver cannot verify the sender's provenance claim, so
it does not create an entry that a later setting could share.

**Frames.** Every frame's length is checked against a 64 KiB + 16 byte limit
before allocation. Handshake reads time out after ten seconds.

**CLI.** `fetchpath lan id | enable | disable | peers | unpair KEY |
pair-host [BIND] | pair-join ADDRESS CODE [LABEL] | serve [BIND]`,
`fetchpath cache status`, and `fetchpath fetch-verified --sha256 HEX --size N
[--peer ADDRESS=KEY]... URL DESTINATION`. State lives under
`%LOCALAPPDATA%\Fetchpath`, or `FETCHPATH_DATA_DIR`. LAN mode is off until
`lan enable`. `fetch-verified` refuses a `--peer` key that is not pinned. A
running `lan serve` rereads the flag every two seconds. The existing
`fetchpath download` command is unchanged.

## Commands actually run

```
cargo test --workspace --offline
cargo clippy --workspace --all-targets --offline -- -D warnings
cargo fmt --check
node tools/tasks.mjs check
```

Cache half, results on 22 September 2026, Windows 11 Pro 26200, x64:

- `cargo test --workspace` — 158 passed, 0 failed. The recorded baseline before
  this work was 120 passed, 0 failed.
- `fetchpath-cache` — 29 tests (14 unit, 15 behavioural).
- `fetchpath-core` — 45 tests, up from 36.
- `cargo clippy --workspace --all-targets -- -D warnings` — clean.
- `cargo fmt --check` — clean.
- `node tools/tasks.mjs check` — PASS.

Per-test outcomes: [cache matrix](evidence/cache/fp020-cache-matrix.json).

LAN half, results on 23 September 2026, same machine, same four commands with
`--locked`:

- `cargo test --workspace --locked` — 205 passed, 0 failed, 4 ignored. The 4
  ignored tests were already marked ignored before this work. The baseline
  before the LAN half was 158 passed.
- `fetchpath-lan` — 40 tests (21 unit, 19 behavioural over loopback TCP).
- `fetchpath-core` — 51 tests, up from 45.
- `fetchpath-cache` — 30 tests, up from 29.
- `cargo clippy --workspace --all-targets -- -D warnings` — clean.
- `cargo fmt --check` — clean.
- `node tools/tasks.mjs check` — PASS.

Per-test outcomes: [LAN matrix](evidence/cache/fp020-lan-matrix.json).

### End-to-end CLI run

Two data directories on one machine stood in for two devices, over loopback.
Recorded 23 September 2026:

1. `lan pair-host` / `lan pair-join` paired them; each pinned the other.
2. Device A downloaded two files from a local HTTP server: one from a plain URL
   and one from a URL with a query string. `cache status` on A: 2 entries,
   1 shareable.
3. The HTTP server was stopped, and A ran `lan serve`.
4. B's `fetch-verified --peer` for the plain-URL file completed with
   `"source":"peer"` and output byte-identical to the original.
5. B's request for the query-string file was refused by A, then fell through
   to the dead mirror and failed. No file was published.
6. B's `cache status` afterwards showed 0 entries, because peer bytes are not
   cached.
7. A `--peer` key that was not pinned was refused locally with
   `lan.not_paired`.
8. After `lan disable` on A, the still-running server refused the same
   plain-URL request that had just succeeded.

### Leak checks verified adversarially

Each guard was removed in turn and the suite rerun, to confirm that the tests
detect the leak rather than merely pass:

- Removing the `is_shareable` check in `serve.rs` fails
  `a_credentialed_entry_is_refused_exactly_as_an_absent_one_is`.
- Removing the pin check in `session.rs` fails
  `an_unpaired_device_is_refused_before_a_single_byte_is_served`.
- Removing the re-verification of peer bytes in `verified.rs` fails
  `corrupt_peer_bytes_are_discarded_unpublished_and_that_peer_is_not_asked_again`
  and `when_every_peer_fails_the_mirrors_are_used_and_nothing_bad_is_published`.

### Cross-process coherence

Recorded 23 September 2026. Before this change, two handles on one store each
persisted their own view. The second writer silently erased the first writer's
entries, and quota was enforced against a partial picture. Five tests cover
the fix:

- `two_handles_on_one_store_never_drop_each_others_entries`
- `a_refreshed_handle_sees_entries_another_handle_inserted`
- `quota_is_enforced_against_what_every_handle_inserted`
- `concurrent_writers_through_separate_handles_lose_nothing` (two threads, 20
  inserts each, through separate handles)
- `a_running_server_serves_an_entry_another_process_inserted_after_it_started`

Skipping the reload makes the concurrent-writer test fail, and skipping the
server's refresh makes the running-server test fail.

After this change: `cargo test --workspace --locked` 210 passed, 0 failed,
4 ignored (pre-existing); clippy with `-D warnings` and `cargo fmt --check`
clean.

### A pre-existing test race, fixed

`tests::cancellation_removes_or_retains_unpublished_staging_by_policy` in
`fetchpath-core` hung in 3 of 6 parallel runs once the peer tests were added.
It cancelled after a fixed 30 ms. The download first waits for a permit from
the process-wide request budget, which the new tests' offline mirrors hold for
about two seconds each on Windows. So the cancel could arrive while the
download was still queued. The download then correctly returned without
connecting, and the test's server thread blocked in `accept` forever. The test
now cancels once the server signals that its first chunk is on the wire. With
that change, 10 of 10 parallel runs passed. Production behaviour did not
change.

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

**No speed claim.** A cache hit is reuse, not throughput. Peer retrieval is
sequential across peers and is not presented as acceleration. Warm-cache completion
time is not wide-area throughput, which is what `DeliverySource` exists to make
visible. No benchmark in this repository reports a cache-served completion as a
transfer rate.

**No publisher-authenticity claim.** A trusted digest supplied by metadata
establishes representation identity, not who published the object. An observed
local digest records what was retained.

**Pairing is not a PAKE.** A passive observer cannot test guesses, because the
confirmation keys also depend on an X25519 secret. An *active* attacker who
impersonates the host to the joiner receives the joiner's confirmation and can
attempt an offline guess of the 50-bit code. To complete the pairing, that
guess must succeed before the code expires, which is two minutes after it was
shown.

**No durability claim beyond what was tested.** Insertion and index writes use
temporary file, durability barrier, then rename. No power-loss testing was
performed; process-level tests cannot establish power-loss behaviour.

## Known limitations

1. **Independent review outstanding.** `adapters/lan` contains the credential
   boundary, the handshake cryptography and DPAPI FFI. AGENTS.md requires a
   strong-model review of those; the author's own review and the adversarial
   checks above do not substitute for it.
2. **mDNS discovery is deferred** to its own task by decision on
   22 September 2026. Peers are reached at an address the user gives.
3. **The desktop settings surface is deferred.** Quota and the LAN flag are
   controlled from the CLI only. The CLI uses a fixed 2 GiB quota and a 1 GiB
   per-entry ceiling.
4. **Pins are per handle.** Since 23 September 2026 the store is safe to share
   between processes: every mutation takes an exclusive lock on `cache/lock`,
   reloads the index, applies its change and persists before releasing. Large
   copies and verification run outside the lock, and incoming copies are
   written at the store root, where a rebuild never walks. A running
   `lan serve` refreshes its view before every request. What is *not* shared
   is pinning. A pin stops this handle from evicting an entry but not another
   process. A reader that loses its file that way sees a miss and falls
   through, never wrong bytes, because every reuse is re-verified and every
   peer transfer is re-verified by the receiver. No power-loss testing covers
   the lock.
5. **Eviction resolution is one second.** `last_used_at_secs` has second
   granularity; entries used within the same second are ordered by content id.
6. **No cache-hit path for partial content.** A hit is whole-artifact only. A
   partially downloaded file is not repaired from cache.
7. **The pin list is not sealed.** It holds public keys, written by temporary
   file and rename, and a malformed file is refused rather than partly read.
   Anyone who can write the user's profile can add a pin; that is the same
   boundary DPAPI protects.
8. **One-sided pins are possible.** The host pins the joiner before its final
   confirmation reaches the joiner. If that last frame is lost, the host holds a
   pin the joiner does not. That pin is useless without the joiner's pin of the
   host, and `lan unpair` removes it.
9. **Budget scope.** The byte budget applies per session and the pace per
   transfer; there is no daily or global upload cap. `lan serve` allows four
   concurrent sessions. The default bind is `0.0.0.0:47631`, so the listening
   port is visible on every interface. Only pinned peers receive anything.
10. **"Not asked again" is per download.** A peer reported `Corrupt` is skipped
    for the rest of that `download_verified_shared` call. Remembering it across
    calls is left to the caller.
11. **The server does not re-verify its own entries before sending.** The
    receiver's check is the gate; a tampered entry on the server is caught there
    and reported `Corrupt`.
12. **Pin membership is observable to someone who knows a pinned key.** The
    server sends its hello once the client's claimed key is pinned, before the
    client proves possession of that key. Anyone who knows a pinned device's
    public key can therefore learn that this server pins it, and learn the
    server's own public key. They get no session and no content.
