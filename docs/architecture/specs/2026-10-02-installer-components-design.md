# Installer components: Full and Custom (FP-098)

Status: proposed, 2 October 2026; implemented by FP-099. Builds on the
[engine platform design](2026-09-24-engine-platform-design.md), the
[design system](2026-10-01-design-system-design.md) and the FP-100 uninstall
choice in [WINDOWS-PACKAGING.md](../../development/WINDOWS-PACKAGING.md). A01, A12, A13.

## What exists today (measured 2 October 2026)

One per-user NSIS (MUI2) package from Tauri's generated template
(`target/release/nsis/x64/installer.nsi`) plus `installer-hooks.nsh`. Pages:
Welcome, Reinstall (when installed), Directory, Start menu, Install files, Finish;
uninstall Confirm carries the FP-100 data checkbox. One `Install` section plus
`WebView2`; no components page, no `InstType`. Switches: `/S`, `/P`, `/NS`,
`/UPDATE`, `/R`, `/ARGS`; uninstall adds `/DELETEAPPDATA`. Hooks are included
before any page and expanded inside the one section, so they cannot add a page
after Welcome, skip the main binary or skip WebView2. `MAINBINARYNAME`
(`fetchpath-desktop`) is hard-coded for shortcuts, running-app check,
`DisplayIcon` and "Run Fetchpath".

Release build, `target/release` and the staged `binaries/`:

| File | Bytes | MiB | Role |
|---|---:|---:|---|
| `fetchpath.exe` | 11 234 304 | 10.7 | engine, CLI, TUI, MCP (one binary, `apps/cli`) |
| `fetchpath-desktop.exe` | 13 917 696 | 13.3 | desktop UI (WebView2) |
| `fetchpath-torrent-helper.exe` | 11 131 904 | 10.6 | torrent sessions, spawned by the engine |
| `fetchpath-browser-host.exe` | 2 644 480 | 2.5 | native messaging host |
| extension folder + two host manifests | 29 073 | <0.1 | browser integration |
| licences, notices, guides, `tools/*.ps1` | 141 713 | 0.1 | core resources |
| `Fetchpath_0.1.0_x64-setup.exe` (lzma solid) | 9 499 082 | 9.1 | the download, same for every selection |

Installed program files total 39.1 MB (37.3 MiB). Media tools (yt-dlp, FFmpeg
essentials) are never bundled: they are downloaded on request into
`%APPDATA%\app.fetchpath.desktop\media-tools` from the pins in
`adapters/media/media-tools.json`. Their download size was not measured here.

From code: desktop and browser host launch the `fetchpath.exe` beside them
(`engine_link.rs`, `browser_bridge.rs`); the host starts the desktop only if its
exe exists; a missing helper gives `torrent.helper_unavailable`
(`adapters/torrent/src/lib.rs`); the person configures MCP hosts
(`docs/user/CLI.md`); approvals exist only in desktop, TUI and `fetchpath approvals`.

## Decisions

1. **No split of `fetchpath.exe`.** Selection controls which separable files are
   copied (desktop, browser host, torrent helper) and which entry points exist
   (shortcuts, PATH, native-host keys, guidance). Engine, CLI, TUI and MCP stay
   one 10.7 MiB binary in Core. A split saves only part of that, not the
   download, and changes every client's launch contract. See the gate.
2. **Full** = every component compiled into *this* build (Core, Desktop,
   Terminal, MCP, Browser integration, Torrent helper), equal to today. Planned
   components (Web) are not compiled in, so cannot be selected.
3. **Usable interface rule.** Desktop or Terminal is installed. MCP and future
   Web serve an agent or a tab, not the person who approves and manages. MCP
   without Desktop auto-selects Terminal (0 extra bytes, same binary).
4. **Headless is real.** Without Desktop: no `fetchpath-desktop.exe`, no WebView2
   section, a "Fetchpath Terminal" Start entry, no "Run Fetchpath" (decision 9).
5. **Optional modules** are separate from interfaces: Browser integration,
   Torrent helper (both bundled), Media tools (download on request, default off,
   never part of Full, never in a quiet install).
6. **Selections persist** in `HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\Fetchpath`:
   `FetchpathInstallType` (REG_SZ `full` or `custom`), `FetchpathComponents`
   (REG_SZ, canonical sorted names, e.g. `browser,cli,core,desktop,mcp,torrent`)
   and `FetchpathComponentsSchema` (DWORD 1). They live and die with the
   uninstall entry. `full` means "whatever the running build offers", so Full
   picks up components added later; `custom` keeps the exact list. No values
   (every install before FP-099) reads as `full`, which is what it has. A
   malformed value, an unknown name, schema above 1 or a stored set with no
   interface is never fatal: setup derives the selection from files on disk
   (desktop exe, browser host, helper; Terminal and MCP assumed when the PATH
   entry exists, Terminal if no interface is found), logs why, and continues.
7. **Change later by rerunning setup.** The reinstall page gains "Change
   components" (preselected) next to reinstall/uninstall. No installer copy is
   kept and `NoModify` stays 1. Settings and `fetchpath engine status` show the
   version and the installed components read-only with "download Fetchpath setup
   again to change them". Media tools stay in Settings and `fetchpath tools`.
   There is no in-app updater, headless or not: an update is a setup rerun, and
   the same two surfaces show the installed version to compare.
8. **Installing is never consent.** No component grants an agent, writes an agent
   host's configuration, starts serving, enables sharing, LAN, seeding or
   sign-in start, or downloads a tool. Each of those stays a separate choice made
   in the app, the CLI or the explicit media checkbox.
9. **Own the template.** `bundle.windows.nsis.template` points to
   `apps/desktop/src-tauri/installer.nsi`, a fork of Tauri's template for the
   pinned `tauri-bundler` version, with sections per component and the pages
   below. A test regenerates Tauri's template and fails when upstream changed
   since the fork was taken, so a Tauri upgrade forces a reviewed re-merge.
   Hooks stay where they are and become section-aware. Each `MAINBINARYNAME`
   use in the generated script is rekeyed:

| Line (generated) | Use | Keyed on |
|---|---|---|
| 364 | reinstall page: "is it still installed" after the old uninstaller | `fetchpath.exe` |
| 416 | Finish "Run Fetchpath" | Desktop selected |
| 637, 780 | `CheckIfAppIsRunning` on install and uninstall | replaced by `FETCHPATH_STOP_ENGINE` (all exes in the folder) |
| 691-696 | `MainBinaryName` value and old-name cleanup | `fetchpath.exe` |
| 700 | `DisplayIcon` | `fetchpath.exe` (carries the icon) |
| 749 | `/R` auto-launch after passive, silent or update | Desktop selected; otherwise no launch |
| 784 | uninstall delete of the main exe | each component's own files |
| 831-846, 917-978 | shortcut checks, migration, creation | Desktop selected; Terminal entry targets `fetchpath.exe` |

## Components

| Component | Required | Copies | Registers | Needs |
|---|---|---|---|---|
| Core (engine) | always, read-only | `fetchpath.exe`, resources, `uninstall.exe` | uninstall entry, `FetchpathComponents` | none |
| Desktop app | interface | `fetchpath-desktop.exe` (13.3 MiB) | Start menu + desktop shortcut, WebView2 check | WebView2 (OS) |
| Terminal (CLI and TUI) | interface | nothing extra | per-user PATH entry; Start "Fetchpath Terminal" when no Desktop | Core |
| AI agents (MCP) | interface for agents | nothing extra | PATH entry (shared with Terminal); Finish shows setup snippets | Desktop or Terminal |
| Web (future, FP-092) | not in this build | | | see prerequisites |
| Browser integration | module | browser host (2.5 MiB), extension folder | three HKCU native-host keys | Core; works headless |
| Torrent helper | module | helper (10.6 MiB) | none | Core |
| Media tools | module, default off | nothing in program files | none | Core, internet, the person's yes |

Ownership: Core owns `fetchpath.exe` and the uninstaller; PATH exists while
Terminal or MCP is selected; native-host keys belong to Browser integration; the
sign-in Run value belongs to the engine setting (removed on every uninstall);
`media-tools\` belongs to the data folder and the FP-100 choice. One install
folder, one uninstaller, one engine owner; no second engine, no service.

## Prerequisites

- **MCP-only.** Not allowed: MCP has no human UI, and requests outside a grant
  wait in `awaiting_approval` for a `user`. The auto-added Terminal brings
  `fetchpath agents grant`, `fetchpath approvals` and the TUI at no cost.
- **Web-only (future).** Selectable only after FP-092, and needs Desktop or
  Terminal until FP-092 proves pairing, approval, grants and recovery work in
  the local web client alone. Installing Web never starts serving; loopback by
  default, no public-network exposure implied.
- **Browser integration headless.** Captures go to the engine inbox and appear in
  the TUI and `fetchpath list`; the host does not try to start a missing desktop.
- **Torrent helper absent.** Torrent links fail with the existing
  `torrent.helper_unavailable` message, which FP-099 points at "run setup and
  add Torrent helper". Installing the helper enables no seeding.

## Pages (interactive)

Welcome → Reinstall (when installed) → **Installation type** → **Components**
(Custom only) → Directory → Start menu → Install files → Finish.

- **Installation type**: two radio rows, the first focused and selected.
  "Full (recommended): the app, the terminal command, AI agent support, browser
  integration and the torrent helper. About 37 MiB." "Custom: choose what to
  install." Then an unticked checkbox replacing today's media MessageBox: "Also
  download video and audio tools (yt-dlp, FFmpeg) from their publishers now.
  Third-party licences apply. You can do this later in Settings." Extension
  guidance stays, only when Browser integration is installed.
- **Components**: `MUI_PAGE_COMPONENTS`, Core `SectionIn RO`, groups "Ways to
  use Fetchpath" and "Optional modules", NSIS per-section sizes, one-sentence
  descriptions (MCP: "Agents get no access until you grant it.").
  `.onSelChange` applies the MCP rule; leave blocks Next with "Choose the app or
  the terminal so you can manage Fetchpath." when no interface remains.
- **Finish**: text depends on selection (terminal hint only with Terminal, MCP
  snippet link only with MCP); "Run Fetchpath" only with Desktop.

## Quiet and passive install

- `/S` and `/P` with no new switch: fresh machine installs Full; an existing
  install keeps its stored type and list. Never the media download, never the
  browser guidance (as today).
- `/COMPONENTS=desktop,cli,mcp,browser,torrent` (core implicit, order free) sets
  a `custom` selection; `/COMPONENTS=full` sets `full`. Unknown names, `web` in a
  build without Web, no interface, or MCP without Desktop or `cli` (no quiet
  auto-add): setup logs the reason and exits with **code 10** ("invalid
  component selection", documented in the user guide) before touching files.
  Code 2 stays NSIS's own Abort and 1 its cancel.
- `/NS` keeps its meaning (no shortcuts). No quiet switch downloads media tools;
  a deployment runs `fetchpath tools install --yes` itself afterwards.
- `/UPDATE` keeps the stored selection, ignores `/COMPONENTS` and removes nothing.

## Upgrade, repair, change, uninstall

- **Every install except `/UPDATE`** (upgrade, repair, change, quiet with
  `/COMPONENTS`) first stops everything running from the install folder
  (`FETCHPATH_STOP_ENGINE`), then removes the difference between the stored (or
  derived) selection and the new one, then installs the new one and writes the
  values. Removal per component: Desktop (exe, Start and desktop shortcuts;
  WebView2 and the `EBWebView` profile stay, the profile under the FP-100
  choice), Browser integration (host, extension folder, three keys), Torrent
  helper (exe), Terminal/MCP (PATH entry once neither remains; Terminal entry).
  Data, grants, history and media tools are never touched.
- **Upgrade** preselects the stored selection; `full` gains new components,
  `custom` does not. **Repair** reinstalls the stored selection.
- **Uninstall** is unchanged: everything in the install folder and every
  registration goes, data follows the FP-100 checkbox / `/DELETEAPPDATA`.
- **Honest limits.** Deselecting MCP or Terminal removes entry points, not the
  capability: an agent host configured with the full path still runs
  `fetchpath.exe mcp` under the same grants and approvals (revoke with `fetchpath
  agents revoke` or Settings > Agent access). One configured with the bare
  `fetchpath` command stops starting once the PATH entry goes: it fails closed,
  and `docs/user/CLI.md` gains the full-path form as the recommended setup.

## Selection and dependency matrix

| Case | Files | Shortcuts / PATH / keys | Approvals by | Disk | Result |
|---|---|---|---|---:|---|
| Desktop only | core, desktop | app shortcuts; no PATH; no keys | desktop | 24.1 MiB | valid |
| Terminal only | core | Terminal Start entry, PATH | TUI, `fetchpath approvals` | 10.8 MiB | valid, no WebView2 check |
| MCP only (interactive) | core | PATH, Terminal entry (auto-added) | TUI, CLI | 10.8 MiB | becomes MCP + Terminal |
| MCP only (`/COMPONENTS=mcp /S`) | none | none | | | exit 10, nothing changed |
| Web only (future) | core, web | none, serving off | requires Desktop or Terminal until FP-092 proves otherwise | n/a | blocked in this build |
| Full | all bundled | app shortcuts, PATH, keys | desktop, TUI, CLI | 37.3 MiB | valid, equals today |
| Custom, no interface | | | | | Next blocked with reason |
| Quiet fresh `/S` | as Full | as Full | | 37.3 MiB | no download, no guidance |
| Upgrade from a pre-FP-099 full install | all bundled | unchanged | unchanged | 37.3 MiB | no values → `full`, written after |
| Stored value unreadable | from disk | from disk | as derived | | logged, never aborts |

Disk figures are program files only; the download is 9.1 MiB in every case.

## Design system inside a native installer

NSIS dialogs use system controls, colours and fonts, ignore dark mode and follow
Windows high contrast by themselves; tokens cannot style them. We control, via
Tauri's existing keys: `installerIcon`, `headerImage` (150×57 BMP) and
`sidebarImage` (164×314 BMP), drawn from the `design/tokens.css` light surface
with the brand mark and trail motif. The script is `PerMonitorV2` DPI aware, so
FP-099 checks the bitmaps at 150% and 200%. Wording follows the design system's
checkbox row: label, then one consequence sentence. English only, as today.

## FP-099 packets

1. **Template fork and guard** (`apps/desktop/src-tauri/installer.nsi`,
   `tauri.conf.json`, `tests/installer/template-fork.test.mjs`): fork unchanged,
   prove same pages and files, add the guard test.
2. **Sections, pages, quiet, persistence** (`installer.nsi`, `installer-hooks.nsh`,
   `tests/installer/components.test.mjs`): per-section files and registrations,
   both pages, MCP rule, no-interface block, `/COMPONENTS` and exit 10, stored
   values and disk derivation, media checkbox, conditional PATH and native-host
   hooks, the `MAINBINARYNAME` rekeying table.
3. **Difference removal** (`installer.nsi`, hooks): reinstall option, removal on
   every non-`/UPDATE` install, `/UPDATE` keeps the selection; CLI.md full-path
   MCP setup.
4. **App surfaces** (desktop Settings, `fetchpath engine status`, the
   `torrent.helper_unavailable` wording): read-only installed components.
5. **Evidence** (`WINDOWS-PACKAGING.md`): Sandbox run per matrix row, media
   download sizes, fix its stale "no `externalBin`" text. Strong review.

## Lead defaults (the owner may reverse)

- Component selection controls entry points only. An engine-level "allow AI
  agents" switch, if wanted, is a separate runtime-policy task and is never set
  by the installer.
- No kept installer copy; Settings shows the read-only list and "download setup
  again"; `NoModify` stays.

## Gate: splitting `fetchpath.exe`

Not part of FP-099. Open a separate task only with a measured build showing the
split saves at least 5 MiB for a realistic selection, or a concrete security
reason that requires MCP or TUI code to be absent from disk. Such a task must
keep one engine owner, keep `fetchpath.exe` as the name every client launches,
and update the launch contract, the stage script and this spec together.
