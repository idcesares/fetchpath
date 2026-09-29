# Stream-first, work-stealing HTTP scheduler

FP-083, 29 September 2026. Status: **accepted by independent strong review**
(29 September 2026, re-checked after revision). FP-085 starts after the
owner accepts the loss bound in 7.1. Implements
in FP-085; connection sharing and redirect pinning in FP-086; measured by
FP-084.

## 1. Problem

One run each on a home connection, 28 September 2026
([adaptive HTTP](../../development/ADAPTIVE-HTTP.md)): a 115 MB GitHub asset
took 3.5 s through the engine and 2.5 s with curl; a 440 MB Hugging Face file
took 26.2 s and 7.1 s. The transfer-layer figures in that record (2.4 s,
18.8 s) used fixed 8 MiB ranges and the engine ranges growing from 1 MiB, so
they do not measure engine overhead. FP-084 must compare both with identical
limits before any overhead is attributed.

Causes found in the code:

| Cause | Where | Effect |
| --- | --- | --- |
| Lock-step rounds: each waits for its slowest range | `crates/fetchpath-http/src/lib.rs:594-676` | Lanes idle behind stragglers |
| Additive ramp: one lane, +1 after two healthy rounds, cap 4 | `lib.rs:450-485` | About 9 s to full width |
| A 1-byte probe before data | `lib.rs:774` | One extra round trip per download |
| Ranges held in memory, written in order; 8 MiB cap exists only because of that buffer | `lib.rs:653-662, 883, 128` | No write overlap; short claims on fast links |
| One easy handle per lane, each following the redirect | `lib.rs:591, 704` | A redirect per range on Hugging Face (FP-086) |
| 16 KiB curl receive buffer (curl's CLI uses about 100 KiB) | `lib.rs:708` | More callbacks per byte |
| Each checkpoint commit scans the parent directory twice | `crates/fetchpath-storage/src/lib.rs:148-174, 201, 243` | Cost grows with the folder |
| Final digest re-reads the whole file | `crates/fetchpath-core/src/transfer.rs:189` | One full read before publication (kept; measured) |
| A resume always runs sequentially | `transfer.rs:151-160` | A resumed large file never uses ranges |
| Any range error discards everything | `lib.rs:644-647`, `transfer.rs:142-148` | A transient error restarts from byte 0 |

## 2. Goals and non-goals

Goals, in order:

1. Match curl's median on an unthrottled single-origin download. On Hugging
   Face this also needs FP-086's redirect pinning.
2. Where an origin limits each connection or delay dominates, finish as fast
   as aria2 with `-x16` while opening fewer connections.
3. Add no connections where they bring no goodput (a per-client limit), and
   share the global budget fairly across downloads.
4. Keep the correctness contract. Bound what a pause or crash discards to a
   few seconds of transfer (4.5).

Non-goals: HTTP/3, multipath, FEC, CDC, a Xet client
([efficiency research](../../development/EFFICIENCY-RESEARCH.md)); HTTP/2
multiplexing and redirect pinning (FP-086); mirror scheduling; parallel
ranges without a strong ETag (deferred, 4.9); any persisted-format change.

## 3. Invariants that do not move

- Bytes from two representations are never combined. Ranges after the first
  carry `If-Range` with the strong ETag of the first response. Every 206 must
  return that strong ETag, the requested start, and the same total, checked
  before its first byte is written. On a resume the total must also equal the
  checkpoint's `expected_total` (a check `resume_headers_match` in
  `crates/fetchpath-core/src/checkpoint.rs:76-86` does not make today).
- A changed ETag or total on any lane means the identity changed: stop all
  lanes and restart once. A 200 without an ETag on a later lane is treated
  the same way. A 200 carrying the *same* strong ETag means that node ignores
  ranges: stop that lane and count it against the range's retry budget
  (4.4); after two such stops in a download, finish sequentially from the
  prefix.
- The checkpoint records a contiguous prefix `[0, committed_len)` with its
  SHA-256 and strong ETag. Format and meaning unchanged.
- The final digest is computed from the staging file on disk. A mismatch
  against an expected checksum deletes the bytes and fails with
  `ChecksumMismatch`, as today (`transfer.rs:190-198`).
- Redirect and credential handling in `configure` are unchanged (FP-086).
- `GlobalBudget` and a rule's `max_connections` are upper bounds.

## 4. Design

### 4.1 Stream-first open and first-response rules

The first request asks for `Range: bytes=O-`, where `O` is 0 or the resume
offset, with `If-Range` on a resume. There is no probe. The response is
classified in its first write callback, and again on completion, because a
response with an empty body never calls the write callback. libcurl calls the
write callback only for the final response after redirects. The header state
is frozen at the first write so trailers cannot change the ETag used.

| Response | Action |
| --- | --- |
| 200 at `O = 0` | Stream sequentially to the end, as today |
| 200 at `O > 0` | Identity restart: reissue from 0 after `store.reset()`; the stream is not reused |
| 206, strong ETag, `start = O`, total known | Accept. If the remainder is under `min_adaptive_bytes`, this request finishes it. Otherwise lane 0 owns `[O, O+S0)` and the rest enters the frontier |
| 206 whose end is below `total - 1` (the server capped it) | Strong ETag: accept what it covers and return the remainder to the frontier. No strong ETag: fall back to a plain GET |
| 206 with `Content-Range: .../*` | Sequential on this request |
| 206 at `O = 0` without a strong ETag | Sequential, provided `start = 0`, `end = total - 1` and the byte count matches; otherwise fail |
| 416 at `O = 0` | Retry once with a plain GET (an empty resource) |
| 416 at `O > 0` | Identity restart |

`S0` is `max(min_segment_bytes, total / 16)`, bounded by the claim-size rule
in 4.2. When lane 0 reaches `S0` and the next bytes are unclaimed, it extends
its claim and keeps streaming.

### 4.2 Frontier, claims and stealing

The unclaimed bytes form a frontier, an ordered set of extents. A lane that
finishes a claim takes the next extent at the front immediately.

- **Claim size is set by time, not a byte cap:** `rate_lane × T` with
  `T = 1.5 s` and a `min_segment_bytes` floor. The 8 MiB `segment_bytes` cap
  goes, because positional writes remove the buffering it protected. Near the
  end, a claim is at most `remaining / active_lanes`, so lanes finish together.
- **Claims may run ahead of the prefix only by `max_ahead`**, which is
  `aggregate_goodput × 3 s`, with a floor of `lanes × min_segment_bytes`.
  This bounds what a pause or crash discards (4.5).
- **Stealing:** an idle lane with an empty frontier takes part of the
  in-flight claim with the most time left. That claim must have more than
  `2T` left at its owner's rate and at least `2 × min_segment_bytes`
  remaining. The split point allows for the stealer's time to first byte:
  `owner_pos + (remaining − owner_rate × ttfb_estimate) / 2`.
- **Prefix starvation:** when claiming is blocked by `max_ahead`, the lane
  holding the prefix may be replaced (4.4) whatever the split minimum. This
  way a slow but not stalled prefix lane cannot park every other lane.

### 4.3 Clipping at the claim boundary

This is the rule that makes splits and replacements safe. A handle cannot be
removed from inside its own callback (`CURLM_RECURSIVE_API_CALL`), so an
owner keeps delivering after its claim shrinks.

- Every write callback reads its claim's current end `E` and position `p`,
  and writes only `data[..min(len, E - p)]`. It loops until a short
  positional write is complete.
- If anything was clipped, the callback returns a short count. The scheduler
  treats the resulting `CURLE_WRITE_ERROR` on that handle as a deliberate
  stop, not a transport error, and removes the handle after `perform`
  returns.
- A generation number per claim rejects any write from a claim already
  replaced. It is a second guard, not the main one.
- Tests: a buffer that straddles a split point; a claim end that shrinks
  between two callbacks; a replaced claim whose late callback writes nothing.

### 4.4 Stalls and retries

- A lane is stalled when it delivers no bytes for 3 s, or less than 10% of
  the median lane rate over 3 s while it holds the prefix. It is stopped
  (the loop removes its handle between `perform` calls, since a stalled
  handle makes no callbacks to clip) and its unreceived bytes return to the front of
  the frontier, to be claimed with `CURLOPT_FRESH_CONNECT`. No byte is
  fetched twice.
- A range's retry budget is 3. A replacement that made no progress counts
  against it. A server that accepts connections and never sends a body
  therefore ends the attempt instead of cycling.
- An attempt fails after any range exhausts its budget. A download makes at
  most 2 attempts and at most 1 identity restart. An attempt that fails
  without an identity change resumes from its committed prefix with the
  scheduler.
- A 429 or 503 halves the lanes. `Retry-After` is honored up to 60 s;
  longer, the job waits and says so.

### 4.5 Write path, prefix, checkpoints and loss

- Writes are positional (`FileExt::seek_write` on Windows), looped until
  complete. `seek_write` moves the cursor on Windows, so every write on a
  shared handle is positional.
- A per-attempt completed-extent set starts at `[0, committed_len)`.
  Completion is never inferred from the file's length: after an in-process
  retry, stale bytes of the same identity may sit past the prefix.
- The **prefix** is the first byte not yet written. When it advances over
  bytes that arrived out of order, those bytes are read back from staging to
  extend the running SHA-256. Bytes written at the prefix are hashed from the
  buffer. An advance reaches any hash or commit thread only after its write
  call has returned.
- **Checkpoints:** commit the prefix when at least 1 s has passed and it has
  advanced at least 64 KiB, and at the end.
- **Loss on pause or crash:** pause is a cancel and join
  (`crates/fetchpath-session/src/lib.rs:1518-1550`), and recovery truncates
  staging to `committed_len` (`validate_staging`). So both discard bytes past
  the prefix. With `max_ahead` of about 3 s of goodput and a 1 s cadence, a
  pause or crash discards about 4 s of transfer or 64 KiB, whichever is
  larger. Today it discards at most one round held in memory (up to 4
  ranges, 32 MiB). [Checkpoint recovery](../../development/CHECKPOINT-RECOVERY.md)
  and the job contract's progress text state this bound, and a test checks it.
- **Cancel ordering:** stop lanes, `sync_payload`, then `commit` the prefix.
  All of it, including any commit thread, finishes before
  `download_with_faults` returns. `RemoveStaging` therefore always runs
  afterwards, as the contract's quiescence rule requires.
- **Writes past the end of file:** NTFS zero-fills up to a write beyond the
  valid data length, so out-of-order lanes can write most bytes twice. FP-084
  measures it. FP-085 then picks one of: `set_len` to the total at accept,
  a sparse file, or claim placement near the prefix.
- `CheckpointStore` keeps the last generation it wrote, removing the two
  directory scans per commit. Fsync and commit run on a commit thread from
  the start, so `FlushFileBuffers` never stalls the socket loop.

### 4.6 Concurrency controller

- Start at 2 lanes once the first 206 is accepted (1 if a rule or the size
  allows only one).
- **Measurement:** a window is `max(1 s, 4 × median TTFB)`. It starts only
  after every new lane has delivered bytes. Windows where any lane sat idle
  for lack of work (tail, split gaps) are ignored.
- **Growth:** double (2, 4, 8, up to the budget) when two consecutive valid
  windows at the new width show at least 15% more goodput than the old
  width. Otherwise return to the old width and stop probing for this
  download. There is no re-probe.
- **Shrink:** halve on a 429 or 503, on backpressure, or on a 30% goodput
  drop sustained over two valid windows.
- **Backpressure** is time spent in write callbacks plus waits on the commit
  thread, above 25% of a window. The loop then stops claiming new work; if
  it persists for a second window, it halves. `CURL_WRITEFUNC_PAUSE` is
  available if pausing handles measures better.
- **Budget:** lanes take permits with a non-blocking `try_reserve` on
  `GlobalBudget`, so the event loop never waits on a condvar. With several
  downloads, each gets at least one lane, and extra lanes are split evenly
  among downloads that are still growing. Downloads beyond the budget's
  request limit wait in the queue. "Buffered bytes" now means one receive
  buffer per lane.

### 4.7 One event loop, separate connections

All lanes of a download are easy handles in one curl multi handle, driven by
one thread per download, with a commit thread beside it (4.5). The receive
buffer rises from 16 KiB to what FP-084 shows best, probably 64–256 KiB.

A multi handle multiplexes HTTP/2 by default, and HTTPS already prefers h2.
That would put every lane on one connection with one congestion window, and
lose on an origin that limits each connection. **FP-085 sets
`CURLMOPT_PIPELINING` to `CURLPIPE_NOTHING`,** so each lane keeps its own
connection. FP-086 decides when multiplexing wins, per origin. A test
asserts that multiplexing is off.

### 4.8 Observability

`SegmentMonitor` becomes continuous: segments appear and disappear as claims
start and end. `TransferReport` gains per-window lanes and goodput, splits,
replacements, retries and connections opened. `observations` keeps its name
and now describes windows, not rounds.

### 4.9 Deferred: parallel ranges without a strong ETag

Allowing ranges when a trusted expected SHA-256 exists but no strong ETag is
cut from FP-085. If it is ever pursued, it needs all of the following:
- On a mismatch, discard and make one single-connection attempt before
  reporting `ChecksumMismatch`, since the fault may be the engine's own
  mixing of bytes.
- No checkpoints or resume, because `recover_download` requires a strong
  ETag. A crash then loses the whole file.
- `If-Range` with a date where `Last-Modified` qualifies as strong.
- Parsing `Last-Modified`.

Revisit it together with Metalink piece hashes.

### 4.10 Switch-over

FP-085 keeps the round scheduler behind an internal switch until FP-084's
paired runs pass. FP-087 deletes it and lists it in
`docs/development/ARCHIVE.md`.

## 5. What each mechanism must show

The plan's gate: at least 10% median gain where the mechanism is aimed, no
unexplained common-case regression above 5%, and budgets held.

| Mechanism | Must win on | Must not regress |
| --- | --- | --- |
| Stream-first open | Small files, `delay` | Everything |
| Continuous lanes, time-sized claims, stealing | `per-connection-limit`, `delay`, GitHub asset | `per-client-limit` |
| Doubling controller | Time to full width on `per-connection-limit` | `per-client-limit` stays at 2; connections opened |
| Stall replacement, retry budget | `stall` | `per-connection-limit` |
| Positional writes, commit thread, 1 s cadence | Engine versus transfer layer at equal limits | Crash and pause loss bound |
| Scheduler on resume | A resumed large file | Resume fault tests |
| Larger receive buffer | CPU-seconds per GiB | None |
| Redirect pinning (FP-086) | `redirect`, Hugging Face file | Credential tests |

## 6. Tests FP-085 must add

- Clipping (4.3): a buffer that straddles a split point; a shrinking claim
  end; late callbacks.
- Frontier: no overlap; each byte written once; `max_ahead` held; prefix-lane
  replacement when claiming is blocked.
- First response (4.1): every table row, including a capped 206, a
  `.../*` total, a 416 on an empty resource, and a 206 at 0 without a strong
  ETag.
- Resume: a total mismatch against `expected_total` restarts; a 200 on resume
  resets before any write; a resume uses ranges.
- Loss bound: pause and resume, and a kill, each discard no more than the
  stated bound; the cadence holds on a slow link.
- Ordering: the cancel commit finishes before `RemoveStaging`.
- Retries: a server that never sends a body ends the attempt; a 429 halves
  the lanes.
- Configuration: HTTP/2 multiplexing is off.
- Existing identity, If-Range, truncation, cancellation and fault-injection
  tests pass unchanged.

## 7. Open questions

1. The loss bound changes from one round in memory to about 4 s of transfer
   (or 64 KiB, whichever is larger). The
   owner accepts this before FP-085 is marked done. Power-loss durability
   stays deferred (FP-077).
2. NTFS zero-fill (4.5): FP-084 measures which placement avoids double
   writes.
3. One loop per download: a shared multi handle across downloads is left for
   FP-086 if measurements ask for it.
