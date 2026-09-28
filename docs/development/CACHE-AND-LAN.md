# Content cache and paired computers

FP-020 (store and LAN adapter, 22–23 September 2026), FP-032 (the cache in
the engine and its clients), FP-033 (pairing and sharing through the engine
and the desktop) and FP-034 (discovery and receiving), 28 September 2026. Design:
[content cache and paired LAN](../architecture/specs/2026-09-22-content-cache-and-paired-lan-design.md).
The earlier per-step record is in Git history.

## What exists

**The store** (`crates/fetchpath-cache`) is bounded and content-addressed. Only
content with a trusted digest enters: `FlatSha256` from a whole-file checksum,
`PieceMapSha256` from a trusted Metalink piece map, never interchangeable. The
store holds no trust policy: every reuse runs the caller's own check, and an
entry that fails is evicted and reported as a miss. Provenance is `Public` or
`Credentialed`; a rebuilt index records every entry `Credentialed`. Every
mutation takes an exclusive lock, reloads the index and persists before
releasing, so handles in several processes never lose each other's entries.
Pins are reference-counted per handle.

**In the engine (FP-032).** A file job with a checksum, from the person or the
browser, is given the cache as it starts (`Session::reconcile_locked` →
`FileJob::use_cache`). The worker first tries `verified::reuse_for_file`: the
entry is checked against the job's own checksum and published through the
create-only fence, and the job reports `reused_from_cache` with no rate.
Otherwise it downloads; after the job reports completion, `remember_file`
copies the file in, `Public` only with no cookies or referrer, no query or
fragment and no user name. **An agent's job never reads or fills the cache**,
so an agent cannot obtain, or learn of, a file the person downloaded by naming
its checksum. `CacheStatus` and `ClearCache` are the person's only; the quota
is the setting `cache_quota_bytes`, 256 MiB to 256 GiB (2 GiB by default,
written only when changed), and lowering it trims at once.

**Paired computers (FP-020 adapter, FP-033 engine).** `adapters/lan`: each
device has an ed25519 identity sealed with DPAPI. Pairing shows a ten-character
code (50 bits) for two minutes, spent by the first attempt; both sides confirm
with HMACs over the X25519 secret, the code and the transcript, plus
signatures. Sessions authenticate pinned keys only (no trust on first use) and
are AES-256-GCM with per-direction keys. The server offers an entry only with
sharing on, to a pinned and authenticated device, and only if it is `Public`;
absent, `Credentialed` and sharing-off requests get the same refusal. Sharing
and pins are re-checked on every request. Frames are length-checked before
allocation; the receiver reads no more than its own declared size and checks
the bytes against its own digest before publishing, and does not cache them.

The engine owns all of it (`crates/fetchpath-session/src/lan.rs`): `LanStatus`,
`SetLanSharing`, `StartPairing`, `CancelPairing`, `JoinPairing` and `Unpair`,
the person's only and never ledgered, because pairing codes must not be
stored. Sharing is off until turned on; while on, the engine serves on
`0.0.0.0:47631` and stays running. Pairing listens on port 47632. The host
shows its code, address and fingerprint and then the joiner's fingerprint;
the joiner sees the host's. Failures read in plain words: a wrong code pairs
nothing, an expired or used code asks for a new one. The server and the engine
share one pin list, so Remove takes effect on the next request.

**Discovery and receiving (FP-034).** While sharing is on, the engine
broadcasts a 62-byte UDP beacon on port 47633 every five seconds: its serve
port, the time, a fresh 16-byte nonce and an HMAC-SHA256 over them keyed by
its own public key (`adapters/lan/src/discovery.rs`). Nothing is sent while
sharing is off. To anyone without that key the beacon is random bytes: no
name, fingerprint or content identifier. Every engine listens; a datagram of
any other length, another magic, a zero port, a time more than 60 s away or a
tag no pinned key produces is dropped, and at most one address is kept per
pinned device, for 30 s. The address is a hint: the session still
authenticates the pinned key, so a forged or replayed beacon only sends a
request to the wrong place. A person's or browser's checksum-verified job
then asks those devices after the cache and before the link
(`verified::fetch_file_from_peers`); their bytes are checked against the
job's checksum, are not cached, and the job reports `from_paired_device`
with no rate ("From your paired computer ..." in the desktop, `from paired`
in the terminal). An agent's job never asks paired devices.

State lives in one local data folder: `FETCHPATH_DATA_DIR`, else a moved
engine home, else `%LOCALAPPDATA%\app.fetchpath.desktop` (`cache\`, `lan\`),
which uninstall removes with the rest of the data when the person asks.

**Clients.** Desktop Settings: Cache (usage, size, Clear cache) and Paired
computers (fingerprint, sharing switch, paired list with Remove, Show a
pairing code, Pair with a computer that shows a code); a reused row reads
"Reused from this computer's cache". Command line: `fetchpath cache [status |
clear]`, `fetchpath lan [status | on | off | pair | join | unpair]` through the
engine, and `fetchpath fetch-verified --peer`, which asks paired devices
directly; `ls` shows `from cache`.

## How it was verified

- Store, adapter and core suites: cache coherence across handles and threads,
  a tampered entry falling through, provenance from the original URL,
  credentialed and absent entries refused alike, unpairing inside a live
  session, corrupt peer bytes discarded, concurrent identity creation. Each
  leak guard was removed in turn to confirm a test fails without it.
- Engine: `a_checksum_download_is_reused_from_the_cache_for_the_person_but_never_for_an_agent`
  (policy suite) and `two_engines_pair_with_a_code_and_the_person_controls_sharing`
  (`tests/lan.rs`: a wrong code pairs nothing, the right one pairs both ways,
  sharing off until on; the other engine hears the beacon, shows the address
  and completes a checksum-verified file from the sharing one with its own
  link dead, labelled with its fingerprint; unpair; agents refused). The
  beacon's own tests cover recognition by the pinned key only, a fresh nonce
  each time, and refusal of short, long, stale and altered datagrams.
- `tests/compatibility/windows/ui-cache.ps1` against the release desktop: the
  same file with its checksum completes from the cache with the server gone,
  byte-identical, with no speed shown; Settings shows use, every control is
  named, Clear cache empties it and keeps saved files.
  [evidence](evidence/desktop/ui-cache.json), 28 September 2026.
- `tests/compatibility/windows/ui-lan.ps1`, from the keyboard, the desktop as
  one computer and a second engine in its own data folder as the other, on
  loopback: sharing starts off; the code is shown with this computer's
  fingerprint; the other joins and each shows the other's fingerprint; the
  used code disappears; with sharing on the other computer receives a public
  checksum-verified file from this one with the link dead, and is refused one
  downloaded from a signed link; after Remove it is refused; sharing turns
  off; every control is named. [evidence](evidence/desktop/ui-lan.json),
  28 September 2026.
- Independent strong-model review of `adapters/lan` (23 September 2026):
  handshake cryptography and DPAPI FFI sound; two defects fixed (unpairing did
  not revoke a running server; a race in identity creation).

## Limitations that still hold

1. **Discovery works within one broadcast domain** (the local subnet), and
   only for a device that is sharing and whose engine is running. Someone who
   knows a device's public key (any device it paired with, or anyone who saw
   a session's hello) can recognize its beacons, so it can be tracked on the
   network by them.
2. **The host names every joiner "paired device"**; the joiner names the host.
3. **Sharing keeps the engine running**, and after a restart only resumes when
   the engine starts (at sign-in if that setting is on).
4. **Pairing is not a PAKE.** Impersonating a host lets an attacker test codes
   offline against 2^50 within two minutes. Anyone on the network can spend a
   code by connecting first; the person shows another.
5. **Knowing a pinned key reveals that it is pinned**, and the server's key,
   but gives no session or content.
6. **One-sided pins are possible** if the final pairing frame is lost; Remove
   clears it.
7. **Budgets are per session and per transfer**: no daily cap; four sessions
   at once; the port listens on every interface.
8. **A hit is whole-file only**, eviction resolution is one second, and a
   reuse copies the whole file.
9. **The server does not re-check its own entries before sending**; the
   receiver's check is the gate.
10. **No power-loss testing** covers the cache lock.
