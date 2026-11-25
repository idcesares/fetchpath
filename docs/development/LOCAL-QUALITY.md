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

## Windows sign-in (FP-108)

Windows Run previously invoked the console executable as `fetchpath.exe engine`,
leaving its blank console open for the engine lifetime. Sign-in now invokes
`fetchpath-startup.exe`, a Windows GUI executable that uses the existing
windowless launch/attach path. Core always packages it, including installs
without Desktop; normal CLI use stays a console application. Engine startup
refreshes opted-in legacy registrations. Uninstall recognizes both commands
only when they name this install.

Validation: 120 CLI unit tests and 13 engine lifecycle tests passed, including
GUI/console PE subsystem checks, launcher start/reattach with stable instance
and start time, and refusal during an installer update hold. Installer checks passed (49 passed, 7 skips for unavailable release artifacts
and Tauri CLI); repository structure checks passed (7). Strict CLI/engine
clippy, workspace formatting, backlog validation and whitespace checks passed.
Independent GPT-6 Astra review found no actionable issues.
No installed user copy was changed; a rebuilt installer is needed to deploy
this repair. No physical reboot or installer rebuild was performed.

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

## Field fixes (FP-109)

Automatic capture now returns a cancelled download to the browser on any
failure and when the host does not answer within 10 s. Settings > AI agents
connects or disconnects Codex and Claude Code through `fetchpath agent-setup`.
The first-run media step is hidden when helpers are found. The disk reserve
accepts No limit (stored as 1 byte; `fetchpath settings disk-reserve-bytes
off`), and a full drive says so. `tools/install/install.ps1` is the headless
install. Checked: browser background tests, space/core/CLI unit tests, tsc,
fmt. Not run: the script against a real release; a real-browser capture.

FP-110: drive space is measured off the queue lock (2 s cache, first look waits at most 100 ms, so a dead share reads as unmeasurable for that pass); a cancel during a reserve stop wins; setup writes browser keys and PATH before reconnecting agents (exit 11 unchanged; NSIS not run).

FP-111: a page sent from the browser menu is named from its title with `.html` (it was saved as an extensionless file, seen in the field); a repeat capture of the same page still makes a second copy. Waiting-for-space rows explain themselves; the remote-listener tests serialise on the shared port. Records ADAPTIVE-HTTP, WINDOWS-PACKAGING, DESKTOP-CHECKSUM, WEB-UI and MEDIA-INTEGRATION were consolidated (old text in ARCHIVE.md). Not run: a real-browser page send, a vite build, the accessibility script.

Real-PC run, 18 October 2026 (FP-111 build, unsigned): `install.ps1` kept a Full install on a rerun without -Components, connected and removed an agent host, uninstalled (data kept, browser key and PATH entry removed), installed cli+mcp from the real 0.3.0 release (checksum verified, no desktop), then upgraded to Full with the new setup (agent registration kept); desktop and engine started, helpers reported ready. Found and fixed: `fetchpath settings disk-reserve-bytes auto|off|10GB` never worked (the numeric arm shadowed it). Still true: a URL with no file name (a root page) is saved as extensionless `download`; the name is fixed before the response is seen.

FP-112: an agent job is checked against its folder grants again as it starts (a junction swapped after the request holds it for approval; a folder the person approved stays approved). The loopback web UI replies with `Cache-Control: no-store` and a CSP with `base-uri`/`object-src`; the desktop CSP gained both. Verified with the session and remote tests and a regression test that fails without the check. Not run: the desktop built with the new CSP, or the web UI loaded in a browser.

## Stabilization pass (FP-136, FP-137, FP-139, FP-140, FP-141, FP-144)

24 November 2026 on Windows 11 x64 (Rust 1.100.0 local toolchain; CI remains Rust 1.98.1):

- **FP-136 (toolchain & accessibility harness):** Replaced deprecated `AtomicUsize::fetch_update` in `crates/fetchpath-protocol/src/pipe/server.rs` with `compare_exchange_weak` loops (clean on Rust 1.98 and 1.100); updated `browser_host.rs` and `engine.rs` integration tests to resolve `fetchpath.exe` from `CARGO_BIN_EXE_fetchpath-browser-host` / ancestor walk for Cargo 1.100's build-dir layout; fixed `ui-accessibility.ps1` modal-inertness assertions (`add-open` instead of closed `url`), replaced flaky fixed sleeps with `Wait-Until` polling, added null guards, and restored `FETCHPATH_APP_DATA_DIR` in `finally`.
- **FP-137 (desktop UI & Tauri):** Moved Settings status icons to Vite-emitted `'self'` SVGs (`assetsInlineLimit: 0` in desktop builds) and restored `#browser-setup-state[data-ready="false"]::before` mask; wired manual `"Inspect again"` to `applyInspection()`; removed HTML `required` on `#url` during `Edit checksum`; routed queue action/refresh errors to a dismissible main-page banner (`#queue-error-banner`) without spamming alerts while the engine is stopped; stabilized the Inbox panel (structural diffing, focus preservation, immediate fast-copy completion/failure reporting, dismissible status); unminimized the window in `show_main_window`; fixed Settings `.quiet` alignment, `.torrent-options` class, `.agent-row` layout, inline `<code>`, and High Contrast tab border; serialized `changeSetting`; reloaded Settings after starting a stopped engine; fixed dialog opener stack and `openDialog` on already-open modals; updated Power mode queue signature and `clearComposer()` torrent reset.
- **FP-139 (session, browser capture & web UI):** Quarantined unreadable/missing `browser-secrets/<id>.bin` and invalid contexts in `ingest_browser_locked` so one corrupt capture never poisons `Session::list()`; aligned `is_media_page()` with desktop `FILE_EXTENSIONS` so dotted handles (`@john.doe`, `ubuntu-24.04`) route to media review; aligned `extensions/browser/policy.js` filename sanitization with Rust `safe_filename()` (240-byte UTF-8 cap, reserved stems, Unicode whitespace/dots, bidi/C1 chars); filtered invalid/insecure cookies in `background.js`, serialized `handingBack` via `storage.session` + TTL and left POST downloads to the browser, and made `pendingSend` origin checks fail closed; added `X-Fetchpath-Session` header so `socket.ts` distinguishes sign-out (and stops polling) from transient `503`/drops; pre-started `Web::apply(true)` before committing `UpdateSettings` to the ledger so bind failures never poison the ledger or persist partial settings; cleared the web UI draft on Cancel.
- **FP-140 (media failure classification & UI errors):** Added redacted `MediaError::{NoMediaFound, AuthRequired, SourceUnavailable}` variants and expanded `classify_helper_failure()` in `adapters/media/src/lib.rs` so non-video/photo links (e.g. X `/photo/1`), login-required posts, and 404/unavailable media no longer report as `helper_crash`. Desktop `errorMessage()` now strips leading machine error codes, quiet media inspection displays the specific reason, and switching to media mode clears stale suggested filenames before inspection.
- **FP-141 (photo-page auto-detection):** Added bounded, cookieless `fetchpath_core::inspect_page_image` (HTTP/HTTPS only, 512 KiB HTML cap, `og:image`/`twitter:image` and direct `image/*` detection, placeholder logo filtering, safe filename + extension derivation) exposed via desktop command `inspect_page_image`. Desktop Add download now auto-detects X `/photo/N` links and falls back to page-image resolution when media inspection finds no video, automatically switching to File mode with the direct image URL and filename; also fixed the startup `refreshToolsStatus()` auto-analysis race and empty `Save as` validation in media mode.
- **FP-144 (conservative cleanup):** Removed redundant `[dev-dependencies] tempfile` in `apps/cli/Cargo.toml`, unused `Win32_Security_Cryptography` feature in `crates/fetchpath-session/Cargo.toml`, unused `browser_bridge`/`view` re-exports, unnecessary `#[allow(clippy::too_many_arguments)]` in `verified.rs`, duplicate private `sha256_file` and `accept_any` helpers, leftover `cachedExcludedOrigins`/`cachedAutoCapture` state in `extensions/browser/background.js`, dead CSS rules and unused `QueueView` methods in `apps/desktop/src/`, duplicate `refreshShare()` IPC in `main.ts`, obsolete `FETCHPATH_HTTP_SCHEDULER` env var in `tools/bench/resume.mjs`, and reconciled documentation across `PROJECT.md`, `REPOSITORY.md`, `docs/user/`, and `docs/development/`.
- **Verification:** `cargo fmt --check`, strict `cargo clippy --workspace --all-targets --locked -- -D warnings`, `cargo test --workspace --locked`, `cargo test -p fetchpath-relay --features serve --locked`, desktop `tsc` + `vite build` + `vite build --mode web`, `node tools/tests.mjs` (114 passed), `node tools/tasks.mjs check`, PowerShell parser check of `ui-accessibility.ps1`, and independent boundary reviews + repair rechecks (verdict PASS). Not run: live `ui-accessibility.ps1` desktop automation run and physical reboot.
