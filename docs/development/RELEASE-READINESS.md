# Release readiness for 0.1.0 — FP-035 to FP-039

Recorded 23 September 2026 on Windows 11 Pro 10.0.26200 x64, on the developer
machine, with network access to the helpers' publishers. This closes the gaps
found by a deliverability review of the release candidate: the product worked,
but an ordinary user could not get to most of it without a developer's help.

## What the review found

| Area | Finding before this work |
| --- | --- |
| Desktop | The download list was below the fold even in a maximized 2560×1550 window: a marketing hero, the welcome card and the full composer came first. |
| Browser capture | The installer shipped `fetchpath-browser-host.exe` but registered it with no browser, and the extension was not shipped at all. Not listed as a limitation. |
| Media | Both helper checksums were empty, so the guided setup always refused. The ffmpeg URL pointed at a moving `latest` tag whose n7.1 build no longer exists (HTTP 404). |
| Command line | `--version`, `-h` and no arguments all printed usage; output was JSON only; no progress; a folder destination was refused; not installed. |
| Documentation | The README was a contributor document; there was no user guide, no licence text for the declared `MIT OR Apache-2.0`, no changelog. README claimed FTP/FTPS/SFTP, which no user surface can reach. |

## FP-039 — media helpers pinned

`media-tools.json` now names permanent, versioned releases: yt-dlp 2026.08.19
and the GyanD ffmpeg 9.0.2 essentials build. `node tools/media-tools/pin.mjs`
recorded each digest only after the download matched the publisher's own
checksum file (`SHA2-256SUMS`; gyan.dev's per-version `.sha256`). Both digests
also match the SHA-256 GitHub reports for the release asset.

| Helper | SHA-256 |
| --- | --- |
| yt-dlp 2026.08.19 | `66674953fe251b89f4d08c5f0e35e0728679bd67ab3d7d05c0562af101dd3e7a` |
| ffmpeg 9.0.2 essentials | `60f467265b1e312373dbcd92200c2618a74850f98d3d078e94296bb3fa2047ba` |

**Defect found and fixed.** The first real guided install failed: "The helpers
were downloaded but could not be found afterwards." yt-dlp is installed first,
so the ffmpeg archive was unpacked beside it; the wrapper folder was then not
the folder's only entry, was never flattened, and ffmpeg stayed two levels
below where discovery looks. The unit test only covered an empty folder. The
archive is now unpacked into a private staging folder, flattened there, and
moved into place. `an_archive_is_flattened_even_when_another_helper_is_already_installed`
is the regression test. The real path is covered by an ignored network test:

```
cargo test -p fetchpath-desktop --locked -- --ignored guided_install
test media_setup::tests::guided_install_fetches_verifies_and_runs_the_pinned_helpers ... ok  (167 s)
```

It downloads both helpers through the verified engine path, installs them,
proves both run, and proves a second run fetches nothing.

Media is documented as best-effort: no public site is named or promised
(decision of 23 September 2026). A11's corpus gate is closed by that decision,
not by a corpus run.

## FP-037 — the command line

`fetchpath download LINK [DESTINATION] [--sha256 HEX] [--json] [--quiet]`,
plus `--version` and `--help`. A missing destination means the Downloads
folder; a folder destination takes the file name from the link, decoded and
fenced against Windows-reserved names and characters. Progress goes to stderr
only when it is a terminal; the saved path goes to stdout. Exit codes: 0 saved,
2 input, 3 exists, 4 network or server, 5 checksum mismatch, 6 write, 130
cancelled. Observed against example.com and a local fixture: every code above
except 6 and 130 was produced by a real run.

The cache and paired-device commands were unlisted until FP-020's independent
review; after it (see [cache and paired LAN](CACHE-AND-LAN.md)) they are listed
under an advanced section of the help and documented in the CLI guide.

## FP-036 — browser capture reaches users

The installer now ships the unpacked Chromium extension (with icons) and two
host manifests whose `path` is relative, so no install path is written into
JSON. It registers `com.fetchpath.browser` under HKCU for Chrome, Edge and
Firefox; the uninstaller deletes exactly those keys. Settings → Browser
extension reports whether the host is registered (read through `reg.exe`, no
unsafe code) and walks the user through Load unpacked. Firefox is registered
for a future signed add-on but is not offered, because release Firefox only
loads signed add-ons.

## FP-035 — the queue is the main screen

The hero is gone. An app bar holds the brand, the active count, Add download,
Settings and Keyboard help; the queue fills the window; the welcome card is a
one-line banner; an empty queue shows a single call to action. The composer is
unchanged inside a modal dialog, so every validation, preview and announcement
it had still applies. Pasting a link anywhere opens it pre-filled. Rows are
compact: the destination is one truncated line with the full path as its
title, and measurements share a line with the row's actions.

Defects found while driving the new layout with real downloads, all fixed:

- **A 404 was retried automatically**, although Settings promises only
  connection problems are. The engine reports every HTTP error as
  `source.transfer_failed`; the desktop and CLI now treat HTTP 4xx other than
  408 and 429 as a link that needs a person, with a plain message.
  `a_link_the_server_refuses_waits_for_a_person_instead_of_retrying`.
- **"NaN hr left"** on a row waiting to retry: the snapshot omits
  `etaSeconds` when unknown and the check tested only for `null`.
- A stale speed and time left on stopped rows; "100%" on a checksum mismatch
  that saved nothing; an animated bar on waiting rows; a retry notice styled
  as an error.
- **Live regions inert under a modal.** A modal dialog makes the page inert,
  so the page-level live regions could not announce a form error raised inside
  Add download. They now move into the top-most open dialog.
- Focus fell to the document body after the first add (its trigger, the empty
  state's button, disappears) and after Pause replaced itself with Resume.

## Installer, PATH and a defect that must not recur

The first installer build edited the per-user PATH inside NSIS. NSIS's
`ReadRegStr` returns an **empty string** for a value longer than
`NSIS_MAX_STRLEN` (1024); it does not truncate. On this machine's 1,069
character PATH the installer therefore wrote a PATH holding only the install
folder, and the uninstaller then wrote an empty one. The value had been saved
beforehand and was restored byte for byte. The edit now lives in
`tools/user-path.ps1`, which reads the raw value through the registry API with
no length limit, keeps `%VARIABLE%` references and the value type, and refuses
any add that shortens PATH or any remove that empties one holding other
entries. `tests/installer/user-path.test.mjs` covers a 4,489-character PATH.

Real install and uninstall, silent, on this machine:

- files: desktop app, `fetchpath.exe`, browser host, both host manifests, the
  extension, `docs\GUIDE.md`, `docs\CLI.md`, both licence texts, notices;
- PATH after install equals the previous value plus `;` and the install folder,
  and `fetchpath --version` resolves from a fresh environment;
- all three browser keys point at the installed manifests, and the installed
  app's Settings reports Chrome and Edge connected;
- after uninstall the PATH is byte-identical to the original, and the install
  folder and all three keys are gone.

## Harnesses re-run

| Harness | Result |
| --- | --- |
| `tests/compatibility/windows/ui-accessibility.ps1` | Passed. 15 tab stops in order across page and dialog; 17 focusable controls, none unnamed; keyboard-only journey published 262144 bytes and announced it; Escape keeps the draft and Cancel clears it; machine restored. [evidence](evidence/windows/ui-accessibility.json) |
| `tests/compatibility/windows/packaging-lifecycle.ps1` | Passed. Install, upgrade to a 0.1.1 build over a running instance with the queue retained, uninstall; PATH byte-identical afterwards. [evidence](evidence/windows/packaging-lifecycle.json) |
| FP-031 checksum in the real UI (DevTools protocol, no global input) | A pasted `SHA256:` digest with capitals and spaces matched and published; a wrong one published nothing, showed both digests with Edit checksum and Retry, and was not retried. |

Both harnesses were updated for the new layout: the composer is opened before
its fields are filled, popup buttons are opened through ExpandCollapse (how
Chromium exposes `aria-haspopup`), and a refused connection may legitimately be
*Scheduled* by automatic retry when sampled.

`apps/desktop/tools/ui-smoke.ps1` was **not** run: it sends global keystrokes to
whatever window is in front.

## Still open

- A clean-machine install on a freshly imaged Windows 11 x64 PC.
- Chrome may warn about developer-mode extensions; store listings are external
  publication and were not attempted.
- The CLI takes the file name from the link only, not from `Content-Disposition`; a link
  with no file name saves as `download`.
