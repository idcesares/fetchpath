# 0.4.0 release gate

FP-113, recorded 19 October 2026 on Windows 11 x64. This replaces the 0.3.0
gate, kept in Git at tag `v0.3.0` with its evidence.

## Verdict

**Candidate, not published.** Branch `release/0.4.0`. The release installer
was not built for this version: both build attempts were stopped by the
machine running out of memory. Nothing is tagged or published. Publishing
(tag, GitHub release, `SHA256SUMS.txt`, the 0.4.0 download names in README and
GUIDE) is the owner's decision and happens after the installer is built,
installed and checked. The installer will be unsigned; a hash checks bytes,
not publisher authenticity.

0.4.0 is a minor release: it adds in-app agent connection, the headless
installer and a "no limit" disk reserve. 0.3.0 can still open a 0.4.0 queue.

## Current evidence

| Check | Result |
| --- | --- |
| Workspace | `cargo test --workspace --locked --no-fail-fast`: 710 passed, 0 failed, 9 ignored. |
| Static checks | Workspace strict clippy (`-D warnings`) and `cargo fmt --check` passed. |
| Repository | `node tools/tests.mjs` (with the debug host built): 99 passed. `node tools/tasks.mjs check` passed. Extension tests: 13 passed. |
| Desktop and web | `tsc`, `pnpm build` and `pnpm build:web` passed. `pnpm audit --prod`: no known vulnerabilities. |
| Reviews | Independent strong review of the new start-time folder-grant check (FP-112): four findings, all repaired, repair recheck passed. Independent strong review of `tools/install/install.ps1` (FP-109, never reviewed before): eight findings, repaired; the script parses and rejects bad arguments. |
| CI | GitHub `checks` passed on PR #32 at commit 6660571 (14 min; an earlier duplicate run was cancelled by the newer one). |
| Real PC | An earlier build of this tree (before the folder-grant check and the popup change) was installed, upgraded over 0.3.0 and run: headless install, upgrade, agent host connect and removal, uninstall keeping data. See LOCAL-QUALITY.md. |

## Not run

- The 0.4.0 release build, installer, clean install and 0.3.0-to-0.4.0 upgrade.
- The desktop and the loopback web UI with the tightened Content-Security-Policy
  (no browser load has confirmed nothing is blocked).
- A real-browser popup send, and the reported first-click failure was never reproduced; the popup changes address its likeliest causes only.
- `tests/compatibility/windows/ui-accessibility.ps1` and the Sandbox harnesses.
- `cargo audit` and secret scanning (tools not installed on the build PC).

## Dependency advisories

RustSec database revision `7eebec69c352c7191b1f13eb95dd510eeca5d1de`, checked
with cargo-audit 0.22.2. The findings are the ones assessed for 0.2.0. Not re-run for 0.4.0: cargo-audit is not installed on the build PC, so these are the 0.3.0 results.

- [RUSTSEC-2023-0071](https://rustsec.org/advisories/RUSTSEC-2023-0071.html),
  `rsa 0.10.0-rc.18`: no patched upstream version. The reachable private RSA
  authentication path is disabled and tested; public server-key verification remains.
- [RUSTSEC-2026-0293](https://rustsec.org/advisories/RUSTSEC-2026-0293.html),
  `ringbuf 0.4.8`: reachable through the torrent helper's librqbit stack as a
  byte ring buffer (`SharedRb<Heap<u8>>`); the panic-in-element-destructor
  prerequisite does not apply to `u8`. Specific to that usage, not an upstream fix.
- `glib 0.18.5` unsoundness and `proc-macro-error` maintenance warnings are
  outside the Windows target. Five `unic` maintenance warnings are in the
  Windows Tauri/urlpattern stack; no concrete vulnerability was identified.

The new web UI dependencies (hyper, hyper-util, http-body-util,
tokio-tungstenite) have no advisories. GitHub secret scanning, push
protection, dependency alerts and pinned Actions remain as for 0.2.0. These
checks complement review and tests; they do not claim that the software has
no vulnerabilities.

## Acceptance and limits

A01, A09 and A13 for the web UI rest on FP-104's tests, G10 and review. The
0.2.0 acceptance evidence still applies to the unchanged engine; A04 (no
sudden power-loss test) and A06 (no HTTP/3) remain partial. No Internet speed
advantage is claimed.

Windows 11 x64 only; no ARM64, remote hub, LAN access or remote control. The
web UI's awaiting-approval view was not exercised in a browser. Going back to
0.2.0 after adding downloads from a browser is not supported. Torrents have
no pause, per-file selection or seeding after completion. Claude Code's
model-driven current download test remains owner-deferred. SmartScreen
warnings and Smart App Control blocking remain consequences of the unsigned
distribution; the Sandbox harnesses adjust SAC only in their disposable guest.
