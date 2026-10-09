# 0.3.0 release gate

FP-105, recorded 9 October 2026 on Windows 11 x64. This replaces the 0.2.0
gate, kept in Git at tag `v0.2.0` with its evidence.

## Verdict

**Published: [v0.3.0](https://github.com/idcesares/fetchpath/releases/tag/v0.3.0).**
[GitHub CI](https://github.com/idcesares/fetchpath/actions/runs/37974334581) passed on the
release commit. No gate is waived. Release tag: `4d1ffb3c9d9d8beddf7d19c69293a609b28c8bfa`;
PR #20 merged with the same tree. The public installer download was verified
against the checksum below.

Published installer: `Fetchpath_0.3.0_x64-setup.exe`, built from `4caa42a`. SHA-256: `bc5c03bc56a4d6d383edcca580a6d949777259f59a0756278a1301105fc69921`.
It is unsigned. The hash checks downloaded bytes, not publisher authenticity.
Later commits change only harnesses and records, not packaged binaries.

Version 0.3.0 is a minor release: the local web UI is a feature, and 0.2.0
cannot read a queue that holds a browser download.

## Current evidence

| Check | Result |
| --- | --- |
| Workspace | `cargo test --workspace --locked --no-fail-fast`: 704 passed, 0 failed, 9 ignored. |
| Static checks | Workspace strict clippy (`-D warnings`) and `cargo fmt --check` passed. |
| Repository | `node tools/tests.mjs`: 92 passed. `node tools/tasks.mjs check` passed. |
| Desktop and web | `tsc`, `pnpm build` and `pnpm build:web` passed. The release engine embeds the web bundle (its asset names are in the staged `fetchpath.exe`; the "not built" page is not). |
| Web UI | FP-104 record: [WEB-UI.md](WEB-UI.md), including G10 in Chrome, the owner's Edge, Firefox, scaling, contrast and keyboard checks, and the independent strong review with its fixes. |
| Dependencies | JavaScript audit: no known vulnerabilities after `source-map-js` 1.2.2 (GHSA-68fv-2mgg-jv7q, build-time only). RustSec findings unchanged from 0.2.0, below. Notices checked for 720 packages. |
| Secrets | Gitleaks 8.30.1 over the release history: no findings. The RFC 6455 sample nonce in the web UI tests is marked inline. |
| Package | Release build passed. [Clean install](evidence/windows/fp105-clean-install.json), [public 0.2.0 upgrade](evidence/windows/fp105-upgrade.json) (queue, setting, data folder and a running download kept), [88 agent installer assertions](evidence/windows/fp105-agent-installer.json) and [actual guest sign-out/sign-in](evidence/windows/sign-in-lifecycle.json) passed on this installer. |

The first upgrade run stopped when the public 0.2.0 installer (hash checked)
exited with code 2 in the guest, before 0.3.0 ran; the rerun with the same
file passed and the failure did not reproduce. The first sign-in run timed
out before setup; its harness also still expected version 0.2.0. Both
harnesses were corrected (version, and a default Full selection stored by a
components-era baseline) before the passing runs.

## Dependency advisories

RustSec database revision `7eebec69c352c7191b1f13eb95dd510eeca5d1de`, checked
with cargo-audit 0.22.2. The findings are the ones assessed for 0.2.0.

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
