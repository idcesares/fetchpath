# Desktop queue and recovery evidence

Verified 20 September 2026 for FP-012 on the supported Windows desktop path.

## Delivered behavior

- The Rust host owns a persistent queue and starts at most three real `fetchpath-core` jobs concurrently. The renderer never simulates progress or completion.
- The composer previews one or many destinations before enqueueing, accepts up to 100 addresses, derives sibling filenames without duplicate destinations, and can assign a future start time.
- Queue reconciliation promotes due schedules on the next poll after system wake. Persisted due items also become queued on the next application launch. Fetchpath does not wake a sleeping computer or register an operating-system wake task.
- History survives restart in the Tauri application-data directory. Search and status filters operate across the recovered history, and terminal items can be retried or removed.
- Existing destination files are never overwritten. A conflict becomes **Needs attention** with a **Choose new path** action.
- Failed, cancelled, and expired-source items expose a specific recovery action: retry, choose a new path, or paste a refreshed link.
- Closing the window hides Fetchpath to the tray. Since FP-055, quitting from the tray, or closing with the tray setting off, closes the window only; downloads continue in the engine (below).
- Native form controls, visible focus styling, `aria-live` status, `Ctrl+L`, `Escape`, and DOM-order tabbing provide the current keyboard path.

## Persistence and privacy boundary

Queue writes use a synced temporary JSON file, retain the previous file as `queue-v1.json.bak`, and restore from the primary file or backup when its schema is recognized. This protects against ordinary partial JSON replacement; it is not a claim of crash-proof filesystem durability under every power-loss mode.

URLs containing query or fragment values are shown redacted and are not persisted as restartable sources. After restart, those records become **Link needed** and require a refreshed address. Fetchpath currently has no credential/header persistence, encrypted secret store, or authenticated browser handoff in this queue slice.

## Verification

The optimized executable was built with:

```powershell
corepack pnpm --dir apps/desktop tauri build --no-bundle
```

`apps/desktop/tools/ui-smoke.ps1` (since gone stale: it waits for the link box, which moved into the Add download dialog in 0.1.0) then drove that executable through Windows UI Automation using localhost-only HTTP fixtures. It observed:

- a two-item batch preview;
- a complete 1 MiB transfer at the selected destination;
- displayed SHA-256 `bf63d8a95fcc2e64619813aae35fdcbe871fdd9264caa3f365eb3aed0f679129`, matching the file on disk;
- non-zero progress on a throttled transfer; and
- terminal cancellation with no destination file published.

The desktop Rust tests additionally exercise a real command-path download, one-slot batch concurrency, a schedule becoming due after elapsed time, safe-source recovery, private-query omission, destination conflict preservation, and user-facing invalid-input errors. The broader workspace suite retains the checkpoint, resume, cancellation, and publication-fence coverage recorded in `CHECKPOINT-RECOVERY.md`.

## Current limits

- Concurrency is fixed at three; user-configurable queue priorities, bandwidth limits, pause, and reorder are later work.
- Schedules are local timestamps. The engine stays running while a job is scheduled (FP-053), but nothing wakes the system, and after a reboot they wait for the next launch unless the sign-in start setting is on.
- Search and filters are client-side over the loaded queue. Very large-history pagination is not implemented.
- Tray lifecycle is implemented and build-validated; this task does not claim installer, update, notification, or multi-monitor acceptance, which remains part of release work.

## FP-055: the desktop as a client of the engine

Recorded 25 September 2026 on Windows 11 Pro 26200 x64. Code:
`apps/desktop/src-tauri/src/{engine_link,view,lib}.rs`, `apps/desktop/src/main.ts`;
tests `apps/desktop/src-tauri/tests/engine.rs`, the `view` and `lib` unit
tests, `tests/repo/structure.test.mjs`.

| Part | What it does |
|---|---|
| No second owner | The app builds no session and never opens `instance.lock`; its own `desktop-window.lock` keeps a second window out. A repository test fails if app sources construct a session or engine or name the engine's lock (the browser host shares the crate for its inbox types only) |
| Connection | `EngineLink`: `attach_or_launch` with `fetchpath.exe` beside the app, checked at each launch. On a lost connection the same envelope is resent to the next engine; the ledger makes that safe |
| Commands | Every Tauri command is one protocol command, run off the main thread. `view.rs` maps protocol records to the shapes `main.ts` already read, so the page's journeys are unchanged. Batches use `CreateJobs` (all or none); browser media pages come from `TakeLinkReviews`; a retry sends only what changed: `Retry` (with a corrected checksum), `ResolveDestination`, or `RefreshSource` with link, destination and checksum together (contract D2) |
| Updates | A watcher subscribes to the queue and emits `fetchpath://queue` (coalesced to 150 ms) and `fetchpath://engine`; the page refreshes on events, with a 1 s pass for what has no event (a schedule coming due, a browser link). The engine state is kept in the host and read by the page, so a report sent before the page listened, or two arriving out of order, cannot leave it wrong |
| Lost engine | The watcher reports it, starts a new engine and resubscribes. The page shows a notice only if the engine is still gone after a second |
| Closing | Close-to-tray hides the window; quitting or closing otherwise ends the window process only. The engine finishes the work and idles out |

### Commands and results

- `cargo test -p fetchpath-desktop`: 18 unit tests (view mapping, retry
  mapping, window lock) and `the_window_follows_the_engine_and_recovers_when_it_is_killed`:
  an engine killed mid-download, the window told, a new engine started by
  the link, the file finished from its checkpoint and byte-identical.
- Session: `a_batch_creates_every_job_or_none_and_a_resend_creates_nothing_more`,
  `the_person_can_correct_the_expected_checksum_before_a_retry`,
  `a_media_page_from_the_browser_is_offered_for_review_once`; agents refused
  the new commands and fields (policy tests).
- CLI: `a_command_that_starts_the_engine_returns_at_once_to_a_script_reading_its_output`
  (failed at 61 s before the launch fix).
- Release build (`tauri build --no-bundle`) with the release engine beside it:
  `tests/compatibility/windows/ui-accessibility.ps1` passed twice against the
  engine (keyboard journey with a real download, live regions, dialogs),
  from an empty data folder, which was then restored.
- UI Automation probes of the real window (scratch data folder): with no
  engine program the notice shows and stays; restoring it, the window starts
  the engine and the notice clears; an engine killed while none can start is
  reported, then cleared on recovery; a plain kill restarts within a moment
  with no lasting notice; the window process killed mid-download (1 MB of
  16 MB), the engine finished the file 19 s later with a matching SHA-256.
- `cargo test --workspace --locked`, clippy, fmt, `node --test`, `tsc`: clean.

| Temporary change | Failing check |
|---|---|
| The old in-process `lib.rs` | `the desktop app never owns the queue` |
| Launch through `std::process::Command` (inherits handles) | `a_command_that_starts_the_engine_returns_at_once_to_a_script_reading_its_output` |

### Found during development

- The engine inherited the launching client's handles, so a script reading
  a client's output waited out the engine's 60 s idle grace. Launch now
  inherits nothing (FP-053 code, `launch.rs`).
- The window cached "engine program missing" at startup and never retried;
  and it missed a disconnect reported before the page listened. Both fixed
  as described above; both were found by the UI probes, not the unit tests.

### Limitations

- Development (`tauri dev`) needs `cargo build -p fetchpath` first, so the
  engine is beside the debug app.
- The desktop's TypeScript types are still hand-written; generating them
  from the protocol schema (platform design §4) is not done.
- Installer upgrade and uninstall do not stop a running engine yet (FP-057).
- The approval card and agent access page are FP-066; a job awaiting
  approval shows only as "Waiting for approval".
