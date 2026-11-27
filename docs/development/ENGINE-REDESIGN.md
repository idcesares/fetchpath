# Engine 2.0 redesign (M20: FP-145–FP-156)

Spec: [Engine 2.0 redesign](../architecture/specs/2026-11-26-engine-redesign.md) · Contract: [JOB-CONTRACT D10](../architecture/JOB-CONTRACT.md#d10--engine-20-data-plane-extent-checkpoints-pacing-and-scheduling-26-november-2026) · Acceptance: A02, A03, A04, A05, A06, A07, A09, A11

This is the single live verification and measurement record for Milestone M20. Update it in place as each M20 task lands; raw paired runs go in `docs/development/evidence/http-adaptive/` or `docs/development/evidence/engine-session/`.

## Baseline (0.4.1 / pre-M20)

Recorded before M20 changes, from [fp086-internet.json](evidence/http-adaptive/fp086-internet.json), [fp087-comparison.json](evidence/http-adaptive/fp087-comparison.json), and [fp084-ntfs-write.json](evidence/http-adaptive/fp084-ntfs-write.json):

| Workload | Pre-M20 Fetchpath | Comparator | Primary diagnosed cause |
| --- | --- | --- | --- |
| Hugging Face 440 MB (Internet, 3 pairs) | **192.71 s** (2.19 MiB/s, 78–164 `prefixStarvation` replacements, 132–193 conns) | `curl` **37.80 s** (11.4 MiB/s) | Zero-margin `slower_than_median` + 1 s `BLOCK_GRACE` + 3 s `max_ahead` thrashing loop (`FP-145`) |
| Loopback 64 MiB `unshaped` (engine) | **0.69 s** (3 conns) | `curl` **0.34 s** (1 conn) | 3× full-file SHA-256 passes + per-job JSON commit latency (`FP-146`, `FP-156`) |
| Loopback 64 MiB `no-etag` (engine) | **0.73 s** (1 conn) | `curl` **0.28 s** (1 conn) | 3× full-file SHA-256 passes + no strong `Last-Modified` path (`FP-146`, `FP-149`) |
| Loopback 64 MiB `per-connection-limit` (engine) | **3.24 s** (5 conns) | `aria2 ×16` **0.79 s** (16 conns) | 4-lane default cap + irreversible controller lock-in (`FP-145`) |
| NTFS 256 MiB positional writes + fsync | **281 ms** (4 interleaved lanes) | **216 ms** (sequential, +30% overhead) | 16 KiB unbuffered `seek_write` + NTFS VDL zero-fill (`FP-148`) |
| HTTP/3 (QUIC) | `http3_unavailable` | — | Packaged `static-curl` lacks QUIC/HTTP/3 (`FP-152`) |

## Delivered in M20

- **`FP-145` (HTTP/2 & adaptive scheduler P0 fixes):**
  - Replaced zero-margin prefix-starvation checks with hysteresis (`PREFIX_SLOW_SHARE = 0.35`, `PREFIX_MIN_AGE = 1500 ms`, `BLOCK_GRACE = 2 s`, `BLOCK_REPLACE_COOLDOWN = 3 s`) and time-based asymmetric goodput smoothing (`GOODPUT_WINDOW = 3 s`, `GOODPUT_RISE_TAU_SECS = 0.25`, `GOODPUT_DECAY_TAU_SECS = 1.5`) in `crates/fetchpath-http/src/scheduler.rs`.
  - Scheduled the HTTP/2 independent-socket `PoolPolicy::Trial` right after the `width = 2` baseline and unlocked controller width growth (`Controller::resume_probing`) when independent sockets gain `>= 15%`; aligned `easy.pipewait` with `PoolPolicy`; added `Controller::revert_starved()` when a doubled probe cannot fit within `max_ahead`.
  - Added bounded cooldown re-probing after transient dips (`REPROBE_COOLDOWN_WINDOWS = 24`, `RECOVERY_COOLDOWN_WINDOWS = 12`, capped at `96`) while keeping 429/503 throttle lockout permanent (`crates/fetchpath-http/src/controller.rs`).
  - Raised `TransferLimits::default()` ceiling to `max_concurrency: 8`, `max_active_requests: 16` (`crates/fetchpath-http/src/lib.rs`).
- **`FP-146` (Single-pass pipelined SHA-256 & callback isolation):**
  - Removed synchronous disk read-back and SHA-256 from `PrefixTracker` (now in-memory extent merging only) and moved incremental hashing into the background `CommitQueue` worker thread with a reused 256 KiB read buffer (`crates/fetchpath-core/src/checkpoint.rs`).
  - Added `Store::validate_staging_hasher` (seeding incremental `Sha256` on resume without a second disk read) and `Store::publish_verified` (O(1) staging length check + `PublicationIntent`/`PublicationFence`/hard-link publication) while retaining full disk `validate_staging()` for crash recovery (`crates/fetchpath-storage/src/lib.rs`, `crates/fetchpath-core/src/transfer.rs`).
- **`FP-147` (Extent-bitmap checkpoint format v2 & prefix-choke removal):**
  - Upgraded `fetchpath-storage` checkpoints to Format v2 (`CompletedExtent { start, end, sha256 }` list strictly past `committed_len`, strict invariant validation on `commit`/`decode_record`/`validate_staging_state`, full v1 backward-compatible load + v2 upgrade on commit, and `PublicationIntent` forbidding non-contiguous extents).
  - Upgraded `PrefixTracker`, `CommitQueue`, and `transfer.rs` to cache per-extent SHA-256 digests on the background worker without re-reading already-hashed extents, commit out-of-order extents on periodic 1 s ticks (`>= 64 KiB` slices) and graceful `RetainStaging` pauses (all slices), and fold merged extents into the single-pass file digest.
  - Extended `ResumePoint` with `completed_extents` so `fetchpath-http` builds its `Frontier` strictly over missing gaps (clipping lane 0 at the first retained extent, treating `offset == 0` with extents as a verified resume across `200`/`206`/`416`), and lifted the default run-ahead window from `3 s` to `12 s` (`TransferLimits.max_ahead_seconds`).

## Verification & paired measurements

- **Phase 1 checks & review:** `cargo fmt --check`, strict Clippy (0 warnings), `fetchpath-storage` 6/6, `fetchpath-core` 79/79, `fetchpath-http` unit + 5/5 compat + 38/38 scheduler; strong boundary review approved `FP-146` and cleared `FP-145` after bounded repair recheck (**PASS**).
- **Phase 2 (`FP-147`) checks & review:** `cargo fmt --check`, strict Clippy (0 warnings), `fetchpath-storage` 8/8 (v1 upgrade + v2 extent validation/corruption/invariant checks), `fetchpath-core` 82/82 (cached extent reuse/folding, out-of-order pause/resume, zero-offset extent resume/restart), `fetchpath-http` 59/59 unit + 5/5 compat + 40/40 scheduler (gap-only resume, zero-offset `200`/`416`/`206` extent resume); strong boundary review flagged one `416` zero-offset edge case in `Engine::on_error`, which was fixed with regression coverage and cleared by bounded repair recheck (**PASS**).
- **Paired benchmarks** ([fp145-hf-internet.json](evidence/http-adaptive/fp145-hf-internet.json), [fp145-fp146-loopback.json](evidence/http-adaptive/fp145-fp146-loopback.json), [fp145-resume.json](evidence/http-adaptive/fp145-resume.json), [fp146-interruption.json](evidence/http-adaptive/fp146-interruption.json), [fp147-interruption.json](evidence/http-adaptive/fp147-interruption.json), [fp147-resume.json](evidence/http-adaptive/fp147-resume.json)):

| Workload | Pre-M20 (0.4.1) | Current M20 | Paired comparator / result |
| --- | --- | --- | --- |
| Hugging Face 440 MB (Internet, 3 pairs, `FETCHPATH_HTTP_TRACE=1`) | **192.71 s** (2.19 MiB/s, 78–164 `prefixStarvation`, 132–193 conns) | **16.53 s** (27.98 MiB/s, **0** `prefixStarvation`, **0** retries, **2** H2 conns, 32–38 reqs) | `curl` **14.67 s** (32.52 MiB/s) — **1.13× curl** (gate `<= 1.15×`) |
| Loopback 64 MiB `per-connection-limit` (engine vs transfer) | `2623.5 -> 3047.2 ms` wall (`+423.7 ms`, `x1.17` verified) | `2738.1 -> 2752.7 ms` wall (`+14.6 ms`, `x1.02` verified) | `curl` **8.27 s** — **2.76× faster than curl**; **-97%** engine overhead |
| Loopback 64 MiB `per-client-limit` (engine vs transfer) | `4084.6 -> 4479.4 ms` wall (`+394.8 ms`, `x1.10` verified) | `4186.9 -> 4191.0 ms` wall (`+4.1 ms`, `x1.01` verified) | `curl` **4.31 s** — **-99%** engine overhead, no regression |
| Loopback 64 MiB `delay` (engine vs transfer) | `1235.9 -> 1489.9 ms` wall (`+254.0 ms`, `x1.25` verified) | `1335.7 -> 1313.6 ms` wall (`-22.1 ms`, `x0.99` verified) | `curl` **2.36 s** — **1.52× faster than curl** |
| Loopback 64 MiB `redirect` (engine vs transfer) | `172.5 -> 686.3 ms` wall (`+513.8 ms`, `x2.59` verified) | `500.9 -> 542.9 ms` wall (`+42.0 ms`, `x1.18` verified) | **-92%** engine overhead |
| Loopback 64 MiB `unshaped` / `no-etag` (engine vs transfer) | `+371.2 ms` / `+458.5 ms` wall (`x2.26` / `x2.28` verified) | `+139.4 ms` / `+140.5 ms` wall (`x1.24` / `x1.14` verified) | **-62% / -69%** engine overhead (zero double-hash pass) |
| Resume 64 MiB (8 MiB retained, 5 pairs, `FP-147`) | — | Single **7.26 s** vs adaptive **2.70 s** (**-62.8%**) | 5/5 verified (up from 3.01 s / -59.1% in Phase 1) |
| Interruption 256 MiB (`SIGKILL`, `FP-147` v2 extents) | — | Gen 4: `88.5 MB` prefix + **9 extents** = **`143.0 MB` retained** (+54.5 MB saved); loss `25.3 MB` (`0.38 s` @ `66.3 MB/s`) | Resumed & SHA-256 verified in `9.27 s` |

## Limits still held

- Sub-second 64 MiB loopback bursts (`unshaped` / `no-etag`) still show ~140 ms of engine overhead over raw transfer (`x1.24` / `x1.14` verified) from unbuffered 16 KiB positional writes, final `sync_data`, and per-job JSON metadata writes (addressed by `FP-148` buffered I/O and `FP-156` SQLite journal).
- Process-kill interruption tests verify `recoverable` durability across process termination, not OS crash or power loss.
- Until `FP-152` lands, HTTP/3 remains unavailable (`http3_unavailable`).
- Until `FP-153` and `FP-154` land, Metalink multi-source downloads remain sequential and FTP/FTPS/SFTP stop at `fetchpath-http`.
