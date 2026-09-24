# First public release candidate — acceptance matrix and decision

FP-018. Assembled 22 September 2026 on Windows 11 Pro build 10.0.26200.0, x64,
with the Microsoft-serviced Edge WebView2 Evergreen runtime present.

This document exists to answer one question honestly: **is Fetchpath 0.1.0 ready
to publish?** The answer below is *not yet*, and the reasons are specific. Every
row of the matrix says what was actually run, and every gap says what would
close it.

## Verdict

**Ready to publish, on the user's word (24 September 2026).** Every gate below
is closed and the candidate checks were re-run on the final tree; see
[the 24 September re-run](#re-run-on-24-september-2026). Publication is a
GitHub release, made only when the user says so.

The record of 23 September follows. **Then: not yet, and close.** On 22 September four gates were open. On 23 September
three were closed and the product was made usable without a developer:

| Gate | State on 23 September 2026 |
| --- | --- |
| Supported public-source corpus (A11) | **Closed by decision.** 0.1.0 names no supported site; video and audio are documented as best-effort, depending on yt-dlp. |
| Media helper checksums (A11) | **Closed.** yt-dlp 2026.08.19 and ffmpeg 9.0.2 are pinned against their publishers' checksum files, and the real guided install passes end to end after a defect in it was fixed. |
| ARM64 (A12) | **Closed by statement.** README, user guide and changelog say 0.1.0 is x64 only. |
| Clean-machine packaging (A12) | **Closed on 23 September** by a scripted Windows Sandbox run (FP-043), which the user accepted as the clean machine. |

FP-020's independent review of `adapters/lan` is done: the cryptography and
FFI are sound, and two defects it found (unpairing did not revoke a running
server; a race in identity creation) are fixed. The paired-device commands are
now listed in the CLI help and documented in the [CLI guide](../user/CLI.md).
Merging this branch to `main` is the lead's integration step.

Builds stay unsigned by decision, with the SmartScreen consequence and the
installer checksum check in front of the user in the README and user guide. No
speed or acceleration claim is made anywhere.

## Acceptance matrix

Status is one of **Met** (evidence exists and was re-run for this candidate),
**Partial** (real evidence exists but does not cover the criterion as written),
or **Open**.

| ID | Required behavior | Status | Evidence |
| --- | --- | --- | --- |
| A01 | Core journeys work for a nontechnical user; advanced controls remain optional | Met | Re-run on 23 September against the queue-first layout (FP-035): 15 tab stops in reading order across the page and the Add download dialog, 17 focusable controls with none unnamed, a keyboard-only journey that opened Add download with Enter and published 262144 bytes announced as "keyboard-only.bin finished downloading", Escape keeping the draft and Cancel clearing it, and live regions present inside the modal dialog. A first-time user is taken through install, SmartScreen, a first download, the browser extension and media setup by the [user guide](../user/GUIDE.md). [release readiness](RELEASE-READINESS.md), [evidence](evidence/windows/ui-accessibility.json) |
| A02 | Completed fixtures match expected bytes; known-bad content is never published as verified | Met | 14 of 14 benchmark outputs matched the fixture SHA-256; Metalink piece-hash repair publishes only through the create-only fence. [benchmark uncertainty](evidence/release/fp018-benchmark-uncertainty.json), [Metalink repair](METALINK-REPAIR.md) |
| A03 | Interruptions never authorize unsafe concatenation or false completion | Met | Source-change, range-refusal, truncated-response and cancellation-race tests in `fetchpath-core`. FP-027 added the pause/publication race: a pause refused because publication had already committed reports the completion rather than a false paused state. [release polish](RELEASE-POLISH.md) |
| A04 | Checkpoints and publication recover consistently within the documented durability envelope | Partial | Kill-point tests around writes, flushes, metadata commits and publication pass, and a paused download now resumes from its checkpoint across a process restart with a single `If-Range`. **Separate OS-level and power-loss tests have not been run.** [checkpoint recovery](CHECKPOINT-RECOVERY.md), [release polish](RELEASE-POLISH.md) |
| A05 | Memory, queued bytes, sockets, workers, and retries obey configured limits | Met | The adaptive path stayed inside its configured budget in every benchmark run (peak 2 of 8 requests, peak 2 MiB of an 8 MiB buffer). Concurrency, retry count and backoff are clamped in one place and tested at their bounds. [benchmark uncertainty](evidence/release/fp018-benchmark-uncertainty.json), [release polish](RELEASE-POLISH.md) |
| A06 | Protocol negotiation and fallback match actual packaged capabilities | Partial | The protocol matrix records `http/1.1` on the HTTP/1.1 endpoint and `h2` on the prior-knowledge endpoint, both hash-verified, with `preferred: h2`. HTTP/3 is **not attempted** because the packaged libcurl reports it unavailable, and `http3_unavailable` is recorded rather than a false attempt. **No real HTTP/3 endpoint and no blocked-UDP case has been exercised.** [protocol compatibility](PROTOCOL-COMPATIBILITY.md), [adaptive HTTP](ADAPTIVE-HTTP.md) |
| A07 | Credentials, filenames, and local control commands stay within their intended boundaries | Met | Redirect, log-redaction, path-escape, malformed-IPC and helper-input tests pass. The filename fence rejects the NTFS alternate-data-stream colon. FP-030 added a second fence on the reveal-in-Explorer path, which refuses a destination containing a quote and writes Explorer's command line with `raw_arg` rather than letting argv quoting mangle it. [Windows packaging](WINDOWS-PACKAGING.md) |
| A08 | Browser capture either hands off reliably or preserves the browser path | Met | Authenticated, expiring, POST and blob cases plus duplicate and cancel races. [browser capture](BROWSER-CAPTURE.md) |
| A09 | Acceleration claims are reproducible and include total usable-file time | Met, and the claim is *none* | Seven paired runs with their spread and 95% intervals. Fetchpath is **slower** than the curl baseline on loopback: 55.9 ms mean against 35.5 ms excluding the first pair, sd 16.3 against 2.1. No acceleration claim is made anywhere, so there is none to substantiate. [benchmark uncertainty](evidence/release/fp018-benchmark-uncertainty.json) |
| A10 | Optional sharing is explicit and obeys authorization and upload/cache budgets | Not in scope | Sharing is M8 (FP-020) and no sharing feature ships in 0.1.0. FP-018 does not list A10. |
| A11 | Compatibility packs deliver correct outputs and bounded failure handling | Met, with the corpus closed by decision | Protocol fixtures, media assembly checks and helper crash/update tests pass against local fixtures. The helpers are pinned and the real guided install downloads, verifies, installs and runs them. No public site is promised, by decision of 23 September 2026. HTTP 4xx failures are no longer retried as if they were connection problems. [release readiness](RELEASE-READINESS.md), [media integration](MEDIA-INTEGRATION.md) |
| A12 | A supported Windows installation can install, update, recover, and uninstall predictably | **Partial** | Re-run on 23 September: per-user install, upgrade to a 0.1.1 build over a running instance with the queue retained, and uninstall pass. The installer now also installs the CLI on PATH, registers the browser host and ships the extension; uninstall reverses each, and the user PATH was byte-identical afterwards on a 1,069-character PATH. x64 only by statement. **A clean-machine run remains.** [release readiness](RELEASE-READINESS.md), [evidence](evidence/windows/packaging-lifecycle.json) |

## Checks re-run for this candidate

23 September 2026:

```
cargo test --workspace --locked                        236 passed, 0 failed, 5 ignored
cargo test -p fetchpath-desktop -- --ignored guided_install   1 passed (real helper download)
cargo clippy --workspace --all-targets -- -D warnings  clean
cargo fmt --check                                      clean
node --test                                            24 passed, 0 failed
node tools/tasks.mjs check                             39 tasks valid
node tools/licenses/generate.mjs --check               current
node tools/media-tools/pin.mjs --check                 both pins match their publishers
corepack pnpm --dir apps/desktop release               Fetchpath_0.1.0_x64-setup.exe
pwsh tests/compatibility/windows/ui-accessibility.ps1  passed
pwsh tests/compatibility/windows/packaging-lifecycle.ps1  passed
```

The benchmark and protocol matrix were not re-run; nothing in the transfer path
changed.

## Re-run on 24 September 2026

FP-042 to FP-046 landed after the 23 September checks, and FP-045 touched the
transfer path (segment progress in `fetchpath-http`), so everything was re-run,
including the benchmark and protocol matrix.

- **FP-043, clean machine.** A fresh Windows Sandbox with no
  `VCRUNTIME140.dll`: silent per-user install; `fetchpath --help`,
  `--version` and a download from a terminal Explorer started; the same file
  downloaded to Complete in the app with a matching SHA-256; silent uninstall
  removed the program, its PATH entry and the three browser-host registrations
  and kept the downloads. [evidence](evidence/windows/sandbox-lifecycle.json)
  The run found one defect: a user PATH ending in `;`, the default on a fresh
  image, lost that separator after install and uninstall. Fixed in
  `apps/desktop/src-tauri/tools/user-path.ps1` with a round-trip test; the
  Sandbox was not re-run after the fix, by the user's decision.
- **FP-044, CLI on a clean Windows.** No VC++ import (checked by
  `tests/installer/runtime-imports.test.mjs`), and `--help` ran in the Sandbox.
- **FP-042 and FP-045, checked by hand by the user on 24 September:** the
  toolbar popup shows the connection and sends the page, a media page becomes
  a video download, a sent link starts Fetchpath when it is closed, and the
  details window shows speed, a moving graph and live segments on a real
  segmented download, leaving the queue unchanged when closed.
- **Dialog layout.** The generic `dialog.dialog` width rule outranked the Add
  download, Details and Settings widths, so every dialog rendered at the small
  default. Fixed; Settings now lays its groups in two columns with one scroll
  region, and the default window is 1040 px wide. At that size none of the
  three dialogs scrolls.

```
cargo test --workspace --locked                        239 passed, 0 failed, 6 ignored
cargo clippy --workspace --all-targets -- -D warnings  clean
cargo fmt --check                                      clean
node --test                                            37 passed, 0 failed
node tools/tasks.mjs check                             46 tasks valid
node tools/licenses/generate.mjs --check               current
node tools/media-tools/pin.mjs --check                 both pins match their publishers
node tools/bench/benchmark.mjs --repetitions 7         14 of 14 outputs match; budget held
node tools/bench/protocol-matrix.mjs                   http/1.1 and h2 verified by hash
```

Benchmark, excluding the first pair: curl 57.3 ms (sd 8.3), Fetchpath
77.2 ms (sd 14.1). Fetchpath is still slower on loopback and no speed claim is
made. The machine was under memory pressure, which likely explains the higher
absolute times. [evidence](evidence/release/fp018-rerun-2026-09-24.json)

## Known limitations of 0.1.0

These are things a user would notice. Each is stated in the README, the user
guide or the changelog.

1. **No public media site is promised.** Video and audio work where yt-dlp
   works; that changes as websites change.
2. **Pause is for file downloads only.** A media download has no checkpoint to
   return to, so pause is not offered on those rows.
3. **The browser extension loads in developer mode** in Chrome and Edge, and
   Firefox is not supported: release Firefox loads only signed add-ons. Store
   listings are external publication and have not been made.
4. **Unsigned.** SmartScreen warns on first run. The Authenticode hook is wired
   and inert; setting `FETCHPATH_SIGN_THUMBPRINT` signs every binary in the
   bundle with no other change.
5. **x64 only.**
6. **HTTP and HTTPS only in the app and CLI.** HTTP/3 is reported unavailable
   by the packaged libcurl; FTP, FTPS and SFTP exist in the engine's
   compatibility module but no user surface reaches them.
7. **No speed claim.** The one paired measurement shows Fetchpath slower than
   curl on loopback.
8. **Offline is not a modelled state.** A connection failure appears as a
   transport failure, which automatic retry handles.
9. **Power-loss durability is argued, not demonstrated.**
10. **The CLI names a file from its link only**, so a link with no file name is
    saved as `download`.

## Path to publication

When the user says so:

1. Create the GitHub repository and push `main`.
2. Build with `corepack pnpm --dir apps/desktop release`.
3. Tag `v0.1.0` and create a GitHub release from the 0.1.0 section of
   `CHANGELOG.md`, attaching `Fetchpath_0.1.0_x64-setup.exe` and
   `SHA256SUMS.txt`.

Store listings for the extension and code signing are worthwhile next steps,
not blockers.
