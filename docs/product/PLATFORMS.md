# Other desktop platforms and mobile

FP-025, scoped 28 September 2026. Fetchpath 0.1.0 is Windows 11 x64 only.
This page says what would have to be built and shown before any other
platform is claimed. It is scope, not a plan with dates, and nothing here is
supported until its evidence exists.

## What already travels

The engine is plain Rust with no Windows call in its logic: `fetchpath-core`
(transfers, checkpoints, publication), `fetchpath-http`, `fetchpath-storage`,
`fetchpath-cache`, `fetchpath-metalink`, the protocol types and schema, the
session (queue, rules, policy, ledger) and the providers adapter. The desktop
interface is web technology inside Tauri 2, which also targets macOS and
Linux. curl is built statically. These are necessary, not sufficient: they
have never been compiled, tested or run anywhere but Windows.

## What is Windows-only today

| Area | Where | What another platform needs |
| --- | --- | --- |
| Engine transport | `fetchpath-protocol/src/pipe` (named pipe, owner SID, DACL) | A Unix domain socket in a private per-user directory (mode 0700), with the peer's user checked (`SO_PEERCRED`, `getpeereid`), and the same framing and secret handshake |
| Secret sealing | `adapters/lan/src/protect.rs`, `fetchpath-browser-inbox` (DPAPI) | macOS Keychain; on Linux the Secret Service, with a stated fallback when no keyring runs (refuse, never store in clear) |
| Launch, single owner, sign-in start | `launch.rs`, `engine/signin.rs` (Run key) | launchd user agent; systemd user unit or XDG autostart; the same lock semantics |
| Setup | NSIS hooks, PATH editing, per-user install, upgrade hold | `.dmg` with notarization; AppImage, `.deb`/`.rpm` or Flatpak, each with its own upgrade and uninstall story, and the engine stopped safely across them |
| Browser host registration | HKCU `NativeMessagingHosts` keys | JSON manifests in each browser's per-user directory, per platform |
| Folders and time | Known Folder for Downloads, `SYSTEMTIME`, `%APPDATA%`/`%LOCALAPPDATA%` | XDG user directories and base directories; `~/Library` on macOS |
| File publication | create-only fence, Windows reserved names, ADS colon | The fence on `link`/`renameat2(RENAME_NOREPLACE)`/`renamex_np`; different forbidden names; case-sensitive file systems |
| Media helpers | pinned `yt-dlp.exe`, ffmpeg Windows build, no console window | Pinned per-platform builds with their publishers' checksums; Gatekeeper quarantine on macOS |
| Accessibility checks | UI Automation harnesses | VoiceOver and AT-SPI walkthroughs, recorded |
| Code signing | Authenticode hook (inert) | Apple Developer ID and notarization are effectively required on macOS |

## Evidence before a claim, per platform

Each platform is its own release gate, with the acceptance matrix re-run on
it: install, first download, pause and resume across restarts, upgrade with a
persisted queue and a running engine, uninstall with data kept or removed as
asked, browser capture, the command line and MCP, and accessibility with that
platform's screen reader. A clean-machine run like the Windows Sandbox one
comes first. Until a platform passes its gate, the README says Windows only.

Suggested order, by cost: **Linux x64** (open tooling, Tauri support,
engine code runs as is once the transport, sealing and publication fence are
ported), then **macOS**, whose signing and notarization are the long pole,
then **Windows ARM64** (deferred by decision until its native, helper and
installer path is tested; the code is portable but the helpers and the
installer are not yet built for it).

## Mobile is a separate product

A phone does not keep a per-user engine running: the operating system decides
when an app runs. On iOS, long transfers belong to background `URLSession`,
which owns the connection and hands back a finished file; on Android, a
foreground service with its notification, or `WorkManager` with constraints.
Neither allows the command line, a named engine, MCP agents or LAN serving in
the same form. What could be shared is the core as a library: checkpoint and
verification logic, the protocol types, the cache's trust model. A mobile
Fetchpath would be designed from its lifecycle outward and gated on its own
evidence (backgrounding, OS kills, metered networks, battery), not ported.

## Decisions this needs from the person

Whether any other platform is wanted at all, which first, and whether paid
signing (Apple Developer Program, an Authenticode certificate) is acceptable.
None of it is started until then.
