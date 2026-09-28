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

This is predictable rather than accidental, and it is a user choice. The
interactive uninstaller's confirmation page carries a **"Delete the application
data"** checkbox. When it is ticked, the uninstaller additionally removes
`%APPDATA%\app.fetchpath.desktop`, `%LOCALAPPDATA%\app.fetchpath.desktop` and
`HKCU\SOFTWARE\Fetchpath contributors`. A silent uninstall - `/S`, which is what
the script runs and what an automated deployment would use - leaves the checkbox
unticked, so all of the retained rows above are kept.

Keeping them by default is the right default: the queue is the user's own record
of what they downloaded and what still needs attention. Note what the retained
rows actually are, though, so nobody is surprised:

- `queue-v1.json` holds the download history, including redacted source URLs;
- the WebView2 profile holds the webview's own cache and storage;
- the retained registry value still points at the now-deleted install
  directory. Tauri reads it to restore a custom install location on reinstall,
  which is why it is tied to the same checkbox rather than removed always.

Manual removal is `%APPDATA%\app.fetchpath.desktop`,
`%LOCALAPPDATA%\app.fetchpath.desktop` and
`HKCU\SOFTWARE\Fetchpath contributors`. The checkbox state cannot be set from the
command line, so the ticked branch was **not** exercised by the automated run;
only its presence in the generated installer script was confirmed.

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
- **EV (extended validation) code signing certificate.** Hardware-backed key,
  and historically the fastest route to SmartScreen reputation - often
  immediately. Higher cost, and the key must live in an HSM or token, which
  complicates automated builds.
- **Azure Trusted Signing.** A Microsoft-operated signing service with
  short-lived certificates and no key material to hold. Cheaper than EV and
  CI-friendly. Its reputation behavior is closer to OV than to EV, and it
  requires an eligible Azure subscription and identity validation.

Whichever is chosen, `sign-windows.ps1` is where it plugs in, and the bundle
build then produces a signed installer with no other configuration change.
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
  places beside the executable. Nothing in this repository creates that
  directory.

  This is a licensing boundary, not an accident. If a future release ships
  FFmpeg inside the installer, FFmpeg's LGPL-2.1-or-later terms - or GPL terms,
  depending on which components that build enables - begin to apply to the
  distribution, alongside yt-dlp's Unlicense notice. That decision has not been
  made and must not be made implicitly by dropping binaries into `media-tools`.

## Current limits

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
- **No clean-machine or ARM64 run.** Both installers were exercised on this
  developer machine, which already had WebView2 present, so the WebView2
  bootstrapper path was never taken. A first-install-on-a-fresh-image run and any
  ARM64 coverage remain open.
- **One display, one scale factor.** DPI awareness was read from the live
  process; no physical move between monitors of different scale was performed.
- **Per-monitor visual review is not automated.** Reduced motion, forced colors
  and light mode are asserted as rules present in the shipped stylesheet, not as
  rendered screenshots.
- **The uninstaller's data-removal checkbox was not exercised.** Its presence in
  the generated installer script was confirmed and its effect is documented, but
  there is no command-line switch for it, so only the silent, data-retaining
  branch was actually run.
- **No generated per-crate license manifest.** The natively compiled C libraries
  are identified precisely; a machine-generated license list for the whole Rust
  dependency graph of the shipping binary has not been produced, and anything in
  it that is not MIT/Apache-2.0/BSD/zlib would need review. This is an open
  release gate.
- **Updates are manual.** There is no updater: a new version is a new installer
  run over the old one, which is what was tested. `bundle.windows.allowDowngrades`
  is `false`, so a lower version will not silently replace a higher one.
