# 0.2.0 release gate

FP-103, recorded 4 October 2026 on Windows 11 x64. This replaces the
0.1.0 gate, retained in Git with its [historical benchmark evidence](evidence/release/fp069-gate.json).

## Verdict

**Local release gate passed; publication awaits green GitHub CI.** Independent
reviews and final package checks passed. No gate is waived.

Candidate: `Fetchpath_0.2.0_x64-setup.exe`, built from the FP-103 release tree.
SHA-256: `9d8e7434cd952c297a7848908cb89296b5f78734c353edef80125e2ef84dd1a4`.
It is unsigned. The hash checks downloaded bytes, not publisher authenticity.

Compact [machine-readable evidence](evidence/release/fp103-gate.json) records the tested product revision and artifact.

## Current evidence

| Check | Result |
| --- | --- |
| Workspace | `cargo test --workspace --locked --no-fail-fast`: 674 passed, 0 failed, 9 ignored. |
| Static checks | Workspace strict clippy and formatting passed; final session strict clippy passed after the localized recovery repair. |
| Repository | `node tools/tests.mjs`: 92 passed. The runner excludes ignored worktrees and installed plugin copies. |
| Desktop | TypeScript/Vite and final release build passed; 5 engine identity/reconnect tests passed. |
| Policy and recovery | Isolated-target ignored session regression passed: running/scheduled media and torrent policy changes; failed/deferred/successful saves; Published to saved Running to restart to Completed without a duplicate folder. 11 helper tests passed. |
| Protocols | Actual FTP, FTPS and SFTP fixtures passed, including Ed25519/password, resume and RSA private-key refusal. |
| Dependencies | JavaScript audit: no known vulnerabilities. RustSec findings assessed below. Notices regenerated and checked for 718 packages. Media publisher pins matched. |
| Secrets | Gitleaks 8.30.1: no findings across 160 commits after two exact exceptions for verified public Chromium identity keys. Release history scan is repeated after the final commit. |
| Package | Release build passed. [Clean install](evidence/windows/fp103-clean-install.json), [public 0.1.0 upgrade](evidence/windows/fp103-upgrade.json), [88 agent installer assertions](evidence/windows/fp103-agent-installer.json), and [actual guest sign-out/sign-in](evidence/windows/sign-in-lifecycle.json) passed. |
| Review | Independent lifecycle, security and dependency reviews ran. Named findings repaired; independent bounded final publication recovery recheck passed. |

## Security findings and repairs

Desktop reconnects preserve the original instance pin. Automatic agent policy
changes checkpoint and rebuild active helpers instead of leaving old byte caps.
Torrent publication uses Windows atomic no-replace moves. Recovery markers
remain until completion is saved successfully, and intermediate Running records
keep the original recovery identity. SFTP RSA private-key signing is refused
before any signature; password, Ed25519 and ECDSA remain available.

RustSec database revision: `ef6173cbc5c50ec8166f9a5b28f07834144373ee`,
checked with cargo-audit 0.22.2. Raw audit is not clean: two advisories,
six maintenance warnings and one unsoundness warning remain.

- [RUSTSEC-2023-0071](https://rustsec.org/advisories/RUSTSEC-2023-0071.html),
  `rsa 0.10.0-rc.18`: no patched upstream version. The reachable private RSA
  authentication path is disabled and tested; public server-key verification remains.
- [RUSTSEC-2026-0293](https://rustsec.org/advisories/RUSTSEC-2026-0293.html),
  `ringbuf 0.4.8`: reachable through the packaged torrent helper's librqbit
  stack. The pinned usage is a byte ring buffer (`SharedRb<Heap<u8>>`); the
  reported panic-in-element-destructor prerequisite does not apply to `u8`.
  This assessment is specific to that usage, not an upstream fix.
- `glib 0.18.5` unsoundness and `proc-macro-error` maintenance warnings are
  outside the Windows target, including the helper feature. Other platforms
  are unvalidated. Five `unic` maintenance warnings are in the Windows
  Tauri/urlpattern stack; no concrete vulnerability was identified.
- The yanked `yoke-derive 0.8.3` was replaced with `0.8.4`.

GitHub secret scanning, push protection and dependency alerts are enabled;
No open secret alerts were observed. The GTK/Linux-only glib dependency alert was dismissed as not used in the supported Windows target, with its scope recorded. Actions are pinned to verified commit IDs,
CI checks the JavaScript audit, and Dependabot covers Cargo, desktop npm and
GitHub Actions. These checks complement review and tests; they do not
claim that the software has no vulnerabilities.

## Acceptance and limits

A01-A03, A05, A07-A09 and A11-A13 reuse established contract evidence and
current relevant regressions. A04 remains partial: process restart and
checkpoint/publication recovery are tested; sudden power loss is not.
A06 remains partial: HTTP/1.1 and HTTP/2 are supported; HTTP/3 is not promised.
The previous benchmark is historical correctness evidence, not a new
performance result. No Internet speed advantage is claimed.

Windows 11 x64 only; no ARM64, remote hub or pre-sign-in Windows service.
Torrents have no pause, per-file selection or seeding after completion.
Media tools are optional and pinned; no named public website is promised.
Claude Code registration/discovery are covered, while its model-driven
current download test is owner-deferred. SmartScreen warnings and Smart App
Control blocking remain consequences of the unsigned distribution; the
Sandbox harness adjusts SAC only in its disposable guest.
