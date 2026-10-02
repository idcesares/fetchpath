# Windows UX and packaging evidence

Verified 22 September 2026 for FP-017 and acceptance criteria A01/A07/A12, on
Windows 11 Pro build 10.0.26200.0, x64, with the Microsoft-serviced Edge WebView2
Evergreen runtime 153.0.4234.48 already present.

## Delivered behavior

Fetchpath installs per user, keeps its queue across an upgrade, and removes its
program files on uninstall while leaving the user's queue where the user can see
it. The primary journey - paste an address, choose a destination, add to the
queue - is operable from the keyboard alone and is fully named in the Windows
accessibility tree. Advanced controls sit behind one **Advanced options**
disclosure, closed by default and off the primary path.

- **Per-user install.** The NSIS bundle installs to `%LOCALAPPDATA%\Fetchpath`
  and registers under `HKCU`. No elevation, no `Program Files`, no `HKLM`.
- **One instance per user account.** A second launch exits and brings the
  running window forward, including from the notification area. Two processes
  would otherwise both own `queue-v1.json`.
- **Accessibility.** One skip link, one polite live region for queue changes and
  one assertive region for problems, an accessible name on every focusable
  control, reading-order tab traversal, a real focus indicator, a modal
  shortcuts dialog that takes and returns focus, and status carried by text as
  well as colour.
- **Filename boundary.** A destination filename is rejected if it contains
  `< > : " / \ | ? *`, ends in a dot or space, names a reserved Windows device,
  or escapes the chosen folder. The colon rule closes an NTFS alternate data
  stream: `notes.txt:hidden` would otherwise satisfy the create-only
  publication fence while attaching bytes to a file the user never chose.
- **Unsigned interim distribution.** An optional Authenticode hook is wired into
  the bundle and is completely inert with no certificate configured.

## What was tested, and how

Everything below was produced by running the real artifacts on this machine.

```powershell
# Build the frontend, the optimized executable and both installers.
corepack pnpm --dir apps/desktop build
corepack pnpm --dir apps/desktop tauri build
corepack pnpm --dir apps/desktop tauri build --config '{"version":"0.1.1"}'

# Accessibility, against the optimized executable. Runs under Windows
# PowerShell 5.1 as well; both editions were exercised, from a clean state.
pwsh -NoProfile -File tests/compatibility/windows/ui-accessibility.ps1

# Real install, upgrade over a running instance, retained queue, uninstall.
pwsh -NoProfile -File tests/compatibility/windows/packaging-lifecycle.ps1

# Host-side checks.
cargo test -p fetchpath-desktop
cargo clippy -p fetchpath-desktop --all-targets -- -D warnings
cargo fmt --check
```

Recorded observations:

- [ui-accessibility.json](evidence/windows/ui-accessibility.json)
- [packaging-lifecycle.json](evidence/windows/packaging-lifecycle.json)

Both scripts throw on any failed assertion and still write their partial
observation if they abort, so a JSON file with `"passed": false` is a failure
record rather than a missing run.

### Keyboard input is delivered to the renderer window, not synthesised globally

The accessibility script delivers keystrokes straight to the
`Chrome_RenderWidgetHostHWND` window and reads focus from the
`HasKeyboardFocus` property of elements inside the renderer subtree.

This is deliberate, and the reason is recorded in the evidence: the foreground
window during a run belongs to another process - `foregroundWindowClass` in the
observation file - because nothing brings Fetchpath forward and an automated run
cannot rely on being the active session. `keybd_event` only reaches the
foreground window, so globally synthesised input is silently discarded, and
`AutomationElement.FocusedElement` reports whatever the rest of the desktop is
doing. Neither affects a message delivered to the renderer window by handle.

Two delivery routes are used, and they are not interchangeable:

- **`Tab` is sent** (`SendMessage`, bounded by `SendMessageTimeout`), so it
  reaches the window procedure without passing through the application's message
  pump. `TranslateMessage` therefore never synthesises a `WM_CHAR`, and focus
  moves with no character typed anywhere.
- **`Enter` and `Escape` are posted** (`PostMessage`), so the pump does translate
  them. `Enter` needs that: Blink activates a button on the `keypress` event, and
  a sent `Enter` opens neither the disclosure nor the shortcuts dialog. The
  character `Escape` translates to is `0x1B`, which Blink does not insert, so
  posting it leaves no text behind.
- **Text is posted as `WM_CHAR` per character**, one character at a time.

Two consequences, both real limitations:

- Modifier chords cannot be exercised by either route, because Chromium reads
  the real asynchronous key state for modifiers rather than the message. `Ctrl+L`
  is therefore asserted as a **declared** shortcut - UI Automation reports
  `AcceleratorKey = Control+L` on the address box - and not as a pressed one.
- A *posted* `Tab` moves focus **and** delivers a literal `WM_CHAR(0x09)`, and
  because the character arrives after focus has already moved, it is typed into
  the field the Tab moved into. That is an artefact of posting, not a defect of
  the application, and it is why `Tab` is sent rather than posted. An earlier
  revision of this script posted it and then pressed Escape to mop up, which
  made the whole keyboard journey depend on that clean-up landing; the journey
  now asserts instead that tabbing through the composer left both text fields
  empty, and Escape's clearing behaviour is asserted separately, on a composer
  that holds a real rejected address.

The script runs under both Windows PowerShell 5.1 and PowerShell 7; both were
exercised. The loopback fixture server it serves bytes from is part of the
harness, so its liveness is recorded next to the download result: a listener that
stopped serving would otherwise look exactly like a failed transfer.

## Accessibility checks and results

All assertions passed, on three consecutive runs from a clean state. The
observation file carries the full inventory and is rewritten by every run.

| Check | Result |
| --- | --- |
| Focusable controls in the renderer with an accessible name | 24 of 24 with an empty queue; `unnamedFocusableControls` is empty. The count is not fixed - each queue card adds two named row actions, so a run that starts with a retained queue enumerates more - so the assertion is on the unnamed list being empty, not on the total |
| Tab order from the document | skip link, Keyboard help, download type, address, destination, Choose, Advanced options, start time, date picker, Start when ready, Add to queue, search, then the five filters - all 17 steps in reading order |
| Advanced options opens from the keyboard | Enter on the `<summary>` reveals the start-time controls |
| Roles and names on the primary journey | address `Edit` "Download addresses one per line"; destination `Edit` "Save first item as"; `Button` "Choose a destination file"; `Button` "Add to queue"; `Button` "Keyboard help"; `Edit` "Search downloads"; `Button` "Start when ready…"; `Button` "Inspect link"; `ComboBox` "Quality"; `Group` "Download type"; `Group` "Filter downloads"; `Group` "Inspect available quality" |
| The queue figure reads as a sentence | `Group` named "No downloads are active." rather than the two loose runs "0" and "active" |
| `Ctrl+L` advertised to assistive technology | `AcceleratorKey = Control+L` |
| Tabbing through the composer types nothing | after the 17-step keyboard traversal, both the address box and the destination box are still empty, so the read-back below measures only what was typed |
| Keyboard-only primary journey | typed a loopback fixture address, read back byte-identical, Tab to the destination, typed the path, read back byte-identical, six Tabs to the submit button, Enter; reached **Complete** and published 262 144 bytes, with the fixture server still serving |
| Polite live region | announced "keyboard-only.bin finished downloading." |
| Queue card naming | `keyboard-only.bin, Complete` - each card's accessible name is its filename plus its state |
| Assertive live region | carried "javascript:alert(1) is not an HTTP or HTTPS address." |
| Error cleared | Escape emptied the composer, removed the error text from the tree, and left focus on a real control (`start-download`) |
| Queue filters | exactly one reports `ToggleState = On`; activating **Active** with Enter moved the pressed state, still exactly one |
| Modal shortcuts dialog | exposed as `Window` named "Keyboard shortcuts"; focus lands on **Close**; the page behind is **not** reachable (`url` disappears from the tree while modal); Escape closes it and returns focus to **Keyboard help** |
| Skip link | activating it moves focus to the `queue-title` heading |
| Stylesheet rules in the shipped bundle | `prefers-reduced-motion`, `forced-colors: active`, `:focus-visible`, `.skip-link`, the pressed-filter rule that is not colour-only (`3px double canvastext` under forced colors), `solid Highlight` focus under forced colors, and the screen-reader sentence excluded from `text-transform: uppercase` |
| The machine is left as it was found | the run's own application data and WebView2 profile directories are removed in a `finally`, so a failing run restores state too; `machineRestored.matchesPreflight` is asserted, and a pre-existing installation's data is recorded and left untouched |

Design decisions behind those results:

- **Two live regions, not many.** The renderer polls the queue several times a
  second. Every per-card `aria-live` was removed and the byte counter was
  explicitly silenced with `aria-live="off"`, because `<output>` is implicitly a
  polite region and progress changes constantly. Announcements are deduplicated
  and terminal transitions are announced once each.
- **The queue only re-renders when something changed.** Rebuilding identical
  cards on every poll churned the accessibility tree and stole keyboard focus, so
  the queue is now rebuilt only when a signature over the visible job fields
  changes. When the control a user was on disappears, focus moves to the queue
  heading rather than falling to the document body.
- **Row actions name their download.** "Retry" repeated across five cards is
  five identical accessible names; each row action is now labelled
  "<action>: <filename>".
- **Nothing is conveyed by colour alone.** Status pills carry their text label,
  the selected filter carries `aria-pressed` plus a weight change and an inset
  ring, and under forced colors it falls back to a double border.

## Install, upgrade and uninstall, as observed

Installer SHA-256 values are recorded in the observation file for the exact
artifacts that were run.

**Install** (`Fetchpath_0.1.0_x64-setup.exe /S`, 3.5 s):

- install directory `C:\Users\<user>\AppData\Local\Fetchpath`, confirmed under
  `%LOCALAPPDATA%`;
- one Apps & features entry at
  `HKEY_CURRENT_USER\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\Fetchpath`
  - DisplayName `Fetchpath`, DisplayVersion `0.1.0`, Publisher
  `Fetchpath contributors`, EstimatedSize 12 210 KB;
- installed files: `fetchpath-desktop.exe`, `fetchpath-browser-host.exe`,
  `THIRD-PARTY-NOTICES.md`, `uninstall.exe`;
- a Start menu shortcut and a desktop shortcut, both resolving to the
  installed `fetchpath-desktop.exe`, checked by reading each shortcut's target;
- `HKCU\SOFTWARE\Fetchpath contributors\Fetchpath` recording the install
  directory, which Tauri reads to restore a custom location on reinstall;
- `Get-AuthenticodeSignature` reports `NotSigned` for both the installer and
  the installed executable, which is the intended interim state.

One naming wrinkle worth knowing: Tauri names the main binary after the Cargo
bin target, so the installed executable is `fetchpath-desktop.exe`, not
`Fetchpath.exe`. Only the shortcuts and the Apps & features entry read
"Fetchpath". Nothing the user sees is wrong, and the shortcut targets were
checked rather than assumed.

**A running instance during upgrade.** The upgrade was deliberately run while
Fetchpath was running. The installer stopped the running instance itself
(`runningInstanceStoppedByInstaller: true`) and completed in 2.8 s with exit code
0. Afterwards there was still exactly **one** uninstall entry, now at version
`0.1.1` - an upgrade replaces rather than accumulates.

**Uninstall** (`uninstall.exe /S`, 2.6 s, run the way Apps & features runs it, so
the uninstaller relocates itself and can delete its own directory):

| Item | After a silent uninstall |
| --- | --- |
| `%LOCALAPPDATA%\Fetchpath` and every program file | removed |
| Start menu shortcut | removed |
| Desktop shortcut | removed |
| `HKCU\...\Uninstall\Fetchpath` | removed |
| `%APPDATA%\app.fetchpath.desktop\queue-v1.json` | **retained** |
| `%APPDATA%\app.fetchpath.desktop\instance.lock` | **retained** |
| `%APPDATA%\app.fetchpath.desktop` itself | **retained** |
| `%LOCALAPPDATA%\app.fetchpath.desktop\EBWebView`, the WebView2 profile | **retained** |
| `HKCU\SOFTWARE\Fetchpath contributors\Fetchpath`, the recorded install path | **retained** |

This is predictable rather than accidental, and it is a user choice (FP-100).
The interactive uninstaller's confirmation page carries a **"Delete the
application data"** checkbox, unticked by default and ignored when setup passes
`/UPDATE` (the in-app update path).
A silent uninstall (`/S`, what the script runs and what a deployment would use)
keeps all of the retained rows above. `/S /DELETEAPPDATA` is the same checkbox
ticked for a person who cannot click it; it is how the delete branch is tested.
Passed with `/UPDATE` it is ignored, as the checkbox is.

**What is Fetchpath-owned.** Inventoried from the code on 2 October 2026
(`EngineHome` in `crates/fetchpath-protocol/src/launch.rs`, `apps/cli/src/lan.rs`,
`crates/fetchpath-browser-inbox`). Every path below sits under one of two roots:

| Root | Contents | Removed by the choice |
| --- | --- | --- |
| `%APPDATA%\app.fetchpath.desktop` | queue and history (`queue-v1.json`), settings, rules, agent grants (`agents-v1.json`), engine secret and endpoint, torrent metadata snapshots and caches, content cache, browser inbox, `cli.toml` terminal preferences and history, `media-tools\` (yt-dlp, FFmpeg), instance and window locks | yes |
| `%LOCALAPPDATA%\app.fetchpath.desktop` | WebView2 profile (`EBWebView`), LAN pairing identity and pins | yes |
| `HKCU\SOFTWARE\Fetchpath contributors\Fetchpath` | recorded install folder | yes, with its empty parent |
| browser-host keys, per-user PATH entry, sign-in Run value, update hold | registrations | removed on every uninstall |
| anywhere the person saved a download, and the default Downloads folder | user content | never |
| `FETCHPATH_APP_DATA_DIR` | a data folder moved by environment variable, for development and tests | never: the uninstaller does not know it |

Nothing owned was found outside the two roots, so no folder is added to the
choice. The file names inside `%APPDATA%\app.fetchpath.desktop` were listed from code, not from a clean
install after every feature was used; the roots are what the removal relies on.

**Why the bundler's removal is not used.** Tauri's script deletes the two roots
with NSIS `RMDir /r`. That command follows junctions. Measured on 2 October 2026
with the NSIS 3.11 that Tauri bundles (a scratch `RMDir /r` over a folder holding a
junction to a folder with a sentinel file): the sentinel was deleted. (The NSIS
source agrees: `myDelete` in `exehead/util.c` recurses into every directory
without checking for a reparse point.) So a junction planted inside the data
folder would have turned the checkbox into deletion of its target.

**What runs instead.** `installer-hooks.nsh` takes the choice over:
`NSIS_HOOK_PREUNINSTALL` stops the engine (`FETCHPATH_STOP_ENGINE`), reads the
checkbox or `/DELETEAPPDATA`, copies `tools\remove-app-data.ps1` to the
uninstaller's plugin folder and clears the bundler's flag so its `RMDir /r` never
runs. `NSIS_HOOK_POSTUNINSTALL`, after the bundler has closed the app and removed the
program files, runs the script, then repeats the bundler's registry cleanup for
this choice. The script (Windows PowerShell 5.1 safe, .NET calls only):

- removes only `%APPDATA%\app.fetchpath.desktop` and `%LOCALAPPDATA%\app.fetchpath.desktop`, the exact leaf directly under the profile folder;
- refuses a root that is itself a junction or link, and says so;
- inside a root, deletes a reparse point as a link and does not enter it;
- keeps going after a locked file, lists each failure, and exits 3, which the
  uninstaller reports in its log and in a message box.

Covered by `tests/installer/data-removal.test.mjs`: the hook ordering, the single
choice, and the script run against scratch folders with a real junction inside a
root and as a root. `sandbox-lifecycle.ps1 -DeleteData` adds the whole path in
Windows Sandbox (see the limits below).

**Older uninstallers.** An interactive "uninstall first" reinstall runs the
previously installed uninstaller, such as 0.1.0, with its own checkbox and the
bundler's original cleanup, which does not have the junction handling above.
The protection applies from the version that carries this change onward.

**Policy.** Group Policy that blocks PowerShell scripts (execution policy) or
puts it in Constrained Language Mode makes the removal script fail. That is
reported in the uninstall log and a message box, exit level 3, and the data is
kept. The bundled engine stop and PATH edit use the same PowerShell route.

Setup also ends every program running from the install folder (browser host,
torrent and media helpers, the app) before removal, and the script deletes the
update hold last.

Keeping is the default: the queue is the user's own record of what they
downloaded and what still needs attention. What the retained rows are:

- `queue-v1.json` holds the download history, including redacted source URLs;
- the WebView2 profile holds the webview's own cache and storage;
- the retained registry value still points at the now-deleted install
  directory. Tauri reads it to restore a custom install location on reinstall,
  which is why it is tied to the same checkbox rather than removed always.

Manual removal is `%APPDATA%\app.fetchpath.desktop`, `%LOCALAPPDATA%\app.fetchpath.desktop` and
`HKCU\SOFTWARE\Fetchpath contributors`. Not exercised on a clean image
yet: the ticked branch and `/DELETEAPPDATA` through a real uninstaller, the
interactive checkbox, and a locked file during removal.

## Retained queues across a real upgrade

Three real jobs were seeded through the installed application's own user
interface before the upgrade, using loopback fixtures only:

1. a completed 256 KiB download from a local HTTP fixture;
2. a failed download from a query-free address on a closed loopback port;
3. a failed download whose address carried `?token=fp017-private-query-value`.

Observed:

- `queue-v1.json` held three records and did **not** contain the private query
  value before the upgrade;
- after the upgrade the file was **byte-identical** - the installer does not
  touch application data;
- after relaunch the three cards read `retained-complete.bin, Complete`,
  `retained-failed.bin, Needs attention` and `retained-private.bin, Link needed`;
- the private-source job therefore requires an explicit refreshed link after
  restore, matching the `queue_persistence_recovers_safe_sources_and_requests_private_source_refresh`
  host test, and the private query value appeared nowhere in the queue file or in
  any accessible name in the restored interface.

## Native compatibility

Baseline is Microsoft-serviced Windows 11 x64 only. ARM64 is explicitly
deferred.

| Property | Observed |
| --- | --- |
| WebView2 runtime | Evergreen 153.0.4234.48, already installed; the installer is configured `webviewInstallMode: downloadBootstrapper` (silent), so Microsoft's bootstrapper is fetched only when the runtime is absent |
| Process DPI awareness | `2` (per-monitor); tao calls `SetProcessDpiAwarenessContext` at startup - the executable carries no `dpiAware` manifest entry, and `SetProcessDpiAwarenessContext` is present in its import strings |
| Effective window DPI | 144 (150% scaling) on this display |
| Top-level window class | `Tauri Window` |
| Close to tray | closing the window keeps the process alive with the window hidden |
| Second launch | exits with code 0, leaves the first instance running, and brings its hidden window back; `Get-Process Fetchpath` count returned to 0 afterwards |

The single-instance guard is a deny-sharing handle on
`%APPDATA%\app.fetchpath.desktop\instance.lock`, in the same directory as the
queue. It fails **open**: only `ERROR_SHARING_VIOLATION` or
`ERROR_LOCK_VIOLATION` means "another instance is running", and any other error -
an unwritable directory, a missing `%APPDATA%` - lets the application start,
because refusing to launch is worse than the unlikely double launch it would
prevent. Activation of the existing window matches on the window class **and**
the title, so an unrelated window called "Fetchpath" - an Explorer window on a
folder of that name, for instance - is never activated instead.

DPI awareness was read from the live process rather than inferred; what was not
done is a physical drag between monitors of different scale factors.

## Distribution, signing and SmartScreen

**Decision: unsigned interim builds, distributed as a direct download.** No
certificate is obtained, generated or installed by this repository.

An optional signing hook is wired in and is inert. `tauri.conf.json` sets
`bundle.windows.signCommand` to
`powershell -NoProfile -ExecutionPolicy Bypass -File tools/sign-windows.ps1 %1`,
and `apps/desktop/src-tauri/tools/sign-windows.ps1` prints one line and exits 0
when `FETCHPATH_SIGN_THUMBPRINT` is unset. Both bundle builds above ran with the
hook active and unsigned; the build log shows
`fetchpath sign hook: FETCHPATH_SIGN_THUMBPRINT is not set; leaving this binary
unsigned.` once per binary, including the NSIS uninstaller and each NSIS plugin
DLL. To enable signing, set `FETCHPATH_SIGN_THUMBPRINT` to the SHA-1 thumbprint
of a certificate already in a Windows certificate store, and optionally
`FETCHPATH_SIGNTOOL`, `FETCHPATH_SIGN_TIMESTAMP_URL` and `FETCHPATH_SIGN_STORE`.

Two NSIS constraints shaped that command, and both were reproduced during this
task. Tauri writes the sign command into the generated installer script as
`!uninstfinalize '<command>'`, a preprocessor directive that does not apply NSIS
string escaping:

- an apostrophe anywhere in the command terminates the directive's argument, and
  the bundle fails with `!uninstfinalize expects 1-3 parameters, got 28`;
- a `$` reaches the shell doubled, so `$hook` arrives as `$$hook` and the
  command becomes a PowerShell parse error.

The command therefore carries no quotes, no `$` and no inline script. The binary
path consequently arrives unquoted, so a repository or output path containing
spaces needs this reworked before signing is enabled.

### What unsigned means for the person downloading it

- **Microsoft Defender SmartScreen, app reputation.** An unsigned executable
  downloaded from the web carries the Mark of the Web and has no reputation, so
  SmartScreen shows the blue "Windows protected your PC" dialog. The **Run
  anyway** button is behind **More info**, which is exactly the kind of step a
  nontechnical user reads as "this is unsafe". Reputation is built per binary, so
  every new release starts over.
- **SmartScreen in Edge, download stage.** The download itself can be reported as
  unrecognised and may need an explicit **Keep** before the file is even saved.
- **Microsoft Defender Antivirus.** Unsigned installers are more likely to be
  sent for cloud analysis and are more exposed to heuristic false positives. Not
  being signed is not by itself detection, but it removes the strongest signal
  that would rule it out.
- **Enterprise policy.** WDAC, AppLocker and Smart App Control rules commonly
  require a signature or a publisher. An unsigned per-user installer is a likely
  block in a managed environment.

### What a certificate would change

- **OV (organisation validation) code signing certificate.** Names a verified
  publisher in the UAC and SmartScreen dialogs and in file properties. It does
  **not** grant instant SmartScreen reputation: warnings persist until enough
  installs accumulate, though reputation then attaches to the publisher rather
  than to each binary.
- **EV (extended validation) code signing certificate.** Hardware-backed key
  and usually higher cost. Microsoft says EV no longer grants automatic
  SmartScreen reputation, so it is not a shortcut around first-download
  warnings.
- **Artifact Signing (formerly Trusted Signing).** A Microsoft-operated
  signing service with short-lived certificates and no key material to hold.
  It is CI-friendly but needs an eligible Azure subscription and identity
  validation; Public Trust eligibility depends on publisher location and type.

Whichever is chosen, `sign-windows.ps1` is where it plugs in, and the bundle
build then needs a verified signing integration for that provider. The current
hook supports a certificate in a Windows store; a service-backed signer needs
its own integration and validation.
Until then the honest position is: this is an unsigned build, SmartScreen will
warn, and that warning is correct.

## Helper and licensing attribution

Fetchpath's own code is **MIT OR Apache-2.0**, from `[workspace.package]` in the
repository root `Cargo.toml`. `THIRD-PARTY-NOTICES.md` ships beside the
executable and was confirmed present in the installed directory.

What is actually redistributed was determined from this repository, not assumed:
`Cargo.lock`, the `curl` features in `crates/fetchpath-core/Cargo.toml`, the
absence of `bundle.externalBin` and helper `bundle.resources` entries in
`tauri.conf.json`, and printable strings read out of the compiled binaries.

**Statically linked into the Fetchpath binary**, via
`curl = { features = ["static-curl", "http2"] }`:

| Library | Version | Evidence | License |
| --- | --- | --- | --- |
| libcurl | 8.21.0 | `curl-sys 0.4.90+curl-8.21.0` | curl license (MIT/X derivative) |
| nghttp2 | 1.68.1 | `libnghttp2-sys 0.1.13+1.68.1` | MIT |
| zlib | inflate 1.3.2 | `libz-sys 1.1.29`; the binary contains `inflate 1.3.2 Copyright 1995-2026 Mark Adler` | zlib license |

**TLS is Schannel**, the stack Windows itself provides, and is not
redistributed. The evidence is direct rather than inferred:
`fetchpath-browser-host.exe`, which links `fetchpath-core` and libcurl but no
webview, contains 91 distinct `schannel:` diagnostic strings from libcurl's
Schannel backend and **no** OpenSSL runtime symbols or version banner.
`openssl-src`, the crate that would vendor and compile OpenSSL, does not appear
in `Cargo.lock` at all; `openssl-sys` and `openssl-probe` are present only as
platform-gated dependencies of `curl`.

**Not redistributed:**

- **Edge WebView2 Runtime** - Microsoft-supplied and Microsoft-serviced. The
  installer downloads Microsoft's bootstrapper when the runtime is missing and
  embeds no Microsoft binary.
- **yt-dlp, FFmpeg and ffprobe** - supervised as external child processes and
  never bundled. `tauri.conf.json` has no `externalBin` entry and no
  `media-tools` resource, and `adapters/media` discovers them from
  `FETCHPATH_YT_DLP` plus `FETCHPATH_FFMPEG_DIR`, from
  `FETCHPATH_MEDIA_TOOLS_DIR`, or from a `media-tools` directory an operator
  places beside the executable. Interactive NSIS setup now offers a default-off
  choice that calls the same pinned, checksum-verified `fetchpath tools install
  --yes` command used by Settings. Silent and passive installs skip it.

  This is a licensing boundary, not an accident. If a future release ships
  FFmpeg inside the installer, FFmpeg's LGPL-2.1-or-later terms - or GPL terms,
  depending on which components that build enables - begin to apply to the
  distribution, alongside yt-dlp's Unlicense notice. That decision has not been
  made and must not be made implicitly by dropping binaries into `media-tools`.

## Current limits

On 29 September 2026, an owner's clean Windows Sandbox run exposed a missing
`VCRUNTIME140.dll` in the torrent helper. The helper build now links the MSVC
runtime statically and the package check reads its PE imports. The rebuilt
installer (SHA-256 `d1a90be5...596f`) then passed the automated clean-Sandbox
lifecycle without the Visual C++ redistributable present, including a helper
start ([evidence](evidence/windows/sandbox-lifecycle.json)), and the owner's
interactive Sandbox pass of the setup choices, torrent intake and Settings
window found no defect (FP-079 to FP-082).

Interactive NSIS setup offers two default-off steps: media tools through the
pinned `fetchpath tools install`, and browser extension guidance that opens the
bundled extension folder and shows Chrome/Edge's Load unpacked steps; browsers
must confirm the extension themselves because there is no store listing or ID
yet. Silent and passive setup skip both, and Settings remains the later setup
path. Once the media download starts, Setup's Cancel cannot stop it; it
finishes or reports a failure and points to Settings. The hook does not clear
the NSIS error flag before opening the extension folder, so an earlier error
could log a false "could not open" line; fix with the next installer change.
The Settings window has one scroll area with category navigation and a project
link.

- **No screen reader was driven end to end.** Every accessibility assertion was
  made through UI Automation, which is the tree Narrator reads, but Narrator,
  JAWS and NVDA were not run and their announcement wording was not heard. Roles,
  names, focus movement, modal containment and live-region content are verified;
  what a specific screen reader says out loud is not.
- **`Ctrl+L` was not pressed.** It is verified as declared
  (`AcceleratorKey = Control+L`) rather than exercised. Chromium reads the real
  asynchronous key state for modifiers, so a chord cannot be delivered by a sent
  or posted message; a globally synthesised one would need Fetchpath to hold the
  foreground, which an automated run cannot rely on. `Tab`, `Escape` and `Enter`,
  which need no modifier, were exercised.
- **`aria-invalid` and `aria-describedby` could not be read back.** Chromium
  reports an empty `AriaProperties`, and the .NET UI Automation client in
  PowerShell exposes neither `IsDataValidForForm` nor `FullDescription` nor
  `LegacyIAccessiblePattern`. The invalid state and the field/error association
  are in the markup, and the error text is verified to reach the assertive live
  region and the accessibility tree, but the association itself is unverified by
  automation.
- **Fresh-image coverage is incomplete.** Earlier installers were exercised on
  this developer machine, which already had WebView2. The owner's September
  Sandbox run found the torrent helper's missing runtime; the rebuilt package
  passed a repeat run. The WebView2 bootstrapper path and ARM64 remain open.
- **One display, one scale factor.** DPI awareness was read from the live
  process; no physical move between monitors of different scale was performed.
- **Per-monitor visual review is not automated.** Reduced motion, forced colors
  and light mode are asserted as rules present in the shipped stylesheet, not as
  rendered screenshots.
- **The delete-data uninstall was not run on an installed copy.** The hook and
  script compile into the installer (built 2 October 2026) and the script is
  tested on scratch folders, but `sandbox-lifecycle.ps1 -DeleteData` and the
  interactive checkbox are still to be run in a clean Sandbox.
- **No generated per-crate license manifest.** The natively compiled C libraries
  are identified precisely; a machine-generated license list for the whole Rust
  dependency graph of the shipping binary has not been produced, and anything in
  it that is not MIT/Apache-2.0/BSD/zlib would need review. This is an open
  release gate.
- **Updates are manual.** There is no updater: a new version is a new installer
  run over the old one, which is what was tested. `bundle.windows.allowDowngrades`
  is `false`, so a lower version will not silently replace a higher one.
