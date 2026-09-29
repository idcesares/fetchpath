# FP-015 measured adaptive HTTP transfers

Date: 21 September 2026  
Acceptance: A03, A05, A06, A09

## Delivered behavior

`fetchpath-http` now owns bounded HTTP protocol selection, identity probing, adaptive range scheduling, and raw transfer diagnostics. The core feeds its existing checkpoint and publication contract from verified, ascending chunks:

- A one-byte `Range` probe establishes exact length and a strong ETag. A server that returns `200` instead streams sequentially without buffering the full object.
- Every segmented request uses `Accept-Encoding: identity`, an exact byte range, and `If-Range` with the probe's strong ETag. Status, `Content-Range`, ETag, and actual byte count must all match.
- The controller starts at one request, requires two healthy observations before growing, reduces concurrency after a measured 20% rate regression, and applies a two-observation cooldown.
- One process-wide budget caps active requests at 8 and buffered range bytes at 32 MiB (8 MiB before 28 September 2026). Ranges start at 1 MiB and grow to at most 8 MiB, sized to about 1.5 s of each lane's measured rate. Each buffered segment reserves its full capacity before a request starts and releases it through RAII on success, failure, panic propagation, or cancellation.
- Chunks are sorted and passed to the core only in ascending order. Adaptive protocol/range failure resets the unpublished staging file before the established sequential path runs; sink, cancellation, checkpoint, and publication errors do not take that fallback.
- The static libcurl build now includes nghttp2. Automatic HTTPS requests prefer H2 and may negotiate lower; controlled cleartext H2 uses an explicit prior-knowledge mode. The packaged build still reports HTTP/3 unavailable, so the policy records `http3_unavailable` and does not claim or simulate an H3 attempt.

## Verification

The Rust suites cover strict range parsing, controller hysteresis/cooldown, impossible reservations, concurrent global budget enforcement, the existing interruption/publication adversarial cases, and a 5 MiB multi-request reassembly through the production core. The Node fixture suite retains Range, validator, refusal, source-change, and truncation coverage.

The controlled protocol matrix produced matching 5 MiB hashes over both H1 and H2-prior-knowledge. H1 recorded preferred H2, negotiated HTTP/1.1, and explicit H3/lower-protocol fallback. H2 recorded negotiated `h2`; H3 was not attempted because the packaged capability is false. Raw observations are in [the protocol matrix](evidence/http-adaptive/fp015-protocol-matrix.json).

The paired pilot used five deterministically randomized 8 MiB loopback pairs. Every Fetchpath and curl output matched SHA-256 `a9407298d138c39f01e2067ad330ea65db7fa553a7cb541fd8bfd243c5405c45`. Fetchpath's wall-clock median was 81.3978 ms and curl's was 56.8628 ms. Fetchpath was therefore slower in this pilot; the first Fetchpath sample also showed a large startup outlier. Peak observed Fetchpath concurrency was 2, peak active requests was 2, and peak buffered bytes was 2,097,152, all within the configured global limits. See [the raw paired observations](evidence/http-adaptive/fp015-loopback-pilot.json).

## Internet throughput (28 September 2026)

Loopback had hidden two defects that the first measurement over the internet
exposed (FP-022). Downloading a 115 MB GitHub release asset took 15.6 s where
curl took 2.5 s, and a 440 MB Hugging Face file 269 s where curl took 7.1 s.

1. **Every 1 MiB range opened a new connection.** `fetch_range` built a fresh
   curl handle per range, so each MiB paid TCP, TLS, slow start and, on
   Hugging Face, a redirect to the CDN: a cold 1 MiB range measured 1.5–1.6 s,
   matching the 1.7 s per round observed over 105 rounds. Each lane now keeps
   one handle across rounds and so reuses its connections, and ranges grow
   from 1 MiB to 8 MiB with the lane's rate. They grow rather than start
   large so that on a slow link progress still moves and a crash loses about
   a round's seconds, not 32 MiB; a job's `bytes_received` now includes a
   range's bytes held in memory, and what survives a crash stays the
   checkpoint.
2. **Every checkpoint re-hashed the whole staging file.** With a strong ETag
   the sink recomputed SHA-256 from byte 0 at each checkpoint (every 64 KiB on
   the sequential path), quadratic in the file's size. Both paths now extend a
   running digest of the in-order prefix; a resume seeds it once from the kept
   bytes. The final digest is still computed from what is on disk.

Transfer layer alone (`fetchpath-http-bench`): 7.6 s → 3.1 s with reuse →
2.4 s with 8 MiB ranges for the GitHub asset (curl 2.5 s); 177 s → 58 s →
18.8 s for the Hugging Face file. End to end through the engine, with ranges
growing from 1 MiB: 3.5 s and 26.2 s. One run each on a home connection, 28 September 2026: evidence of the
defects and their size, not a speed claim.

## Benchmark harness and baseline (FP-084, 29 September 2026)

`tools/bench/benchmark.mjs` runs paired downloads with a seeded random client order per repetition and profile. Clients: `fetchpath-http` (transfer layer, `fetchpath-http-bench`), `fetchpath-cli` (`fetchpath download`, the whole engine), curl, aria2 (defaults and `-x16 -s16 -k1M`) and wget2. A missing tool is skipped with a recorded reason. The fixture server shapes traffic itself: per-connection limit, per-client limit, delay, stalls, redirect to a second origin, weak ETag, no ETag. Per run it records time to a SHA-256-verified file, goodput, process CPU and peak memory, connections and requests seen by the server, and a probe request every 100 ms (p50, p95). Summaries give median, min, max and a bootstrap 95% interval of the median. Usage and profile parameters are in `tools/bench/README.md`. To measure a new scheduler, point `--http-bench` and `--cli` at its binaries.

Both engine clients use `TransferLimits::default()` (segments 1 to 8 MiB, up to 4 lanes, 8 active requests, 32 MiB buffered), so the difference between them is engine overhead: queue, checkpointing, running SHA-256, publication.

Baseline: the engine as of commit 8071460 (bench binary built from this tree, CLI from the main checkout's release build of the same day), 64 MiB, 5 repetitions, one machine (Windows 11, NTFS, SSD, loopback). Median time to verified file in seconds, with the interval in brackets. Raw data: [fp084-baseline.json](evidence/http-adaptive/fp084-baseline.json).

| profile | transfer layer | engine (CLI) | curl (1 connection) |
|---|---|---|---|
| unshaped | 0.47 [0.40-0.57] | 0.87 [0.66-1.03] | 0.44 [0.31-0.52] |
| per-connection-limit 8 MiB/s | 4.41 [4.37-4.44] | 4.82 [4.79-5.01] | 8.25 [8.22-8.28] |
| per-client-limit 16 MiB/s | 4.29 [4.23-4.31] | 4.80 [4.73-4.87] | 4.22 [4.21-4.27] |
| delay 50 ms, 32 MiB/s per connection | 1.66 [1.62-1.68] | 2.09 [1.93-2.22] | 2.33 [2.26-2.35] |
| stall (every 4th range pauses 3 s) | 6.58 [6.47-6.64] | 6.95 [6.93-7.24] | 0.38 [0.37-0.44] (never stalled) |
| redirect | 0.51 [0.47-0.59] | 0.94 [0.83-1.17] | 0.33 [0.28-0.40] |
| weak-etag | fails: needs strong ETag | 0.73 [0.63-0.80] | 0.41 [0.39-0.50] |
| no-etag | fails: needs strong ETag | 0.87 [0.77-0.94] | 0.40 [0.32-0.45] |

What it shows about the current engine:

- Per-connection limit: about 1.9x curl, from up to 4 lanes. Per-client limit: no gain over curl, as expected, and about 14% slower through the engine.
- Stall: the 11 range requests hit two 3 s pauses, so both engine clients take about 6.6 s against 0.47 s unshaped, because each round waits for its slowest range. This is the straggler cost the FP-083 scheduler targets.
- Unshaped and redirect loopback: the engine takes about 1.9x the transfer layer and 2 to 3x curl. The engine's share is about 0.4 s per 64 MiB. On shaped profiles the overhead is 6 to 25%.
- Weak or absent validators: the transfer-layer binary returns "segmentation requires a strong ETag" and does not fall back to a single stream; the engine does fall back, at about 2x curl's time on loopback.
- The probe p95 stayed at 17 to 28 ms for every client on loopback; no client starved the probe here.
- CPU per GiB is not comparable across the two Fetchpath clients: the CLI number covers only the small command process, the engine runs in another process. Windows CPU time has about 15.6 ms resolution, so short runs are coarse. Peak memory has the same scope.

Internet corpus, one home connection, 3 runs each, 29 September 2026 (not a speed claim; [fp084-internet.json](evidence/http-adaptive/fp084-internet.json)). Median seconds to verified file: GitHub release 113 MB: transfer layer 3.07, engine 3.93, curl 2.05. Hugging Face 440 MB: 22.1, 23.5, 9.5 (curl's interval 9.2 to 18.8). Kernel mirror 140 MB: transfer layer fails (no strong ETag), engine 3.9 (interval 3.2 to 43.0), curl 2.6. The GitHub and mirror hashes are self-recorded from the first successful run; the Hugging Face hash is the published one. aria2 and wget2 are not installed on this machine, so no rows exist for them.

NTFS write order (`tools/bench/ntfs-write.mjs`, 256 MiB in 1 MiB positional writes plus fsync, median of 3, [fp084-ntfs-write.json](evidence/http-adaptive/fp084-ntfs-write.json)): sequential 216 ms; out-of-order in 4 interleaved lanes 281 ms; out-of-order in shuffled 8 MiB segments 268 ms; the same two after `ftruncate` to full size 254 ms and 303 ms. Out-of-order writes cost about 25 to 40% more here, and setting the end of file first did not help, since NTFS still zero-fills up to the valid data length. Absolute times are small because this is a fast SSD and the cache absorbs most of the writes; a slower disk may show a larger gap.

Harness limits: application-level shaping only (no packet loss, no real congestion or RTT); a single-request client is never stalled; loopback timings compare builds and are not internet speeds; five runs give intervals that understate uncertainty; the Node fixture and the tools share one machine.

## Limits carried forward

No speed advantage is claimed. Loopback does not model RTT, loss, congestion, blocked UDP, slow storage, or competing traffic, and five pairs cannot establish tail percentiles. Each lane keeps its own easy handle; ranges are fetched in lock-step rounds, each waiting for its slowest range, and every range on Hugging Face still follows the redirect to the CDN, which keeps a 440 MB Xet-backed file at about 2.6× curl's time. H2 is negotiated but one multiplexed H2 connection is not shared across ranges. A packaged HTTP/3 backend, controlled H3 endpoint, and blocked-UDP fallback run remain required before any H3 support claim.
