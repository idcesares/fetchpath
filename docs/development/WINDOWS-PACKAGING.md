# Windows UX and packaging evidence

FP-017 and acceptance criteria A01/A07/A12, plus FP-079 to FP-082, FP-099, FP-100 and FP-102. Baseline: Windows 11
Pro build 10.0.26200.0, x64, Microsoft-serviced Edge WebView2 Evergreen runtime 153.0.4234.48. ARM64 is deferred.

## What was built

Fetchpath installs per user, keeps its queue across an upgrade, and removes its program files on uninstall while
leaving the user's queue where the user can see it. The primary journey (paste an address, choose a destination, add
to the queue) is operable from the keyboard alone and fully named in the Windows accessibility tree. Advanced
controls sit behind one **Advanced options** disclosure, closed by default.

- **Per-user install.** The NSIS bundle installs to `%LOCALAPPDATA%\Fetchpath` and registers under `HKCU`. No
  elevation, no `Program Files`, no `HKLM`. Tauri names the main binary after the Cargo bin target, so the installed
  executable is `fetchpath-desktop.exe`; shortcuts and the Apps & features entry read "Fetchpath".
- **One instance per user account.** A second launch exits and brings the running window forward, including from the
  notification area. The guard is a deny-sharing handle on `%APPDATA%\app.fetchpath.desktop\instance.lock`. It fails
  **open**: only `ERROR_SHARING_VIOLATION` or `ERROR_LOCK_VIOLATION` means another instance is running; any other
  error lets the application start. Activation matches the window class **and** the title.
- **Accessibility.** One skip link, one polite live region for queue changes and one assertive region for problems,
  an accessible name on every focusable control, reading-order tab traversal, a real focus indicator, a modal
  shortcuts dialog that takes and returns focus, and status carried by text as well as colour. Two live regions, not
  many: per-card `aria-live` was removed and the byte counter silenced with `aria-live="off"`; announcements are
  deduplicated. The queue re-renders only when a signature over the visible job fields changes, and when the control a
  user was on disappears, focus moves to the queue heading. Row actions are labelled "<action>: <filename>". The
  selected filter carries `aria-pressed`, a weight change and an inset ring (a double border under forced colors).
- **Filename boundary.** A destination filename is rejected if it contains `< > : " / \ | ? *`, ends in a dot or
  space, names a reserved Windows device, or escapes the chosen folder. The colon rule closes an NTFS alternate data
  stream (`notes.txt:hidden` would satisfy the create-only publication fence while attaching bytes to a file the user
  never chose).
- **Unsigned interim distribution.** An optional Authenticode hook is wired into the bundle and is inert with no
  certificate configured (see Distribution).
- **Updates are manual.** There is no updater: a new version is a new installer run over the old one.
  `bundle.windows.allowDowngrades` is `false`.

### Installer components (FP-099)

`src-tauri/installer.nsi` forks Tauri's NSIS template (recorded upstream hash; `tests/installer/template-fork.test.mjs`
fails when the installed CLI's template differs). One section per component (Core, Desktop with its WebView2 step,
Terminal, AI agents, Browser integration, Torrent helper), an installation type page (Full or Custom, unticked media
box), a components page, "Change components / Repair / Uninstall" on a same-version reinstall, quiet `/COMPONENTS=`
(exit 10 and nothing touched when invalid; `/UPDATE` ignores it), the selection stored with the uninstall entry
(`FetchpathInstallType`, `FetchpathComponents`, `FetchpathComponentsSchema`; unreadable values are derived from disk),
removal of deselected components after the engine stops, and read-only "installed components" in `fetchpath engine
status` and Settings. The torrent helper's missing-file error says to rerun setup. `fetchpath.exe` is one binary.

Interactive setup offers two default-off steps: media tools through the pinned `fetchpath tools install --yes` (the
same command as Settings), and browser extension guidance that opens the bundled extension folder and shows
Chrome/Edge's Load unpacked steps (browsers must confirm the extension themselves; there is no store listing or ID).
Silent and passive setup skip both. Once the media download starts, Setup's Cancel cannot stop it; it finishes or
reports a failure and points to Settings. The Settings window has one scroll area with category navigation and a
project link.

### Guided agent registration (FP-102)

The AI agents component has a separate, unchecked host-selection page for per-user Codex and Claude Code
registration; quiet setup requires the explicit `/AGENTHOSTS` option to add a new registration. Setup grants no
download access. The CLI owns configuration mutation, conflict handling and recovery (see
[MCP](MCP.md#guided-host-setup-fp-102)). Upgrade/repair reconcile owned executable paths. Uninstall-first version
replacement carries `/REPLACE` internally so connections survive until the replacement reconciles them. Actual removal
and AI agents deselection clean still-matching owned entries regardless of the application-data retention choice.
Errors exit 11 while retaining the program and recovery inventory; edited entries require explicit resolution. The
update hold is lifted only once selected program files are in place, before connection checks; a failed cleanup lifts
it too so recovery commands can run.

## How it was verified

```powershell
corepack pnpm --dir apps/desktop build
corepack pnpm --dir apps/desktop tauri build
corepack pnpm --dir apps/desktop tauri build --config '{"version":"0.1.1"}'
pwsh -NoProfile -File tests/compatibility/windows/ui-accessibility.ps1   # also under Windows PowerShell 5.1
pwsh -NoProfile -File tests/compatibility/windows/packaging-lifecycle.ps1
cargo test -p fetchpath-desktop
cargo clippy -p fetchpath-desktop --all-targets -- -D warnings
cargo fmt --check
```

Observations: [ui-accessibility.json](evidence/windows/ui-accessibility.json) and
[packaging-lifecycle.json](evidence/windows/packaging-lifecycle.json). Both scripts throw on any failed assertion and
still write their partial observation, so a JSON file with `"passed": false` is a failure record. Installer SHA-256
values are recorded in the observation files.

### Keyboard input is delivered to the renderer window, not synthesised globally

The accessibility script delivers keystrokes straight to the `Chrome_RenderWidgetHostHWND` window and reads focus from
the `HasKeyboardFocus` property of elements inside the renderer subtree. An automated run cannot rely on being the
foreground session (`foregroundWindowClass` in the observation file), `keybd_event` only reaches the foreground
window, and `AutomationElement.FocusedElement` reports the rest of the desktop; a message delivered by handle is
unaffected. The delivery routes are not interchangeable:

- **`Tab` is sent** (`SendMessage`, bounded by `SendMessageTimeout`), so `TranslateMessage` never synthesises a
  `WM_CHAR`. A *posted* `Tab` moves focus and then types a literal `WM_CHAR(0x09)` into the field it moved into; the
  journey asserts that tabbing left both text fields empty.
- **`Enter` and `Escape` are posted**, because Blink activates a button on `keypress` (a sent `Enter` opens neither
  the disclosure nor the shortcuts dialog); the character `Escape` translates to, `0x1B`, is not inserted.
- **Text is posted as `WM_CHAR`**, one character at a time.

The loopback fixture server is part of the harness and its liveness is recorded next to the download result, so a
stopped listener does not look like a failed transfer. Modifier chords cannot be exercised by either route (Chromium
reads real asynchronous key state), so `Ctrl+L` is asserted as declared (`AcceleratorKey = Control+L`), not pressed.

### Accessibility results

All assertions passed on three consecutive runs from a clean state, under both PowerShell editions.

| Check | Result |
| --- | --- |
| Named focusable controls | 24 of 24 with an empty queue; `unnamedFocusableControls` is empty. The count is not fixed (each queue card adds two named row actions), so the assertion is on the unnamed list being empty |
| Tab order | skip link, Keyboard help, download type, address, destination, Choose, Advanced options, start time, date picker, Start when ready, Add to queue, search, then the five filters: 17 steps in reading order |
| Advanced options | Enter on the `<summary>` reveals the start-time controls |
| Roles and names | address `Edit` "Download addresses one per line"; destination `Edit` "Save first item as"; `Button` "Choose a destination file", "Add to queue", "Keyboard help", "Start when ready…", "Inspect link"; `Edit` "Search downloads"; `ComboBox` "Quality"; `Group` "Download type", "Filter downloads", "Inspect available quality" |
| Queue figure | `Group` named "No downloads are active." rather than loose runs |
| Keyboard-only journey | typed a loopback address and path, read back byte-identical, six Tabs to submit, Enter; reached **Complete** and published 262 144 bytes with the fixture still serving |
| Live regions | polite: "keyboard-only.bin finished downloading."; assertive: "javascript:alert(1) is not an HTTP or HTTPS address." |
| Queue card naming | `keyboard-only.bin, Complete` (filename plus state) |
| Error cleared | Escape emptied the composer, removed the error text and left focus on `start-download` |
| Queue filters | exactly one `ToggleState = On`; Enter on **Active** moved it, still exactly one |
| Modal shortcuts dialog | `Window` "Keyboard shortcuts"; focus lands on **Close**; the page behind is not reachable; Escape closes and returns focus to **Keyboard help** |
| Skip link | moves focus to the `queue-title` heading |
| Stylesheet rules in the shipped bundle | `prefers-reduced-motion`, `forced-colors: active`, `:focus-visible`, `.skip-link`, the non-colour pressed-filter rule, `solid Highlight` focus under forced colors, screen-reader sentence excluded from `text-transform: uppercase` |
| Machine left as found | the run's own application data and WebView2 profile are removed in a `finally`; `machineRestored.matchesPreflight` is asserted and a pre-existing installation's data is left untouched |

### Install, upgrade and uninstall

**Install** (`Fetchpath_0.1.0_x64-setup.exe /S`, 3.5 s): directory under `%LOCALAPPDATA%`; one Apps & features entry at
`HKCU\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\Fetchpath` (DisplayName `Fetchpath`, DisplayVersion `0.1.0`,
Publisher `Fetchpath contributors`); installed files `fetchpath-desktop.exe`, `fetchpath-browser-host.exe`,
`THIRD-PARTY-NOTICES.md`, `uninstall.exe`; Start menu and desktop shortcuts resolving to the installed executable
(targets read, not assumed); `HKCU\SOFTWARE\Fetchpath contributors\Fetchpath` recording the install directory, which
Tauri reads to restore a custom location; `Get-AuthenticodeSignature` reports `NotSigned` for installer and
executable, the intended interim state.

**Upgrade over a running instance**: the installer stopped the running instance itself and completed in 2.8 s with
exit code 0; there was still exactly one uninstall entry, now `0.1.1`.

**Retained queues across a real upgrade**: three jobs seeded through the installed UI with loopback fixtures (a
completed 256 KiB download, a failed one on a closed port, a failed one whose address carried
`?token=fp017-private-query-value`). `queue-v1.json` held three records without the private query value, was
byte-identical after the upgrade, and the relaunched cards read `retained-complete.bin, Complete`,
`retained-failed.bin, Needs attention` and `retained-private.bin, Link needed`. The private-source job requires a
refreshed link after restore (host test
`queue_persistence_recovers_safe_sources_and_requests_private_source_refresh`); the private value appeared nowhere in
the queue file or any accessible name.

**Uninstall** (`uninstall.exe /S`, 2.6 s, run the way Apps & features runs it):

| Item | After a silent uninstall |
| --- | --- |
| `%LOCALAPPDATA%\Fetchpath`, program files, Start menu and desktop shortcuts, `HKCU\...\Uninstall\Fetchpath` | removed |
| `%APPDATA%\app.fetchpath.desktop` (`queue-v1.json`, `instance.lock`) | **retained** |
| `%LOCALAPPDATA%\app.fetchpath.desktop\EBWebView`, the WebView2 profile | **retained** |
| `HKCU\SOFTWARE\Fetchpath contributors\Fetchpath`, the recorded install path | **retained** |

Keeping is the default and a user choice (FP-100): the queue is the user's own record (`queue-v1.json` holds the
history including redacted source URLs). The interactive uninstaller's confirmation page carries a **"Delete the
application data"** checkbox, unticked by default and ignored when setup passes `/UPDATE`. A silent `/S` keeps all
retained rows; `/S /DELETEAPPDATA` is the same checkbox ticked, and is how the delete branch is tested. The retained
registry value is tied to the same checkbox because Tauri reads it to restore a custom install location. Setup also
ends every program running from the install folder (browser host, torrent and media helpers, the app) before removal,
and the script deletes the update hold last.

**What is Fetchpath-owned** (inventoried from `EngineHome` in `crates/fetchpath-protocol/src/launch.rs`,
`apps/cli/src/lan.rs` and `crates/fetchpath-browser-inbox`):

| Root | Contents | Removed by the choice |
| --- | --- | --- |
| `%APPDATA%\app.fetchpath.desktop` | queue and history, settings, rules, agent grants (`agents-v1.json`), engine secret and endpoint, torrent metadata snapshots and caches, content cache, browser inbox, `cli.toml` terminal preferences and history, `media-tools\`, instance and window locks | yes |
| `%LOCALAPPDATA%\app.fetchpath.desktop` | WebView2 profile (`EBWebView`), LAN pairing identity and pins | yes |
| `HKCU\SOFTWARE\Fetchpath contributors\Fetchpath` | recorded install folder | yes, with its empty parent |
| browser-host keys, per-user PATH entry, sign-in Run value, update hold | registrations | removed on every uninstall |
| anywhere the person saved a download, and the default Downloads folder | user content | never |
| `FETCHPATH_APP_DATA_DIR` | a data folder moved by environment variable | never: the uninstaller does not know it |

Nothing owned was found outside the two roots. File names inside the roots were listed from code, not from a clean
install after every feature was used. Manual removal is the two `app.fetchpath.desktop` folders and
`HKCU\SOFTWARE\Fetchpath contributors`.

**Why the bundler's removal is not used.** Tauri's script deletes the two roots with NSIS `RMDir /r`, which follows
junctions. Measured with the NSIS 3.11 that Tauri bundles: a scratch `RMDir /r` over a folder holding a junction to a
folder with a sentinel file deleted the sentinel (`myDelete` in `exehead/util.c` recurses without checking for a
reparse point). A planted junction would have turned the checkbox into deletion of its target.

**What runs instead.** `installer-hooks.nsh`: `NSIS_HOOK_PREUNINSTALL` stops the engine (`FETCHPATH_STOP_ENGINE`),
reads the checkbox or `/DELETEAPPDATA`, copies `tools\remove-app-data.ps1` to the uninstaller's plugin folder and
clears the bundler's flag so its `RMDir /r` never runs. `NSIS_HOOK_POSTUNINSTALL`, after the bundler has removed the
program files, runs the script, then repeats the bundler's registry cleanup for this choice. The script (Windows
PowerShell 5.1 safe, .NET calls only) removes only the two exact leaf directories under the profile folder, refuses a
root that is itself a junction or link, deletes a reparse point inside a root as a link without entering it, and keeps
going after a locked file, listing each failure and exiting 3 (reported in the uninstall log and a message box).
`tests/installer/data-removal.test.mjs` covers the hook ordering, the single choice, and the script against scratch
folders with a real junction inside a root and as a root.

An interactive "uninstall first" reinstall runs the previously installed uninstaller (such as 0.1.0) with the
bundler's original cleanup, which lacks the junction handling; the protection applies from the version that carries
it. Group Policy that blocks PowerShell scripts or enforces Constrained Language Mode makes the removal script fail:
reported with exit level 3 and a message box, and the data is kept. The bundled engine stop and PATH edit use the same
PowerShell route.

### Windows Sandbox evidence

- Clean-Sandbox lifecycle ([sandbox-lifecycle.json](evidence/windows/sandbox-lifecycle.json)): an owner's run found a
  missing `VCRUNTIME140.dll` in the torrent helper; the helper now links the MSVC runtime statically and the package
  check reads its PE imports. The rebuilt installer (SHA-256 `d1a90be5...596f`) passed the automated lifecycle without
  the Visual C++ redistributable, including a helper start; the owner's interactive pass of the setup choices, torrent
  intake and Settings window found no defect (FP-079 to FP-082).
- Components (one installer, one run at a time, in `evidence/windows/`): `sandbox-components-full.json` (default
  install, Full stored, `/UPDATE /COMPONENTS=cli` changes nothing, keep-data uninstall);
  `sandbox-components-terminal-only.json` (nine refused selections exit 10 with the machine untouched;
  `/COMPONENTS=cli` installs no desktop exe, no WebView2 bootstrapper, Terminal Start entry, PATH, no host keys; CLI
  and engine work; uninstall clean); `sandbox-components-upgrade-to-terminal.json` (an earlier Full build seeded with
  history, a setting, a data file and a running download, then `/COMPONENTS=cli`: desktop exe, shortcuts, host keys,
  host, extension and helper removed; data, setting and history kept; the download finished). `node --test` covers the
  template, `MAINBINARYNAME` keying, parsing, storage, removal and no-consent rules as text.
- Data removal: `sandbox-delete-data.json` (`sandbox-lifecycle.ps1 -DeleteData`) and `sandbox-lifecycle.json`.
- Agents: `tests/installer/agent-setup.test.mjs` protects consent and removal ordering;
  `tests/compatibility/windows/agent-installer.ps1` runs a disposable Sandbox lifecycle (host discovery fixtures are
  command stubs; real agent calls are checked by `agent-hosts.ps1`). The release installer passed 88 assertions on
  Sandbox build 26100: the host-selection page started unchecked and registered both explicit selections; quiet setup
  without host choices registered nothing; repeated setup, repair, update and uninstall-first replacement retained
  connections; component removal and both keep-data and delete-data uninstall removed owned entries while unrelated
  configuration and completed downloads survived ([installer evidence](evidence/windows/agent-installer.json)). The
  unsigned build's Smart App Control adjustment was confined to the disposable Sandbox.

### Native compatibility

| Property | Observed |
| --- | --- |
| WebView2 runtime | Evergreen 153.0.4234.48 already installed; the installer is configured `webviewInstallMode: downloadBootstrapper` (silent), fetched only when the runtime is absent |
| Process DPI awareness | `2` (per-monitor); tao calls `SetProcessDpiAwarenessContext` at startup (no `dpiAware` manifest entry) |
| Effective window DPI | 144 (150% scaling) |
| Top-level window class | `Tauri Window` |
| Close to tray | closing the window keeps the process alive with the window hidden |
| Second launch | exits with code 0, leaves the first instance running, brings its hidden window back |

## Distribution, signing and SmartScreen

**Decision: unsigned interim builds, distributed as a direct download.** No certificate is obtained, generated or
installed by this repository.

An optional signing hook is wired in and is inert. `tauri.conf.json` sets `bundle.windows.signCommand` to
`powershell -NoProfile -ExecutionPolicy Bypass -File tools/sign-windows.ps1 %1`, and
`apps/desktop/src-tauri/tools/sign-windows.ps1` prints one line and exits 0 when `FETCHPATH_SIGN_THUMBPRINT` is unset
(once per binary, including the NSIS uninstaller and plugin DLLs). To enable signing, set
`FETCHPATH_SIGN_THUMBPRINT` to the SHA-1 thumbprint of a certificate in a Windows certificate store, and optionally
`FETCHPATH_SIGNTOOL`, `FETCHPATH_SIGN_TIMESTAMP_URL` and `FETCHPATH_SIGN_STORE`.

Two NSIS constraints shaped that command: Tauri writes it into the installer script as `!uninstfinalize '<command>'`,
a preprocessor directive without NSIS string escaping, so an apostrophe terminates the argument (the bundle fails with
`!uninstfinalize expects 1-3 parameters, got 28`) and a `$` arrives doubled (`$hook` becomes `$$hook`, a PowerShell
parse error). The command therefore carries no quotes, no `$` and no inline script, and the binary path arrives
unquoted, so a repository or output path containing spaces needs rework before signing is enabled.

### What unsigned means for the person downloading it

- **SmartScreen, app reputation.** An unsigned executable from the web carries the Mark of the Web and has no
  reputation, so SmartScreen shows "Windows protected your PC"; **Run anyway** is behind **More info**. Reputation is
  per binary, so every release starts over.
- **SmartScreen in Edge, download stage.** The download can be reported as unrecognised and need an explicit **Keep**.
- **Defender Antivirus.** Unsigned installers are more likely to be sent for cloud analysis and exposed to heuristic
  false positives.
- **Enterprise policy.** WDAC, AppLocker and Smart App Control commonly require a signature or publisher, so an
  unsigned per-user installer is a likely block in a managed environment.

### What a certificate would change

- **OV code signing certificate.** Names a verified publisher in UAC and SmartScreen dialogs. It does **not** grant
  instant SmartScreen reputation, though reputation then attaches to the publisher rather than each binary.
- **EV code signing certificate.** Hardware-backed key and usually higher cost. Microsoft says EV no longer grants
  automatic SmartScreen reputation.
- **Artifact Signing (formerly Trusted Signing).** A Microsoft-operated service with short-lived certificates and no
  key material to hold; CI-friendly but needs an eligible Azure subscription and identity validation, and Public
  Trust eligibility depends on publisher location and type.

`sign-windows.ps1` is where any of these plugs in. The current hook supports a certificate in a Windows store; a
service-backed signer needs its own integration and validation. Until then: this is an unsigned build, SmartScreen
will warn, and that warning is correct.

## Helper and licensing attribution

Fetchpath's own code is **MIT OR Apache-2.0** (`[workspace.package]` in the root `Cargo.toml`).
`THIRD-PARTY-NOTICES.md` ships beside the executable. What is redistributed was determined from `Cargo.lock`, the
`curl` features in `crates/fetchpath-core/Cargo.toml`, the `bundle.externalBin` entries in
`tauri.release.conf.json`, and strings read from the compiled binaries.

**Statically linked**, via `curl = { features = ["static-curl", "http2"] }`:

| Library | Version | Evidence | License |
| --- | --- | --- | --- |
| libcurl | 8.21.0 | `curl-sys 0.4.90+curl-8.21.0` | curl license (MIT/X derivative) |
| nghttp2 | 1.68.1 | `libnghttp2-sys 0.1.13+1.68.1` | MIT |
| zlib | inflate 1.3.2 | `libz-sys 1.1.29`; the binary contains `inflate 1.3.2 Copyright 1995-2026 Mark Adler` | zlib license |

**TLS is Schannel**, provided by Windows and not redistributed. `fetchpath-browser-host.exe` (libcurl, no webview)
contains 91 distinct `schannel:` diagnostic strings and no OpenSSL runtime symbols or version banner; `openssl-src`
does not appear in `Cargo.lock`, and `openssl-sys`/`openssl-probe` are present only as platform-gated dependencies of
`curl`.

**Not redistributed:**

- **Edge WebView2 Runtime**: Microsoft-supplied and serviced; the installer downloads Microsoft's bootstrapper when the
  runtime is missing and embeds no Microsoft binary.
- **yt-dlp, FFmpeg and ffprobe**: supervised as external child processes, never bundled. The release config bundles
  only `fetchpath` and the torrent helper as `externalBin` (the base `tauri.conf.json` has none) and no `media-tools`
  resource; `adapters/media` discovers them from `FETCHPATH_YT_DLP` plus `FETCHPATH_FFMPEG_DIR`, from
  `FETCHPATH_MEDIA_TOOLS_DIR`, or from a `media-tools` directory beside the executable.

This is a licensing boundary. If a future release ships FFmpeg inside the installer, FFmpeg's LGPL-2.1-or-later terms
(or GPL, depending on the enabled components) begin to apply, alongside yt-dlp's Unlicense notice. That decision has
not been made and must not be made implicitly by dropping binaries into `media-tools`.

## Limitations that still hold

- **No screen reader was driven end to end.** Assertions were made through UI Automation, the tree Narrator reads, but
  Narrator, JAWS and NVDA were not run; what a screen reader says aloud is not verified.
- **`Ctrl+L` was not pressed**, only verified as declared (`AcceleratorKey = Control+L`); modifier chords cannot be
  delivered by a sent or posted message. `Tab`, `Escape` and `Enter` were exercised.
- **`aria-invalid` and `aria-describedby` could not be read back.** Chromium reports an empty `AriaProperties` and the
  .NET UI Automation client exposes neither `IsDataValidForForm`, `FullDescription` nor `LegacyIAccessiblePattern`.
  The state and association are in the markup and the error text reaches the assertive live region and the tree, but
  the association itself is unverified by automation.
- **Fresh-image coverage is incomplete.** The WebView2 bootstrapper path and ARM64 remain open.
- **One display, one scale factor.** DPI awareness was read from the live process; no physical move between monitors
  of different scale was performed.
- **Visual review is not automated.** Reduced motion, forced colors and light mode are asserted as rules in the
  shipped stylesheet, not as rendered screenshots.
- **Delete-data uninstall:** `sandbox-delete-data.json` records a clean-Sandbox run (FP-100); the interactive checkbox
  and a locked file during removal were not driven there. The script is also tested on scratch folders.
- **Interactive installer pages** (type, components, finish, reinstall radios, agent host page) are compiled and
  checked as text but were not driven in the Sandbox; native control automation verifies page wiring, not appearance.
  Selections not run there (Desktop-only, Custom with MCP, add components later, repair) rest on the same code paths.
  One early Full run's WebView2 installer step failed (setup exit 2) and passed on rerun; its log was not captured.
  Media tool download sizes were not measured; sidebar bitmap DPI was not checked (no bitmaps are configured).
- The NSIS hook does not clear the error flag before opening the extension folder, so an earlier error could log a
  false "could not open" line.
- **No generated per-crate license manifest.** The native C libraries are identified precisely, but a
  machine-generated license list for the whole Rust dependency graph of the shipping binary has not been produced; it
  is an open release gate.
