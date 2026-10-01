# Adaptive HTTP transfers (FP-015, FP-084, FP-085, FP-086, FP-087)

Updated 30 September 2026

Acceptance: A02, A03, A04, A05, A06, A09

## Delivered behavior

`fetchpath-http` owns protocol selection, identity checks, range scheduling and raw transfer diagnostics. The core feeds its checkpoint and publication contract from the pieces the scheduler delivers. The design is [the stream-first scheduler spec](../architecture/specs/2026-09-29-stream-first-http-scheduler-design.md).

- **No probe.** The first request asks for `Range: bytes=O-` (O is 0, or the resume offset with `If-Range`). Its first write classifies the response: 200 at zero streams on one request; 206 with a strong ETag, the requested start and a known total is accepted; a capped 206, `bytes 0-9/*`, a 206 without a strong ETag, and 416 each have a rule; a 200 or 416 on a resume, another validator or another total restarts from byte zero once. The rules are the table in spec 4.1, one test per row.
- **One event loop per download.** All lanes are easy handles in one curl multi handle with a shared connection cache and HTTP/2 multiplexing. HTTP/1.1 overlapping lanes keep separate connections. A lane owns a claim `[start, end)`. Every write is clipped at the claim's current end, so a claim can be split or replaced while its owner is mid-transfer. A clipped short write is a deliberate stop, not a failure.
- **Claims.** Sized by time (1.5 s of a lane's rate, 1 MiB floor, at most four times the longest so far, an even share of what is unreceived near the end). Non-prefix claim ends and callback writes are limited to `max_ahead` past the contiguous prefix: 3 s of recent goodput with a 64 KiB floor. The initial claim is bounded to twice the minimum segment size, and slow links retain the prefix stream until another minimum range fits. Placement is near the prefix; the file is not preallocated. An idle lane takes the front of the frontier, or splits the claim with the most time left. If every lane is parked behind the prefix lane for a second, that lane is replaced.
- **Controller.** Two lanes to start, then double when two consecutive valid windows (1 s or 4 times the median time to first byte, only with every lane delivering and no lane idle for lack of work) each gain 15%; otherwise go back and never probe again. Halve on 429 or 503, on two windows of backpressure, or on a sustained 30% drop. The default cap stays at 4 lanes. Lanes take permits from `GlobalBudget` without waiting; each download can always get its first lane, and extra lanes are shared among downloads that are still growing.
- **Stalls and retries.** A lane with no bytes for 3 s (or under 10% of the median lane rate over 3 s while it holds the prefix) is dropped. Its unreceived bytes go back to the front of the frontier and are fetched on a fresh connection. A range that fails 3 times without substantial transport progress (64 KiB) ends the attempt; crawl and prefix-starvation replacements count only when they delivered no bytes. Before a range receives its first byte, the stall allowance also accounts for measured response latency. `Retry-After` is honored in a cancellable wait, including values above 60 s; clients do not yet show a separate waiting reason. Later lanes must return the same strong ETag, start and total; another one is an identity change. A 200 with the same ETag counts as a node that ignores ranges (after two, lanes drop to one).
- **Core.** Writes are positional; the prefix digest is extended by read-back; checkpoints every second and 64 KiB on a commit thread with an independently opened flush handle; a resume uses the scheduler, and a fully retained file goes straight to verification and publication. At most two transfer attempts are made. See [checkpoint recovery](CHECKPOINT-RECOVERY.md).
- **One production scheduler.** FP-087 removes the FP-015 round scheduler and its benchmark-only switch after the comparative gate. Historical paired evidence remains tied to its recorded commits; stream, plain identity restart and resume remain.
- The static libcurl build includes nghttp2. HTTPS prefers h2; controlled cleartext h2 uses prior knowledge. HTTP/3 is unavailable in the packaged build, so the policy records `http3_unavailable`.
- One receive buffer per lane, 16 KiB; `max_buffered_bytes` now budgets these buffers.

## Verification

Validation uses `cargo test -p fetchpath-http -p fetchpath-core -p fetchpath-storage --locked` and `node --test`. New tests cover: clipping (a buffer that straddles a split point, a claim end that shrinks between two callbacks, a late callback of a replaced claim); every first-response row; a later lane that sees another representation; a node that ignores ranges; a stalled lane replaced without a byte written twice; a server that never sends a body ending the attempt; a 429 and a 503 with `Retry-After`; cancellation at eight different moments; multiplexing and bounded independent-connection trials (h2c server); claim planning (time size, growth cap, no overlap under random claims and returns, `max_ahead`); controller rules; budget fairness; and the core tests listed in [checkpoint recovery](CHECKPOINT-RECOVERY.md).

The controlled protocol matrix produced matching 5 MiB hashes over both H1 and H2-prior-knowledge. H1 recorded preferred H2, negotiated HTTP/1.1, and explicit H3/lower-protocol fallback. H2 recorded negotiated `h2`; H3 was not attempted because the packaged capability is false. Raw observations are in [the protocol matrix](evidence/http-adaptive/fp015-protocol-matrix.json). The FP-015 pilot (five 8 MiB loopback pairs) was slower than curl; see [the raw paired observations](evidence/http-adaptive/fp015-loopback-pilot.json).

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

## Stream-first scheduler results (FP-085, 30 September 2026)

Fresh release builds, Windows/NTFS loopback, 64 MiB, five paired repetitions
with seeded client order. Both scheduler choices run the same binaries and
core; build sizes and modification times are recorded for both Fetchpath
clients. Median seconds to a verified file; intervals and all runs are in
[fp085-comparison.json](evidence/http-adaptive/fp085-comparison.json).

| profile | new transfer | rounds transfer | new engine | rounds engine | curl | aria2 x16 |
|---|---|---|---|---|---|---|
| unshaped | 0.25 | 0.28 | 0.51 | 0.61 | 0.26 | 0.31 |
| per-connection-limit | 2.68 | 4.25 | 2.98 | 4.55 | 8.09 | 0.71 |
| per-client-limit | 4.13 | 4.12 | 4.46 | 4.37 | 4.10 | 4.16 |
| delay | 1.26 | 1.48 | 1.61 | 1.76 | 2.17 | 0.44 |
| stall | 3.27 | 6.32 | 3.54 | 6.58 | 0.23 | 3.28 |
| redirect | 0.21 | 0.30 | 0.65 | 0.69 | 0.22 | 0.32 |
| weak-etag | 0.25 | fails | 0.47 | 0.53 | 0.25 | 0.32 |
| no-etag | 0.25 | fails | 0.48 | 0.52 | 0.22 | 0.34 |

Against paired rounds, the transfer layer improves 37% on the per-connection
limit, 48% on stalls and 15% on delay. The unshaped median improves 10%;
per-client limiting is unchanged. The engine improves 35%, 46% and 8% on
those shaped profiles, respectively. Its unshaped median is 16% lower; the
per-client median is 2% higher. No common-case median regresses above 5% in
this run. All 80 new-scheduler transfer/engine outputs match the fixture hash.
The ten failures are the old transfer-only rounds path on weak/no ETags.

A separate 1 MiB fixture with 100 ms response delay shows the stream-first
open at 0.19 s against rounds at 0.32 s (about 40% faster), with one request
instead of two; five paired repetitions, all hashes match
([small-file evidence](evidence/http-adaptive/fp085-small.json)).

Peak transfer memory in the 128 KiB experiment is about 6.5 MiB against 22 to 30 MiB for rounds. The
configured four-lane cap is held. aria2 x16 is still faster on the
per-connection limit and delay; on stalls its median is 3.28 s against 3.27 s
for the new transfer layer. Some stall runs encounter a second fixture pause,
so the new transfer interval extends to 6.26 s. Controller probing and
stealing are not isolated by these profiles.

Engine minus transfer medians range from about 0.26 to 0.44 s. This run
measures the combined scheduler/core repairs, including the independently
opened flush handle; it does not isolate that handle's performance. FP-088
covers the remaining engine reconcile/poll delay. A larger receive buffer has
not independently demonstrated a CPU gain: earlier 16/64/128/256 KiB trials
were within noise. The default is restored to 16 KiB; the unproven larger
buffer optimization is not retained. The table above records the earlier
128 KiB run; a targeted final-default comparison is recorded below. The
separate flush handle is a reviewed correctness repair, with no isolated
speed claim. An Internet rerun remains unmeasured.


Final-default checks retain the 16 KiB buffer. On 64 MiB/five paired repetitions,
transfer medians are 0.46 versus 0.47 s unshaped, 2.80 versus 4.42 s under the
per-connection limit (37% faster), and 4.34 versus 4.29 s under the per-client
limit (1% slower). Engine per-client medians are 4.62 versus 4.79 s. All 30
new-client hashes match; lane and buffer budgets hold
([final-default raw pairs](evidence/http-adaptive/fp085-default.json)).
The initial fast-engine sample was 1.08 versus 0.99 s (9% slower), with
overlapping intervals. One quiet targeted recheck after native checks completed
was 0.96 versus 1.00 s (4% faster), again with overlapping intervals
([quiet engine pairs](evidence/http-adaptive/fp085-engine-quiet.json)).
The deviation is not stable across samples. The existing 250 ms engine
reconcile/poll timing remains FP-088; no fast-engine speed claim rests on this
variation. A diagnostic sample that overlapped compilation is excluded from
performance decisions. No further reruns are needed for this gate.

A 64 MiB resumed transfer with the same 8 MiB retained prefix takes 2.71 s
adaptively against 7.43 s with one lane (63% faster). Five alternating pairs
verify all ten full-file hashes and enforce the requested lane ceiling
([resume pairs](evidence/http-adaptive/fp085-resume.json)). This isolates parallel
scheduling on resume with the same binary; the one-lane baseline is not a
historical core build. `node tools/bench/resume.mjs` reproduces it.

A real forced process stop of a scratch engine during a paced 256 MiB download
keeps a 90,921,813-byte checkpoint, resumes exactly at that offset and produces
the fixture hash. The conservative served-byte loss upper bound is 37,632,455
bytes (about 1.1 s at the origin's recent served rate), inside the four-second
envelope. This includes completed out-of-order extents and avoids graceful
commit draining ([interruption evidence](evidence/http-adaptive/fp085-interruption.json));
run it with `node tools/bench/interruption.mjs` as described in the benchmark
README. This is one controlled process-kill check, not OS-crash or power-loss
proof. Fault-injection coverage also snapshots the durable checkpoint before
unwinding and counts all staged bytes, rather than active lanes alone.

Independent Astra review examined the representation, callback clipping,
retry, cancellation, prefix and checkpoint boundaries. Its findings led to
hard run-ahead limits including callback enforcement after a slowdown,
progress-sensitive crawl replacement, measured first-byte allowances,
completed-claim removal, a separate flush handle, overlap refusal before
writing, cancellation cleanup despite commit failure, offline completion of
a fully retained prefix, and a shared two-attempt budget. Retry-After values
above 60 s are honored cancellably; extreme deadlines fail without overflow.

The independent Sol/medium bounded repair recheck accepted these fixes with
no blocking findings, reusing existing evidence. Its focused follow-up accepted
the 16 KiB default and resumed benchmark path. After those edits, HTTP tests,
strict HTTP clippy, formatting and repository/backlog checks pass. Optional
buffer tuning is removed; mandatory flush/cancellation/identity repairs have
no individually attributed speed claim. The initial strong review is reused.

## Redirect reuse (FP-086, 30 September 2026)

HTTP/2 begins with multiplexed streams in the shared curl connection cache.
After stable width and valid throughput windows, one bounded trial replaces a
lane with a fresh connection at the same budgeted width. Two windows must
each gain at least 15% to retain independent connections; otherwise the
scheduler restores sharing and stops pool probing. A shared origin cap
therefore does not trigger repeated connection experiments. The trial
requires a long enough ranged transfer and valid measurement windows; it is
not an immediate classification of an origin's bandwidth policy.

The production stream/resume scheduler follows at most ten HTTP(S) redirects
explicitly, discarding redirect bodies. Once the final response passes the
strong ETag and total-size checks, later ranges use that destination directly.
A 403, 404 or 410 invalidates the destination within the existing retry budget;
expiry from Cache-Control max-age or supported signed-URL timestamps also
causes re-resolution. The refreshed response passes the same representation
checks before any bytes are accepted. Only one lane resolves an invalidated
destination; already validated lanes can continue.

Cookies, URL credentials and referer survive only same-origin hops. A change
of scheme, host or port removes them for the rest of that chain, including a
return to the original origin; Location cannot introduce new URL credentials.
Same-origin redirects preserve the cookie engine. Redirect trailers cannot
replace the accepted Location or expiry. Pins remain in memory for this
transfer, and their URLs are absent from routine traces. This change covers
the production scheduler; link inspection retains its existing redirect
behavior. FP-087 removes the legacy benchmark scheduler.

Validation: HTTP/core/storage tests pass (180 tests; four real-fixture tests
remain ignored), strict HTTP clippy and workspace formatting pass. The H2
production tests exercise per-TCP and shared-origin bandwidth caps at the
same two-lane global budget, with complete, non-overlapping bytes. Separate
connections improve the former; the latter rolls back once and never
reprobes. The original multiplexing test confirms two streams share one
socket. Redirect tests cover expiry, 403/404/410 refresh with changed ETag
or size, cross-port and A-to-B-to-A secret stripping, same-origin cookies,
redirect trailers, cancellation and bounded rejection.

The final release benchmark uses 64 MiB and five paired repetitions on four
HTTP/1.1 fixture profiles; all 40 files verify. Median seconds to a verified
file: unshaped 0.38 versus curl 0.32; per-connection limit 2.85 versus 8.22;
shared client limit 4.26 versus 4.23; redirect 0.32 versus 0.37. Every
Fetchpath redirect run resolves exactly once. Intervals overlap for the
unshaped and redirect cases, so those do not establish a speed advantage.
Raw runs, build metadata and fixture hashes are in
[the fixture evidence](evidence/http-adaptive/fp086-fixture.json).

The FP-084 Hugging Face corpus file (440 MB), three final-build pairs on
one home connection, verifies in every run. The Fetchpath transfer median
is **192.71 s** [164.84–232.36], against curl **37.80 s** [37.37–57.02]:
about **5.1 times slower**, so the 10% target is **not met**. All Fetchpath
runs negotiate HTTP/1.1 and resolve exactly one redirect; pinning works,
but cannot provide HTTP/2 multiplexing on this negotiated path. They record
78, 131, 164 replacements, respectively, all triggered by prefix
starvation, which creates repeated fresh-connection/range churn. Removing
redirect hops has not removed that scheduler cost. The exact origin or
transport cause of the blocked prefix is not isolated by this run; no
Internet speed improvement or causal regression percentage is claimed.
The throughput gap is explicitly carried forward, rather than weakening
the target or attributing all time to redirects. Raw pairs and the bounded
trace counters are in [the Internet evidence](evidence/http-adaptive/fp086-internet.json).
Reproduce with the existing Internet corpus restricted to its Hugging Face
entry, the final release benchmark binary, three repetitions and
`FETCHPATH_HTTP_TRACE=1`. Trace-counter smoke validation matches the
fixture's independent redirect count; raw URLs/traces are not committed.

Implementation used GPT-6.1 Sol/medium. The initial independent GPT-6
Astra/low boundary review found one H2 throughput
regression; the bounded Sol/medium repair recheck accepted the socket trial
and rollback with no blockers. It reused the credential and representation
review. No changes to public APIs or persisted formats are required.

## Tool comparisons (FP-087, 30 September 2026)

The full Fetchpath engine is faster than single-connection curl, default
aria2 and wget2 on the per-connection-limit and delay fixtures. It is slower
on unshaped, redirect, weak/no-ETag and stall workloads. With a shared client
limit it has no gain: observed engine medians are 8–11% higher than those
single-connection tools. aria2 at 16 connections is faster on connection
limits and delay. No general browser or Internet speed advantage is claimed.

This is a **development build**, starting at `f6053ef` with this task's
round-scheduler retirement, not a measurement of the published 0.1.0
installer. Fresh release CLI and transfer-layer binaries use the same source.
Windows 11, NTFS, SSD, HTTP/1.1 loopback, 64 MiB, five repetitions per
profile/client, seeded randomized order (15015). All **280/280** outputs
match the fixture's SHA-256. Each time includes the independent hash pass;
Fetchpath's column is the full engine/CLI, including its own checkpoint,
digest and publication work. The transfer-only diagnostic remains in the raw
artifact and is not substituted for the product column.

Median **seconds to verified file / total TCP connections carrying requests**
(not peak concurrent lanes). Connections include redirect hops and retries.

| profile | Fetchpath engine | curl | aria2 default | aria2 ×16 | wget2 | Chrome cold† |
|---|---|---|---|---|---|---|
| unshaped | 0.69 / 3 | 0.34 / 1 | 0.43 / 1 | 0.37 / 16 | 0.39 / 1 | 1.41 / 1 |
| per-connection-limit | 3.24 / 5 | 8.20 / 1 | 8.27 / 1 | 0.79 / 16 | 8.30 / 1 | 8.94 / 1 |
| per-client-limit | 4.64 / 5 | 4.19 / 1 | 4.24 / 1 | 4.30 / 16 | 4.29 / 1 | 4.95 / 1 |
| delay | 1.70 / 3 | 2.26 / 1 | 2.37 / 1 | 0.52 / 17 | 2.33 / 1 | 2.93 / 1 |
| stall | 6.77 / 6 | 0.26 / 1 | 0.43 / 1 | 3.38 / 17 | 0.36 / 1 | 1.72 / 1 |
| redirect | 0.82 / 4 | 0.25 / 2 | 0.43 / 2 | 0.37 / 33 | 0.39 / 2 | 1.32 / 2 |
| weak-etag | 0.77 / 1 | 0.27 / 1 | 0.44 / 1 | 0.34 / 16 | 0.37 / 1 | 1.67 / 1 |
| no-etag | 0.73 / 1 | 0.28 / 1 | 0.43 / 1 | 0.37 / 16 | 0.39 / 1 | 1.71 / 1 |

Uncertainty is the seeded 2,000-resample bootstrap 95% interval in the
[raw comparison](evidence/http-adaptive/fp087-comparison.json); five runs do
not capture every source of variation. The comparative-claim gate requires
all five files verified, at least a 10% median advantage, and disjoint
intervals for a positive speed statement. Overlapping intervals are
inconclusive; similar observed medians do not prove equivalence. The
connection-limit engine/curl pair is 3.24 s [3.08–3.25] versus 8.20 s
[8.15–8.24], using 5 versus 1 total connections. Delay is 1.70 s [1.57–1.77]
versus 2.26 s [2.23–2.28], using 3 versus 1 connections. These two scoped
comparisons pass that gate. Shared-client limiting shows no acceleration.

† Chrome's native download manager runs in an isolated headless instance
with a fresh profile/cache and disabled extensions/proxy. The table includes
**cold startup and shutdown**, whereas the Fetchpath engine is warmed by an
unmeasured download. Those columns do not establish an advantage over an
already running browser. The following diagnostic is the median of each
Chrome run's `nativeDownloadMs + verifyMs`, excluding startup/shutdown;
brackets are **observed min–max**, not a confidence interval or a separately
measured warm-browser test. The raw record retains both components.

| profile | Chrome native download phase + hash, seconds [range] |
|---|---|
| unshaped | 0.62 [0.57–0.69] |
| per-connection-limit | 8.27 [8.21–8.35] |
| per-client-limit | 4.25 [4.22–4.32] |
| delay | 2.32 [2.27–2.32] |
| stall | 0.60 [0.52–0.66] |
| redirect | 0.62 [0.51–0.69] |
| weak-etag | 0.63 [0.60–0.64] |
| no-etag | 0.62 [0.56–0.69] |

The stall fixture pauses every fourth sufficiently large range; curl,
default aria2, wget2 and Chrome use a single non-range request and encounter
no pause. These rows are different request workloads, not proof of weaker
recovery against an equally stalled single stream. Fetchpath can hit a
second pause; its engine interval spans 3.74–6.79 s. aria2 ×16 requests more
ranges and has a different pause sequence. Claims about faster stall
recovery are withheld. The engine minus transfer-layer median ranges from
0.34 to 0.50 s across this corpus; FP-088 addresses the remaining scheduling
and completion-notification delay.

Versions/settings: Fetchpath reports 0.1.0, with binary sizes/mtime and source
baseline in the raw record; default 2–4 lanes and 16 KiB receive buffers.
curl 8.21.0 uses one HTTP/1.1 connection, follows redirects and ignores user
configuration/proxies. aria2 1.37.0 uses defaults or explicit
`-x16 -s16 -k1M`, without user configuration/proxies. wget2 2.2.1 uses
`--no-config --no-proxy` with its HSTS/OCSP files in scratch; this is the
[upstream Windows asset](https://github.com/rockdaboot/wget2/releases/tag/v2.2.1),
matched to the release asset's SHA-256
`c65f66842888b878d1fad48294d1372553985b86613f279cf4ed9612c4f80428`.
The newer 2.3.0 source release has no Windows executable asset in that
upstream release; no 2.3.0 Windows result is implied. Chrome is
154.0.8037.93. No licensed IDM copy was available to this harness, so no IDM
row exists. CLI engines use both scratch data-directory overrides; personal
cache, peer policy and browser profiles are not read. CPU/memory do not cover
the CLI engine process tree; Chrome CPU/memory is unmeasured. No resource
efficiency claim is made.

Reproduce after the release build with:

`node tools/bench/benchmark.mjs --output-dir work/bench-comparison --size 64m --repetitions 5 --clients fetchpath-http,fetchpath-cli,curl,aria2,aria2-x16,wget2,chrome --wget2 PATH --chrome PATH --timeout-s 120`

This reruns every FP-084 fixture profile on the final build. The Internet
Hugging Face evidence above remains a **prior transfer-layer observation**
(192.71 s versus 37.80 s), not a new full-engine or cross-tool Internet run.
Its prefix-starvation churn and unmet target remain disclosed. Application
shaping on one localhost machine does not establish Internet performance,
RTT/loss behavior or results on slower storage. The old rounds-only code and
public Rust symbols are removed, with their last containing commit in
[the archive](ARCHIVE.md); stream, plain identity restart and resume tests
remain passing. No wire or persisted-format changes.

Validation: 97 HTTP tests and 76 core tests passed; three HTTP protocol
checks and one core external-mirror check remain ignored. Strict HTTP clippy,
workspace formatting, 12 fixture tests and 12 repository/task tests passed.
Independent GPT-6 Astra/low review recomputed all 56 time/connection groups,
all Chrome diagnostic ranges and the six positive comparison gates from
raw runs; no blockers. Static review also accepted scratch isolation and
round-scheduler retirement.

## Limits carried forward

No speed advantage over curl is claimed on one unthrottled connection: loopback shows parity within noise, and the engine adds a fixed cost that FP-085 did not remove. Loopback does not model RTT, loss, congestion, blocked UDP, slow storage or competing traffic. aria2 with 16 connections is faster than four lanes wherever a per-connection limit or a delay dominates. The default cap of 4 lanes is unchanged, and doubling to 8 needs more 1 s windows than a 64 MiB file lasts. FP-086 pins validated redirect destinations and begins with shared HTTP/2 streams; short transfers may finish before its bounded separate-connection trial. With timely checkpoint completion, an interruption loses about 4 s of recent transfer, or 64 KiB, whichever is larger; storage stalls can extend checkpoint lag. A packaged HTTP/3 backend, controlled H3 endpoint, and blocked-UDP fallback run remain required before any H3 support claim.
