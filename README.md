# Fetchpath

A Windows download manager for everyday and power users, with video and audio
downloading included in the first public release. The engine is a portable Rust
core; the desktop client is Tauri 2 on Microsoft-serviced Windows 11 x64.

**Current state:** release-candidate work for 0.1.0. The desktop application
performs real downloads through the native core, with a persistent queue,
checkpointed pause and resume, schedules, browser capture, media quality
selection, settings and a statistics panel. It has not been published, and
interim builds are unsigned by decision.

See [PROJECT.md](PROJECT.md) for the current phase and
[docs/tasks/backlog.json](docs/tasks/backlog.json) for what remains. The backlog
is authoritative for task state; nothing else in the repository duplicates it.

## What it does today

- **Queue and history.** A persistent bounded queue with search, filters,
  batch adds, scheduled start times and recovery after a restart.
- **Honest progress.** Percent, received of total, transfer rate and remaining
  time, all derived from what the source actually stated. A source that reports
  no length produces no percentage rather than a guessed one.
- **Pause and resume.** A running file download pauses to its checkpoint and
  resumes from that offset, across a restart. Media downloads have no checkpoint
  to return to, so pause is not offered on them.
- **Safe publication.** Downloads are staged, verified and published
  create-only. Fetchpath never overwrites a destination and reports the SHA-256
  it observed, which is an integrity record and not a publisher-authenticity
  claim.
- **Verified multi-source repair.** Metalink 4 piece hashes localize damage and
  repair only the failing byte ranges from another mirror.
- **Bounded content cache.** Content carrying a trusted digest is retained under
  a quota so a later download of the same content can complete with no network
  at all. Cached bytes are re-verified before they are published, and anything
  fetched with credentials is recorded as such and is never shareable. A cache
  hit is reuse, not throughput, and is reported as such rather than as a
  transfer rate.
- **Protocols.** HTTP/1.1 and HTTP/2 through a statically linked libcurl, plus
  FTP, FTPS and SFTP. HTTP/3 is reported unavailable by the packaged build, and
  no speed claim is made for any of them.
- **Browser capture.** An extension hands off explicit per-link captures with
  exact-origin permissions, origin-scoped cookie replay and a DPAPI-protected
  inbox.
- **Video and audio.** Engine-confirmed quality lists and selected-variant
  downloads through supervised `yt-dlp` and `ffmpeg` helpers, which Fetchpath
  does not bundle and helps you set up instead.
- **Settings and Power mode.** Concurrency, default save folder, automatic
  retry, media tool setup, window behaviour and appearance. Power mode adds
  per-download diagnostics and a session statistics panel without moving
  anything else.

## Run and verify

Node.js 24+ and Git are enough for the repository checks. No npm install and no
third-party JavaScript dependencies are needed for them.

```powershell
node tools/tasks.mjs next          # the next ready task
node tools/tasks.mjs show FP-018   # one task in full
node tools/tasks.mjs check         # backlog graph, contracts and evidence
node --test                        # fixture, extension and backlog tests
node tools/licenses/generate.mjs --check   # third-party notices are current
```

The Rust workspace needs Rust 1.98+:

```powershell
$cargo = Join-Path $env:USERPROFILE '.cargo\bin\cargo.exe'
& $cargo test --workspace --locked
& $cargo clippy --workspace --all-targets -- -D warnings
& $cargo fmt --check
& $cargo build --workspace --locked
target\debug\fetchpath.exe download https://example.com/ example.html
```

The desktop application needs `pnpm` through corepack:

```powershell
corepack pnpm --dir apps/desktop install
corepack pnpm --dir apps/desktop tauri dev     # run it
corepack pnpm --dir apps/desktop tauri build   # optimized build and installer
```

On-machine compatibility harnesses, which install and drive the real artifacts:

```powershell
pwsh -NoProfile -File tests/compatibility/windows/ui-accessibility.ps1
pwsh -NoProfile -File tests/compatibility/windows/packaging-lifecycle.ps1
pwsh -NoProfile -File tests/compatibility/metalink/run.ps1
```

Both Windows scripts throw on a failed assertion and still write their partial
observation, so a JSON file with `"passed": false` is a failure record rather
than a missing run. See [benchmark instructions](tools/bench/README.md) for
fixture and curl baseline commands. Generated files belong under ignored
`work/`; promote a small verified summary into the validation record.

## Installing

Interim builds are **unsigned by decision**, so Windows SmartScreen warns the
first time one runs. That is the expected consequence of shipping without a
certificate, not a sign that anything is wrong with a build you produced
yourself. An Authenticode hook is wired into the bundle and is completely inert
with no certificate configured; setting `FETCHPATH_SIGN_THUMBPRINT` signs every
binary in the bundle with no other change.

The installer is per-user: it installs to `%LOCALAPPDATA%\Fetchpath`, registers
under `HKCU`, and never asks for elevation. Uninstalling asks whether to remove
your queue, history and settings, defaulting to keeping them; files you have
already downloaded are never removed, wherever you saved them.

`yt-dlp` and `ffmpeg` are **not** bundled. Settings detects whether you already
have them, accepts a folder you point at, or downloads a pinned version and
checks it against a recorded SHA-256 before installing it. A build whose
`media-tools.json` has no recorded digest refuses the download and offers only
the manual path; see [tools/media-tools/pin.mjs](tools/media-tools/pin.mjs) for
how a maintainer records one.

## Documentation

- [Project direction and current phase](PROJECT.md)
- [Architecture and roadmap](docs/architecture/PLAN.md)
- [Repository map and boundaries](docs/architecture/REPOSITORY.md)
- [UX contract](docs/product/UX.md)
- [Development workflow](docs/development/WORKFLOW.md)
- [Windows UX and packaging evidence](docs/development/WINDOWS-PACKAGING.md)
- [Metalink repair evidence](docs/development/METALINK-REPAIR.md)
- [Content cache and paired LAN evidence](docs/development/CACHE-AND-LAN.md)
- [Media integration evidence](docs/development/MEDIA-INTEGRATION.md)
- [Browser capture evidence](docs/development/BROWSER-CAPTURE.md)

## Working on it

[AGENTS.md](AGENTS.md) defines bounded retrieval, task ownership, evidence and
escalation. Start a task with its backlog ID and record `in_progress`, an owner
and a narrow owned file area before changing anything.

The repository has no remote and no published releases. Local development does
not imply publishing, paid provisioning, external messages, or destructive
actions.
