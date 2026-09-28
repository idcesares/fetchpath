# 0.1.0 release gate

FP-069, on the engine platform. Recorded 28 September 2026 on Windows 11 Pro
10.0.26200, x64. It replaces the FP-018 record for the pre-engine build at
`ce6615a` (22–24 September), which nothing will publish; that history is in Git.

## Verdict

**Ready to publish, on the user's word.** First passed on `1b6ef32`, then
re-run in full on 28 September 2026 on `dc01da2`, after the cache, paired
computers and discovery (FP-032 to FP-034), Hugging Face repositories (FP-022)
and the HTTP throughput fix. Every row below passed on that tree: the Rust
suite (509 passed, 0 failed, 8 ignored), clippy, fmt, Node (45), licence
notices, media pins, the real guided helper install, benchmark and protocol
matrix, the release build, and all eight Windows harnesses including the
Sandbox clean-machine run.

The re-run first reported one failure, "fetchpath.exe was not replaced", when
upgrading over a running engine. It was the harness, not the product: the
0.1.2 upgrade installer dated from the day before while the CLI it was
compared with was freshly staged. Rebuilt from the tree, both lifecycles
passed, and both now refuse an installer built before the last commit.

A change to the product after this needs the affected rows re-run before
`v0.1.0` is tagged: the Rust and Node suites always, the Windows harnesses for
anything they drive, the benchmark and protocol matrix for the transfer path.

Publishing is a GitHub release (tag `v0.1.0`, the installer and
`SHA256SUMS.txt`), made only when the user says so. No remote exists yet.

## Acceptance matrix

**Met** means evidence exists and was re-run for this gate; **Partial** means
real evidence that does not cover the criterion as written.

| ID | Criterion | Status | Evidence |
| --- | --- | --- | --- |
| A01 | Core journeys work for a nontechnical user; advanced controls stay optional | Met | `ui-accessibility.ps1` against the engine-client desktop in its own data folder: onboarding, keyboard-only add and publish, tab order, named controls, live regions, Settings and shortcuts. [evidence](evidence/windows/ui-accessibility.json) |
| A02 | Completed fixtures match expected bytes; known-bad content is never published as verified | Met | 14 of 14 benchmark outputs match the fixture SHA-256; checksum-mismatch and Metalink tests in the suites. [evidence](evidence/release/fp069-gate.json) |
| A03 | Interruptions never authorize unsafe concatenation or false completion | Met | Source-change, range-refusal, truncation and pause/publication race tests in `fetchpath-core` and `fetchpath-session`. |
| A04 | Checkpoints and publication recover within the documented envelope | Partial | Kill-point tests, resume across restarts, and upgrade over an engine mid-download (below). **No OS-level or power-loss test.** |
| A05 | Limits are obeyed | Met | Benchmark peak 1 of 8 requests and 7 MiB of the 32 MiB buffer; bounds tests. [evidence](evidence/release/fp069-gate.json) |
| A06 | Negotiation matches packaged capabilities | Partial | `http/1.1` and `h2` verified by hash; HTTP/3 not attempted because the packaged libcurl lacks it, and recorded so. No real HTTP/3 endpoint. |
| A07 | Credentials, filenames and local control stay inside their boundaries | Met | Pipe authentication, principal, filename-fence, redaction and malformed-input tests; FP-067's attack of the agent boundary. |
| A08 | Browser capture hands off or preserves the browser path | Met | Capture through the engine without the window (FP-056) and rules applied to captures (`ui-rules.ps1`). [evidence](evidence/desktop/ui-rules.json) |
| A09 | Acceleration claims are reproducible | Met; the claim is *none* | Loopback, 7 pairs, excluding the first: curl 62.7 ms (sd 12.7), Fetchpath 61.1 ms (sd 15.3), level after the HTTP fix (67.0 against 57.7 on `1b6ef32`). Over the internet Fetchpath is still behind curl on a Hugging Face file ([adaptive HTTP](ADAPTIVE-HTTP.md)). No speed claim is made. |
| A11 | Compatibility packs give correct output and bounded failure | Met by decision | Pinned helpers pass the real guided install; no public site is promised (decided 23 September). |
| A12 | Install, update, recover and uninstall predictably | Met, x64 only | Clean machine, upgrade and uninstall rows below. |
| A13 | Agents act only within granted permissions | Met | `ui-agents.ps1`, the policy and MCP suites, and FP-067's findings fixed. [MCP record](MCP.md) |

A10 (sharing) is not in 0.1.0's scope.

### Engine, terminal and agents

| Row | Result |
| --- | --- |
| Clean machine | Windows Sandbox 26100 with no `VCRUNTIME140.dll`: silent per-user install; from a new terminal `fetchpath --help`, `--version`, a direct download, and `fetchpath add --wait` through the engine (262144 bytes each); `fetchpath engine status`; the same file downloaded in the desktop to Complete with a matching SHA-256; silent uninstall **with the engine still running** stopped it and left no Fetchpath process, no install folder, no uninstall entry, no browser-host key, a byte-identical user PATH, and the downloads. [evidence](evidence/windows/sandbox-lifecycle.json) |
| Upgrade with a persisted queue | 0.1.0 queue, history and settings served by the upgraded engine; upgrade over a running engine mid-download finished under the hook's wait with the old engine gone. [evidence](evidence/windows/engine-lifecycle.json) |
| Uninstall with the engine running | Mid-download with sign-in start on: engine stopped, Run key removed, nothing left running. [evidence](evidence/windows/engine-lifecycle.json) |
| Installer upgrade over the running app | Queue kept, tray and single instance behave. [evidence](evidence/windows/packaging-lifecycle.json) |
| Terminal and command line | CLI integration suites (queue, engine, TUI, MCP) in `cargo test --workspace`. |

**Found by the clean-machine run.** Current Windows Sandbox images enforce
Smart App Control (`VerifiedAndReputablePolicyState` = 1), which blocks the
unsigned installer outright, with no "run anyway". The harness turns it off
inside the disposable Sandbox, matching the development host, and records
that it did. For people it is a real limitation of an unsigned build: README,
the user guide and the changelog now say so.

## Commands run

On `dc01da2`, 28 September 2026 (the installers for 0.1.1 and 0.1.2 rebuilt
from that tree as CONTRIBUTING.md describes):

```
cargo test --workspace --locked                        509 passed, 0 failed, 8 ignored
cargo clippy --workspace --all-targets -- -D warnings  clean
cargo fmt --check                                      clean
node --test                                            45 passed
node tools/licenses/generate.mjs --check               current
node tools/media-tools/pin.mjs --check                 both pins match
cargo test -p fetchpath-media -- --ignored guided_install   passed (real helper download)
node tools/bench/benchmark.mjs --repetitions 7         14 of 14 match; budget held
node tools/bench/protocol-matrix.mjs                   http/1.1 and h2 verified
corepack pnpm --dir apps/desktop release               Fetchpath_0.1.0_x64-setup.exe
pwsh tests/compatibility/windows/ui-accessibility.ps1  passed
pwsh tests/compatibility/windows/ui-agents.ps1         passed
pwsh tests/compatibility/windows/ui-rules.ps1          passed
pwsh tests/compatibility/windows/ui-cache.ps1          passed
pwsh tests/compatibility/windows/ui-lan.ps1            passed
pwsh tests/compatibility/windows/packaging-lifecycle.ps1  passed
pwsh tests/compatibility/windows/engine-lifecycle.ps1  passed
pwsh tests/compatibility/windows/sandbox-lifecycle.ps1 passed
```

## Known limitations of 0.1.0

Each is stated in the README, the user guide or the changelog.

1. **Unsigned.** SmartScreen warns, and Smart App Control blocks installation
   where it is on. The Authenticode hook is wired and inert
   (`FETCHPATH_SIGN_THUMBPRINT`).
2. **No public media site is promised**; video and audio work where yt-dlp does,
   and cannot be paused.
3. **The browser extension loads in developer mode**; Firefox is unsupported.
4. **x64 only.**
5. **HTTP and HTTPS only** in the app and CLI; no HTTP/3.
6. **No speed claim**; Fetchpath is slower than curl on loopback.
7. **Power-loss durability is argued, not demonstrated.**
8. **Agent protection assumes no malicious program already runs as the
   person.**
9. **Paired computers find each other only on one local subnet** (FP-033
   and FP-034, after this gate).

## Path to publication

When the user says so: create the GitHub repository and push `main`; build
with `corepack pnpm --dir apps/desktop release`; tag `v0.1.0` and create the
release from the changelog's 0.1.0 section with the installer and
`SHA256SUMS.txt`. Code signing and store listings for the extension are the
next steps worth taking, not blockers.
