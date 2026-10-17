# Local development baseline and acceptance

FP-106 and FP-107, checked 17 October 2026 on Windows 11 x64, based on
`ecbaccd` plus the task changes. This is local development evidence, not a
new installer or release gate.

## Baseline

Checked from the isolated `fp-106-107-local-quality` worktree:

- `corepack pnpm --dir apps/desktop install --frozen-lockfile`, `build` and
  `build:web`: passed, with lockfiles unchanged.
- `FETCHPATH_REQUIRE_WEB=1 cargo build --workspace --locked`: passed.
- Rust workspace test targets: 704 passed, 9 existing ignored; all doc-test
  targets passed (no examples). Coverage combines passing targets and focused
  reruns after fixture repairs rather than claiming a single final full run.
- `cargo clippy --workspace --all-targets --locked -- -D warnings`,
  `cargo fmt --all -- --check` and `git diff --check`: passed.
- `node tools/tests.mjs`: 88 passed, 4 skipped, no failures. The skips are
  unbuilt release/installer-sidecar runtime-import checks.
- `cargo fetch --locked` then `node tools/licenses/generate.mjs --check`:
  third-party notices are current.
- `node tools/tasks.mjs check`: passed for 107 tasks.

The contributor guide now gives the required bundle/native build order.
The development index, field-feedback status and CI description are reconciled.

Ordinary transfer test fixtures explicitly choose a small disk reserve instead
of depending on the host having 5% of its entire drive free. Production defaults
are unchanged; dedicated disk-reserve tests still exercise waiting, stopping and
recovery, and the space unit tests still check the automatic reserve formula.
Use NTFS for transfer tests: this machine's exFAT volume does not support the
staging operations. Tests and browser QA use isolated queue and output folders.

## Local journeys

The shared queue recovers keyboard focus when a row action replaces its own
button (Pause/Resume). Browser approval announcements respect its capabilities
and direct approval to Fetchpath on this PC. The web client now moves live
regions into an open modal, returns focus to links after a rejected submission,
and clears a dismissed draft's alert before opening another dialog.

Chrome with the real scratch engine and loopback fixtures checks the pending
approval view, rejected draft, details and Escape recovery, disk-reserve waiting
then publication, HTTP 403 guidance, pause/resume and engine interruption.
The query-free recovery fixture resumed with nonzero Range offsets and published
4,194,304 bytes matching its expected fixture SHA-256
`5844dba40249a165238afbf182bb9ebc09617fd11ee8289cc0f650368d2298c0`.
A query-bearing link instead required a refreshed source after restart, as
intended because private query values are not saved.

## Limits

No installer rebuild, physical reboot, power-loss test, new media-site corpus,
remote access, paid provisioning or publication is part of this check.
The installed browser-extension toolbar was not exercised again: browser policy
blocks the extension-management page. Extension behavior tests and the framed
native-host/real-engine handoff tests provide the current capture checks;
the owner's prior real-Chromium verification remains historical evidence.
Chrome accessibility-tree checks prove region placement and focus, not audible
Narrator output. Edge/Firefox and theme/scaling observations remain the existing
FP-104 evidence; they were not repeated here.
