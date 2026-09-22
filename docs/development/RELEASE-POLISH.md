# Release polish evidence

Covers FP-026 through FP-030, the work that closed the gap between what
[the UX contract](../product/UX.md) promised and what the desktop application
actually did. Verified 22 September 2026 on Windows 11 Pro build 10.0.26200.0,
x64.

## Why these tasks existed

The backlog said one gate remained before the first release candidate. An audit
against the UX contract found that several things the contract described had
never been built, and that one recorded observation was not true:

| UX contract | State before this work |
| --- | --- |
| "Progress, speed, time remaining" | The queue carried `bytesReceived` and nothing else. `main.ts` documented the progress bar as permanently indeterminate because "the total size is not known", while `transfer.rs` was computing `expected_total` and discarding it. |
| Pause and resume on every item, a `Paused` state | Not implemented anywhere in the desktop application. The core already had `CancelCleanup::RetainStaging` and checkpoint resume; nothing used them. |
| "Open folder" on completion | Only "Copy path". |
| "retry only when enabled in settings" | There was no settings surface at all. |
| "Power mode exposes diagnostic and transfer details inline" | Did not exist. |
| "default Downloads destination" | The destination field started empty and was required. |
| Media helpers | `FETCHPATH_MEDIA_TOOLS_DIR` or nothing. A first run that chose "Video or audio" produced `helper_unavailable` and a "Retry after setup" button with no setup to do. |

`packaging-lifecycle.ps1` additionally recorded
`uninstallerOffersDataRemovalCheckbox = $true` as a hardcoded constant, for a
control the uninstaller did not have. That field is now read from the shipped
configuration instead, and the branch it describes exists.

## FP-026 — Transfer statistics

The total was already known inside the engine and thrown away at the boundary.
It now reaches the interface, and only ever from a value a source actually
stated.

- `CancellationToken` carries a `total` alongside `received`. Zero means
  unknown, and `set_total` ignores `None` and a stated zero rather than
  recording them.
- `transfer.rs` sets it at the three points the engine learns it: a recovered
  checkpoint's `expected_total`, each adaptive chunk's `total_bytes`, and the
  validated response headers of a single-response transfer.
- Headers are read through a new `header_total`, which returns `None` rather
  than a fallback. The existing `response_total` substitutes the bytes received
  when a response omits a length, which is correct for a checkpoint written
  after the body is complete and wrong for live progress: mid-transfer it would
  report the bytes so far as the total and show 100% from the first chunk.
- The desktop layer derives rate and remaining time from successive samples with
  a time-based exponentially weighted average, ignoring samples closer together
  than 400 ms and withdrawing the rate after 5 seconds without progress. Rate
  and remaining time are cleared for every state except `running`, so a queued,
  paused or finished row never shows a speed nothing is producing.
- The interface shows a percentage and a remaining time only when a total is
  known. Otherwise the bar stays indeterminate and the row says the total size
  is unknown.

Three core tests fix this behaviour: a stated `Content-Length` becomes the
reported total; a response with no declared length reports `total_bytes: None`
while still receiving every byte; and a queued job has no total before the
source has stated one.

## FP-027 — Pause and resume

Pause stops a running download at its retained checkpoint. Resume recreates a
recoverable job, which revalidates that checkpoint against the source before
reusing a byte of it.

The pause command races publication, and the race is handled rather than
assumed away: cancellation is refused once the engine has committed to
publishing, and when that happens the download really did finish, so the command
reports the completion instead of claiming a paused state for a file already on
disk. The worker is joined with the queue lock released, because joining waits
for a network read to unwind and would otherwise freeze the poll that draws the
interface.

Two defects were found and fixed by the tests rather than by inspection:

1. `refresh_record` copied the underlying job's state over the view on every
   poll. A record paused before it started still held a prepared job reporting
   itself as queued, so the next poll put it straight back in the queue and
   reconcile started it — the exact transfer the user had stopped.
2. `QueueRecord::restore` treated any non-terminal saved state as resumable and
   rebuilt it as queued, so a paused download started itself on the next launch.

Four desktop tests cover it. The central one pauses a throttled 768 KB transfer
partway through and asserts that the resumed transfer sent exactly one
`If-Range` request — the header only the resume path sends — and that the
published file matches the source byte for byte. Others cover pausing before the
first byte, surviving a restart still paused, and media downloads refusing to
pause rather than pretending: a media job has no checkpoint to return to, so
pause is not offered on those rows at all.

## FP-028 — Settings and Power mode

`settings.rs` persists the choices to `settings-v1.json` beside the queue, with
two rules:

- **A settings file never stops Fetchpath from starting.** A truncated or
  hand-edited file falls back to defaults, a file from an older build keeps
  every field it does have, and the load reports that it had to repair
  something so the interface can say so rather than presenting defaults as the
  user's own choices.
- **Every bound is enforced in one place.** Concurrency clamps to 1–8, retry
  attempts to 0–10, and the backoff to 5–3600 seconds. A stored directory is
  discarded unless it is absolute, because a relative path would resolve against
  whatever directory Fetchpath happened to be launched from.

Automatic retry re-queues on a doubling backoff, and only for failures the
engine classified as plain transport trouble. A destination conflict, an invalid
link, an expired private source and a missing helper all need a person to decide
something; retrying them on a timer would bury that decision under repeated
identical failures.

Power mode is off by default and strictly additive: it adds a session
statistics panel above the queue and a diagnostics block inside each row,
and removes or moves nothing.

Six settings tests and two desktop tests cover clamping, the round trip, the
truncated-file fallback, the relative-path rejection, the backoff ceiling, and
that a stored concurrency wins over the launch default across a restart.

## FP-029 — Guided media tool setup

Fetchpath does not redistribute `yt-dlp` or `ffmpeg`. Settings now detects
helpers the user already has, accepts a folder they point at, or downloads a
pinned artifact — through the same verified transfer path as any other download
— and checks it against a recorded SHA-256 before installing it.

**The pin is the whole point, and this build is deliberately unpinned.**
`media-tools.json` ships with empty `sha256` fields. The application refuses the
guided download for any entry without one and says so by name, and the interface
offers only the manual path in that state. A digest computed here from whatever
arrived would prove the bytes survived the network and nothing about who
published them; presenting that as authenticity is exactly the claim
[AGENTS.md](../../AGENTS.md) forbids.

`tools/media-tools/pin.mjs` is the only sanctioned way to fill a digest in. It
downloads the artifact *and* the publisher's own checksum document, compares
them, and writes the digest only when the two agree. It needs network access, so
a maintainer runs it; the build does not.

**This is the known limitation of this feature at 0.1.0.** Until a maintainer
runs `pin.mjs`, the guided download is unavailable and media setup is the
detect-or-choose-a-folder path. That path is complete and tested. Every layer
below it — download, verify, extract, flatten, re-verify by running
`--version` — is implemented and exercised against local fixtures.

Six module tests cover the manifest's shape, the refusal of an unpinned entry
with nothing left behind, an empty directory reporting not-ready, a wrong folder
being rejected with the path named, and the wrapper-directory flattening that
published ffmpeg archives need. Five repository tests check the shipped assets
themselves.

## FP-030 — Installer data removal and license manifest

`installer-hooks.nsh` adds `NSIS_HOOK_PREUNINSTALL`. An interactive uninstall
asks whether to also remove the queue, history and settings, defaulting to
**No**, because an uninstall is often really a reinstall and must not lose a
half-finished download. A silent uninstall takes the keeping branch, because
nobody is present to choose and keeping is the recoverable answer. Answering
yes removes exactly the two directories Fetchpath created:

```
%APPDATA%\app.fetchpath.desktop        queue, settings, browser inbox, media-tools
%LOCALAPPDATA%\app.fetchpath.desktop   the WebView2 profile
```

Downloaded files are never touched, wherever they were saved. A download manager
that deletes your downloads when you uninstall it is a data-loss bug, not a
thorough cleanup.

`tools/licenses/generate.mjs` replaces the hand-maintained notices file. It
reads the package graph from `Cargo.lock` and each license from the crate's own
manifest in the local registry, falling back to `curated.json` for the
statically linked C libraries. Every row says which of the two it came from. A
crate with neither fails the run, so a new dependency cannot reach a release
with no license recorded. The generated file covers **566 packages**, and
`--check` fails when it is out of date.

`makensis` accepted the hook and produced `Fetchpath_0.1.0_x64-setup.exe`, so
the branch is compiled into the shipped installer rather than only present in
the repository.

## Checks run

```powershell
cargo test --workspace --locked                        # 120 passed, 0 failed
cargo clippy --workspace --all-targets -- -D warnings  # clean
cargo fmt --check                                      # clean
node --test                                            # 20 passed, 0 failed
node tools/tasks.mjs check                             # 30 tasks valid
node tools/licenses/generate.mjs --check               # notices current
corepack pnpm --dir apps/desktop build                 # tsc + vite, clean
corepack pnpm --dir apps/desktop tauri build           # installer produced
pwsh -NoProfile -File tests/compatibility/windows/ui-accessibility.ps1
```

## Accessibility

The accessibility harness was extended for the new surface and found two real
problems on its first run against the new build, both since fixed:

1. **A timing race in the harness itself.** The welcome card is revealed by an
   asynchronous settings read, so sampling for it at the instant the window
   appears reported "no card" for a card that showed a moment later, shifting
   every tab-order step by two. The harness now polls for it.
2. **An unnamed group in the application.** `media-options` took its accessible
   name from a heading inside the inspect-controls branch. With the helpers not
   set up, that branch is hidden and the section had no name. The name now lives
   outside both branches.

A third problem surfaced from reading the harness's own intent rather than its
output. It asserts that the composer is empty before the journey types into it,
warning against "a field the application populated on its own". The new
destination suggestion satisfied that assertion, but re-reading it exposed a
defect: the suggestion fired on the first character typed, so `h` became
`Downloads\download.bin` and never corrected itself, because from the second
character the field was no longer empty. The suggestion now only runs while the
destination still holds Fetchpath's own value — any edit or folder pick makes it
the user's and it is never overwritten — and only once the address parses as a
real URL.

The harness additionally checks that the settings dialog is a named modal
window, takes focus, hides the page behind it from assistive technology, returns
focus on Escape, and exposes no unnamed focusable control; that the welcome card
is dismissible from the keyboard and does not come back after a restart; and
that whichever media branch this machine presents is operable, so neither is a
dead end.

The harness then passed with **no failures** against the optimized executable,
from a clean machine state that it restored on exit:

- Tab order follows reading order for all 19 stops, starting at the skip link.
- 26 keyboard-focusable controls, **none unnamed**.
- The welcome card showed on first launch, was dismissed with Enter, and did
  **not** return after a restart, so the dismissal persisted.
- Advanced options opened from the keyboard.
- The settings dialog is a `Window` named "Settings" with 11 focusable
  controls, none unnamed, and it hides the page behind it.
- The keyboard-only journey published 262144 bytes, the queue card is named
  "keyboard-only.bin, Complete", and the polite live region announced
  "keyboard-only.bin finished downloading."
- The media branch shown was the setup path, because this machine has no
  helpers, and it is operable rather than a dead end.
- The shipped stylesheet still carries every rule UI Automation cannot see:
  reduced motion, forced colours, `:focus-visible`, the skip link, the pressed
  filter marked by more than colour, a `Highlight` focus ring under forced
  colours, and screen-reader text that is not uppercased.

Recorded observations: [ui-accessibility.json](evidence/windows/ui-accessibility.json).

## What is not claimed

- No speed or acceleration claim. The statistics panel reports throughput
  observed across active downloads, which is not a measure of the user's
  connection, and the panel says so.
- The observed SHA-256 remains an integrity record, not publisher authenticity.
- The guided media download is unavailable in this build by design, as above.
- Builds remain unsigned by decision, with SmartScreen consequences documented.
- Pause and resume are proven for HTTP file downloads against a local fixture.
  Media downloads are excluded from pause rather than supported.
