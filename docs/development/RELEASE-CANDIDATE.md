# First public release candidate — acceptance matrix and decision

FP-018. Assembled 22 September 2026 on Windows 11 Pro build 10.0.26200.0, x64,
with the Microsoft-serviced Edge WebView2 Evergreen runtime present.

This document exists to answer one question honestly: **is Fetchpath 0.1.0 ready
to publish?** The answer below is *not yet*, and the reasons are specific. Every
row of the matrix says what was actually run, and every gap says what would
close it.

## Verdict

**Do not publish yet.** Four gates are open, and none of them can be closed from
a single developer machine with no network access to third-party sites:

| Gate | Why it is open | What closes it |
| --- | --- | --- |
| Supported public-source corpus (A11) | Media is proven against lawful local HLS/DASH fixtures only. No public site has been selected, tested, or committed to. | A product decision on which sites the release supports, then a recorded run against each. |
| Media helper checksums (A11) | `media-tools.json` ships unpinned by design, so the guided download refuses. | `node tools/media-tools/pin.mjs` on a machine with network access. |
| Clean-machine packaging (A12) | Install, upgrade and uninstall are exercised only on a developer machine that already has the WebView2 runtime, the Visual C++ runtime and a warm disk cache. | One run on a freshly imaged Windows 11 x64 machine. |
| ARM64 (A12) | Deferred by decision. No native, helper or installer path has been built or tested for it. | Either ARM64 hardware and a full path, or an explicit statement that 0.1.0 is x64 only. |

Two further things are true of this candidate and are **decisions, not gaps**:
builds are unsigned, with the SmartScreen consequence documented; and no speed
or acceleration claim is made anywhere in the product or its documentation.

## Acceptance matrix

Status is one of **Met** (evidence exists and was re-run for this candidate),
**Partial** (real evidence exists but does not cover the criterion as written),
or **Open**.

| ID | Required behavior | Status | Evidence |
| --- | --- | --- | --- |
| A01 | Core journeys work for a nontechnical user; advanced controls remain optional | Met | `ui-accessibility.ps1` against the optimized executable: 19 tab stops in reading order from the skip link, 26 focusable controls with none unnamed, a keyboard-only journey that published 262144 bytes and was announced as "keyboard-only.bin finished downloading", a welcome card dismissed with Enter that did not return after a restart, Advanced options opened from the keyboard, and a modal Settings dialog with 11 named controls that takes and returns focus. Advanced options, Settings and Power mode are all off the primary path and closed by default. [Windows packaging](WINDOWS-PACKAGING.md), [release polish](RELEASE-POLISH.md) |
| A02 | Completed fixtures match expected bytes; known-bad content is never published as verified | Met | 14 of 14 benchmark outputs matched the fixture SHA-256; Metalink piece-hash repair publishes only through the create-only fence. [benchmark uncertainty](evidence/release/fp018-benchmark-uncertainty.json), [Metalink repair](METALINK-REPAIR.md) |
| A03 | Interruptions never authorize unsafe concatenation or false completion | Met | Source-change, range-refusal, truncated-response and cancellation-race tests in `fetchpath-core`. FP-027 added the pause/publication race: a pause refused because publication had already committed reports the completion rather than a false paused state. [release polish](RELEASE-POLISH.md) |
| A04 | Checkpoints and publication recover consistently within the documented durability envelope | Partial | Kill-point tests around writes, flushes, metadata commits and publication pass, and a paused download now resumes from its checkpoint across a process restart with a single `If-Range`. **Separate OS-level and power-loss tests have not been run.** [checkpoint recovery](CHECKPOINT-RECOVERY.md), [release polish](RELEASE-POLISH.md) |
| A05 | Memory, queued bytes, sockets, workers, and retries obey configured limits | Met | The adaptive path stayed inside its configured budget in every benchmark run (peak 2 of 8 requests, peak 2 MiB of an 8 MiB buffer). Concurrency, retry count and backoff are clamped in one place and tested at their bounds. [benchmark uncertainty](evidence/release/fp018-benchmark-uncertainty.json), [release polish](RELEASE-POLISH.md) |
| A06 | Protocol negotiation and fallback match actual packaged capabilities | Partial | The protocol matrix records `http/1.1` on the HTTP/1.1 endpoint and `h2` on the prior-knowledge endpoint, both hash-verified, with `preferred: h2`. HTTP/3 is **not attempted** because the packaged libcurl reports it unavailable, and `http3_unavailable` is recorded rather than a false attempt. **No real HTTP/3 endpoint and no blocked-UDP case has been exercised.** [protocol compatibility](PROTOCOL-COMPATIBILITY.md), [adaptive HTTP](ADAPTIVE-HTTP.md) |
| A07 | Credentials, filenames, and local control commands stay within their intended boundaries | Met | Redirect, log-redaction, path-escape, malformed-IPC and helper-input tests pass. The filename fence rejects the NTFS alternate-data-stream colon. FP-030 added a second fence on the reveal-in-Explorer path, which refuses a destination containing a quote and writes Explorer's command line with `raw_arg` rather than letting argv quoting mangle it. [Windows packaging](WINDOWS-PACKAGING.md) |
| A08 | Browser capture either hands off reliably or preserves the browser path | Met | Authenticated, expiring, POST and blob cases plus duplicate and cancel races. [browser capture](BROWSER-CAPTURE.md) |
| A09 | Acceleration claims are reproducible and include total usable-file time | Met, and the claim is *none* | Seven paired runs with their spread and 95% intervals. Fetchpath is **slower** than the curl baseline on loopback: 55.9 ms mean against 35.5 ms excluding the first pair, sd 16.3 against 2.1. No acceleration claim is made anywhere, so there is none to substantiate. [benchmark uncertainty](evidence/release/fp018-benchmark-uncertainty.json) |
| A10 | Optional sharing is explicit and obeys authorization and upload/cache budgets | Not in scope | Sharing is M8 (FP-020) and no sharing feature ships in 0.1.0. FP-018 does not list A10. |
| A11 | Compatibility packs deliver correct outputs and bounded failure handling | **Open** | Protocol fixtures, media assembly checks and helper crash/update tests pass against **local fixtures**. The supported public-source corpus has not been selected or tested, and the media helper checksums are unpinned. [media integration](MEDIA-INTEGRATION.md), [release polish](RELEASE-POLISH.md) |
| A12 | A supported Windows installation can install, update, recover, and uninstall predictably | **Open** | Per-user install, upgrade over a running instance with a byte-identical retained queue, and uninstall are all exercised on this machine, and the uninstaller now has a real data-removal branch compiled into the shipped installer. **Clean-machine and ARM64 coverage remain untested.** [Windows packaging](WINDOWS-PACKAGING.md), [release polish](RELEASE-POLISH.md) |

## Checks re-run for this candidate

```
cargo test --workspace --locked                        120 passed, 0 failed
cargo clippy --workspace --all-targets -- -D warnings  clean
cargo fmt --check                                      clean
node --test                                            20 passed, 0 failed
node tools/tasks.mjs check                             30 tasks valid
node tools/licenses/generate.mjs --check               566 packages, current
corepack pnpm --dir apps/desktop build                 clean
corepack pnpm --dir apps/desktop tauri build           Fetchpath_0.1.0_x64-setup.exe
node tools/bench/benchmark.mjs                         7 pairs, 14/14 hashes matched
node tools/bench/protocol-matrix.mjs                   h1 and h2 verified, h3 unavailable
pwsh tests/compatibility/windows/ui-accessibility.ps1  see evidence
```

## Known limitations of 0.1.0

These are things a user would notice. They are limitations, not defects, and
each is a deliberate position rather than an oversight.

1. **Video and audio need two programs Fetchpath does not ship.** Settings
   detects them, accepts a folder, and can download a pinned copy — but this
   build ships with no recorded checksums, so the guided download refuses and
   the manual path is the only one offered. Recording a digest from whatever
   arrived would prove the bytes survived the network and nothing about who
   published them.
2. **No public media site is supported yet.** Media works against lawful local
   HLS/DASH fixtures. Nothing else has been tested or promised.
3. **Pause is for file downloads only.** A media download has no checkpoint to
   return to, so pause is not offered on those rows rather than offered and
   then refused.
4. **Unsigned.** SmartScreen warns on first run. The Authenticode hook is wired
   and inert; setting `FETCHPATH_SIGN_THUMBPRINT` signs every binary in the
   bundle with no other change.
5. **x64 only.** ARM64 is deferred until its complete native, helper and
   installer path is built and tested.
6. **No HTTP/3.** The packaged libcurl reports it unavailable, and Fetchpath
   records that rather than pretending to attempt it.
7. **No speed claim.** The statistics panel reports throughput observed across
   active downloads, which is not a measure of the user's connection, and the
   panel says so. The one paired measurement that exists shows Fetchpath slower
   than curl on loopback.
8. **Offline is not a modelled state.** A connection failure appears as a
   transport failure with a retry; automatic retry is the setting that governs
   how it is handled.
9. **Power-loss durability is argued, not demonstrated.** Kill-point tests cover
   the process; no separate OS-level or power-interruption test has been run.

## Recommended path to publication

In order, because each depends on the one before:

1. Decide the supported site corpus. This is a product decision, not an
   engineering one, and everything in A11 waits on it.
2. Run `node tools/media-tools/pin.mjs` to record the helper checksums, then
   re-run the media setup path end to end with the guided download enabled.
3. Test each corpus entry and record the results.
4. Run the packaging lifecycle on a freshly imaged Windows 11 x64 machine.
5. State explicitly that 0.1.0 is x64 only, or build and test the ARM64 path.
6. Decide on signing. Shipping unsigned is a supportable choice as long as the
   SmartScreen consequence and the SHA-256 verification instructions are in
   front of the user before they download.

Steps 1, 4 and 5 need resources this machine does not have. Steps 2 and 3 need
network access to third-party sites. None of them are blocked on code.
