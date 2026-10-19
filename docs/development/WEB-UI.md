# Local web UI

FP-104, slice S1b (§17) of the
[instance access and remote hub design](../architecture/specs/2026-10-03-instance-access-and-remote-hub-design.md),
device rules in [D6](../architecture/JOB-CONTRACT.md) (FP-104 bullet). The queue in a browser on this PC at
`http://fetchpath.localhost:PORT`; no LAN, `.local` or remote listener.

## What was built

- **Listener** (`apps/cli/src/remote`): hyper plus tokio-tungstenite, loopback only, port 47474 or the next free one,
  off by default (`web_ui` setting, `fetchpath web on|off|open|sign-out`). Host allowlist (other hosts get 421),
  Origin check on the socket upgrade, CSP `default-src 'self'; frame-ancestors 'none'`.
- **Sign-in**: `OpenWebUi` over the authenticated pipe (user principal only) issues a single-use 60-second ticket;
  `/open` exchanges it for the `fp_session` cookie (HttpOnly, SameSite=Strict, Path=/). Sessions live in memory and
  end with the engine or `SignOutBrowsers`; tickets and cookies are never logged. One device id per sign-in.
- **Device principal**: view, add into host folder choices, pause, resume, start now, cancel, remove, plain retry and
  reveal on the host. No approve, settings, rules, agent access, LAN, cache, hub or shutdown; retry with a new link,
  folder or checksum stays on the desktop. Cancelling or removing withdraws a browser's own pending request. The
  awaiting-approval view shows the reason, offers no Approve/Deny control, and directs the person to Fetchpath on this PC.
- **Client**: shared views in `apps/desktop/src/ui/` used by the desktop and by the web entry (`web.html`,
  `src/web.ts`, socket client `src/engine/socket.ts`); capabilities hide desktop-only actions. `pnpm build:web` writes
  `dist-web`, which `apps/cli/build.rs` embeds (`remote/assets.rs`, exact-path lookup). Without a bundle, `/` serves
  a "not built" page; `FETCHPATH_REQUIRE_WEB=1` (set by `stage-cli.mjs` in the release build and by CI, which runs `build:web`) makes
  that a build error. Modal alerts stay in the modal accessibility tree; a rejected draft returns focus to Links,
  Escape restores the opener, the dismissed draft alert does not appear in Details, and Pause/Resume retains focus on
  the replacement row action.
- **Desktop**: Settings > Web UI (toggle, Open in browser, Sign out all browsers) and a tray item. The ticket link
  goes only to the default browser and only when it is exactly `http://fetchpath.localhost:PORT/...` (user info and
  non-numeric ports are rejected). The page reloads at most once until a socket opens.

## How it was verified

- `cargo test -p fetchpath`: `src/remote/tests.rs` covers single-use and expired tickets, the canonical host,
  session and origin on the socket, unknown calls closing it, folder-choice-only submit and unretargeted retry,
  refused settings, person-only links and sign-out, web UI off closing sockets and the port, loopback only;
  `tests/web.rs` covers the pipe side and engine lifetime;
  `cargo test -p fetchpath-desktop`; clippy `-D warnings` for both; `tsc`, `pnpm build`, `pnpm build:web`; no
  `@tauri-apps` or `__TAURI` string in `dist-web`; desktop `ui-smoke.ps1` passes.
- G10 in Chrome on Windows 11 against a scratch engine: signed-out page, 421 on a foreign Host, 404 for
  assets without a session, ticket sign-in, cookie not visible to script, no console or CSP errors, a 4 MiB local
  download completed with the expected SHA-256, details and remove, engine stop and restart (one reload to the
  signed-out page), `fetchpath web sign-out` ending a live page, 403 on a foreign-origin upgrade.
- Owner check against a scratch engine: Edge and Firefox, 200% scaling, narrow widths, high contrast and keyboard
  focus passed.
- Independent strong review: pass; its findings (release build requires the web bundle, link check, single reload)
  are built in.
- The FP-107 Chrome walkthrough (awaiting-approval view, modal focus, Pause/Resume focus) is in
  [Local quality evidence](LOCAL-QUALITY.md).

## Limitations that still hold

- No immutable caching of hashed assets.
- Audible screen-reader output was not tested; Chrome accessibility-tree placement and keyboard focus were checked.
- 0.2.0 cannot read a job store that holds a browser job (CHANGELOG upgrade note).
