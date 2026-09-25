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

`apps/desktop/tools/ui-smoke.ps1` then drove that executable through Windows UI Automation using localhost-only HTTP fixtures. It observed:

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
| No second owner | A build fact: the desktop does not depend on `fetchpath-session`, directly or through any path dependency, so it cannot construct a queue. The browser host's inbox types moved to `crates/fetchpath-browser-inbox` for that. `tests/repo/structure.test.mjs` walks the desktop's path dependencies and fails if the session crate appears, or if app sources name the engine's lock or `lock_path`. The window's own `desktop-window.lock` keeps a second window out |
| Connection | `EngineLink`: `attach_or_launch` with `fetchpath.exe` beside the app, checked at each launch. On a lost connection the same envelope is resent to the next engine; the ledger makes that safe |
| Commands | Every Tauri command is one protocol command, run on a blocking thread (`spawn_blocking`) so a slow engine or an engine start cannot hold up Tauri's async runtime; a start never holds the connection lock. `view.rs` maps protocol records to the shapes `main.ts` already read, so the page's journeys are unchanged. Batches use `CreateJobs` (all or none); browser media pages come from `TakeLinkReviews`; a retry sends only what changed: `Retry` or `ResolveDestination`, either with a corrected checksum, or `RefreshSource` with link, destination and checksum together (contract D2) |
| Updates | A watcher subscribes to the queue and emits `fetchpath://queue` (coalesced to 150 ms) and `fetchpath://engine`; the page refreshes on events, with a 1 s pass for what has no event (a schedule coming due, a browser link). The engine state is kept in the host and read by the page, so a report sent before the page listened, or two arriving out of order, cannot leave it wrong |
| Lost engine | A stopping engine removes its endpoint file as it decides to stop; a killed one leaves it. After a kill the watcher starts a new engine and resubscribes, and the page shows a notice only if the engine is still gone after a second. After an orderly stop (`fetchpath engine stop`, an installer), or three failed starts in a row, the window starts nothing by itself: the page says so at once and offers Start Fetchpath. Reconnecting to an engine started elsewhere re-arms crash recovery. A lost connection is resent to the next engine; a timeout is not, so a long media inspection is not run twice |
| Closing | Close-to-tray hides the window; quitting or closing otherwise ends the window process only. The engine finishes the work and idles out |

### Commands and results

- `cargo test -p fetchpath-desktop`: 18 unit tests (view mapping, retry
  mapping, window lock) and `the_window_follows_the_engine_and_recovers_when_it_is_killed`:
  an engine killed mid-download, the window told, a new engine started by
  the link, the file finished from its checkpoint and byte-identical;
  `an_engine_stopped_on_purpose_is_not_started_again_until_the_person_asks`
  (nothing started for 4 s after `EngineShutdown`, commands refused, Start
  reconnects); `an_engine_that_will_not_start_is_not_retried_forever`.
- Session: `a_batch_creates_every_job_or_none_and_a_resend_creates_nothing_more`,
  `the_person_can_correct_the_expected_checksum_before_a_retry`,
  `a_media_page_from_the_browser_is_offered_for_review_once`; agents refused
  the new commands and fields (policy tests).
- CLI: `a_command_that_starts_the_engine_returns_at_once_to_a_script_reading_its_output`
  (failed at 61 s before the launch fix).
- `apps/desktop/tools/ui-smoke.ps1`, repaired: it had gone stale when the
  composer moved into the Add download dialog (0.1.0) and used global
  keystrokes. It now opens the dialog, sends input by window message through
  `uia-common.ps1`, and runs against its own engine in `work\desktop-e2e`.
  Against the engine: a two-link batch preview; a 1 MiB download published
  with the displayed SHA-256 `bf63d8a9…` equal to the file (as in FP-012);
  progress then cancellation publishing nothing; a wrong checksum saving
  nothing with Edit checksum offered, and `SHA256:` in capitals matching.
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
- After the review fixes, on a fresh release build: the smoke walkthrough and
  the accessibility harness passed again; `fetchpath engine stop` with the
  window open left no engine for 5 s and showed the stopped notice with
  Start Fetchpath, which started the engine and cleared it; a killed engine
  was replaced by the window with no lasting notice.
- `cargo test --workspace --locked`, clippy, fmt, `node --test`, `tsc`: clean.

| Temporary change | Failing check |
|---|---|
| The old in-process `lib.rs` | `the desktop app never owns the queue` |
| Launch through `std::process::Command` (inherits handles) | `a_command_that_starts_the_engine_returns_at_once_to_a_script_reading_its_output` |
| The desktop depends on `fetchpath-session` again | `the desktop app never owns the queue` |
| The engine keeps its endpoint file when it stops | `an_engine_stopped_on_purpose_is_not_started_again_until_the_person_asks` |

### Found during development

- The engine inherited the launching client's handles, so a script reading
  a client's output waited out the engine's 60 s idle grace. Launch now
  inherits nothing (FP-053 code, `launch.rs`).
- The window cached "engine program missing" at startup and never retried;
  and it missed a disconnect reported before the page listened. Both fixed
  as described above; both were found by the UI probes, not the unit tests.

### Independent review

Strong-model review, 25 September 2026 (a first attempt stopped at the
account's session limit): **changes required**, all fixed. It confirmed the
no-second-owner guarantee in the code as written, resend safety under the
ledger (ledger lookup before the age check, entries kept 12 min against a
resend of seconds, a stopping engine refusing before it acts), agent refusal
of every new command and field, and the `CreateProcessW` and
`SHGetKnownFolderPath` FFI.

| Finding | Fix |
|---|---|
| Medium: the window restarted an engine stopped on purpose, and relaunched every second forever when none could start | Orderly stop removes the endpoint; the window then waits for Start; three failed starts stop the retries. Two tests above |
| Medium-low: the structure test was a name match that rustfmt-style imports, aliases and `lock_path()` slipped past | The inbox moved to its own crate and the desktop no longer depends on the session crate; the test walks path dependencies |
| Low-medium: `#[tauri::command(async)]` ran blocking pipe calls on the async runtime, behind a lock held across a 10 s start | `spawn_blocking` per command; starts serialized on their own lock |
| Low: D2 promised a `work_generation` the session does not have | D2 reworded: staged bytes may be resumed; the whole-file hash check at publication keeps a changed checksum safe |
| Low: a new destination with a corrected checksum and no new link was refused | `ChooseNewPath` carries an optional checksum, person only (agent refusal tested) |
| Info: a timed-out command was resent, running an inspection twice | Timeouts are not resent |

### Limitations

- Development (`tauri dev`) needs `cargo build -p fetchpath` first, so the
  engine is beside the debug app.
- The desktop's TypeScript types are still hand-written; generating them
  from the protocol schema (platform design §4) is not done.
- Installer upgrade and uninstall do not stop a running engine yet (FP-057).
- The approval card and agent access page are FP-066; a job awaiting
  approval shows only as "Waiting for approval".
