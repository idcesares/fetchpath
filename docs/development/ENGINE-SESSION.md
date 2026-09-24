# Engine session

This record covers moving the desktop queue into `crates/fetchpath-session`
([platform design](../architecture/specs/2026-09-24-engine-platform-design.md)
§9). FP-048 pinned the current behavior first; FP-049 moves the code and must
leave these tests passing unchanged.

## FP-048: characterization of the desktop queue

Recorded 24 September 2026 on Windows 11 Pro 26200 x64. Plan:
[2026-09-24-fp-048-desktop-queue-characterization](plans/2026-09-24-fp-048-desktop-queue-characterization.md).

Twenty-one tests, now in `crates/fetchpath-session/src/characterization.rs` (moved by FP-049), pin, on
unchanged production code:

| Area | What is pinned |
|---|---|
| File shapes | A 0.1.0 queue file (`tests/fixtures/queue-0.1.0.json`, fourteen records covering every saved state, media, checksums and a browser capture) re-serializes to exactly itself through the production types; a 0.1.0 settings file with every value changed loads without a repair and writes back identically; a non-ASCII destination survives a save |
| Restart | What each saved state becomes on load: completed and failed rows are kept; a paused row with a public link stays paused; private links and lost browser context become `needs_source` with `edit_link` or `recapture`; a row that was running comes back queued; an unreadable saved checksum fails closed; a media row without helpers fails with `configure_media_tools`; no rate or pending retry survives a restart |
| Queue file | Saving goes through a backup with no temporary left behind; a corrupt file falls back to its backup; a corrupt file and backup start an empty queue; unknown fields are ignored; fields added since the first format default when missing |
| Failures | The failure-to-action table, including precedence; only failed rows carry an action; only `retry` rows are retried automatically, with a 15 s then 30 s backoff, never past the attempt limit and never over an existing file; a transport failure restored after a restart is retried |
| Rates | Smoothing (0.3), the 400 ms minimum interval, withdrawal after 5 s without progress, restart on a lower offset, no rate below 1 B/s, remaining time rounded up and only with a total and a rate |
| Display | Shown links drop queries, fragments and user info; only links without a query or fragment are kept for restart |
| Interface JSON | The exact field names of a queue row, queue statistics, segments, details and cancel responses |

### Mutation checks

Each change was made to `lib.rs`, the tests run, and the change reverted.

| Temporary change | Failing test |
|---|---|
| Checksum mismatches classified as `retry` | `each_failure_maps_to_the_step_a_person_or_the_queue_takes_next` |
| Paused rows not restored as paused | `every_saved_state_comes_back_after_a_restart_as_it_did_in_0_1_0` |
| Backup fallback removed | `a_corrupt_queue_file_falls_back_to_its_backup` |
| Minimum rate interval 400 → 100 ms | `the_rate_is_smoothed_ignores_short_intervals_and_withdraws_when_stalled` |
| `cleanupPending` renamed on the wire | Nine tests, including `the_0_1_0_queue_file_is_exactly_what_the_queue_writes` and `the_interface_reads_these_exact_field_names` |

### Commands and results

- `cargo test -p fetchpath-desktop --locked`: 70 passed, 0 failed, 1 ignored (pre-existing), against 49 before.
- `cargo clippy -p fetchpath-desktop --all-targets --locked -- -D warnings`: clean.
- `cargo fmt -p fetchpath-desktop --check`: clean.

No production code changed; `lib.rs` gained only the test module declaration.

### Findings (pinned, not fixed)

Tests named `finding_…` pin behavior that is believed wrong, so the move
cannot change it silently. Each fix is its own task and updates its test on
purpose.

- **F1: a newer queue is lost.** A queue written by a newer schema version is treated as no queue; the first save moves it to the backup and the second deletes it. Once an engine and clients from different builds coexist, opening an older build loses the queue. Fix: FP-070.
- **F2: unknown errors are retried.** Unrecognized and `internal.*` errors map to `retry` and are retried automatically, contrary to [the job contract](../architecture/JOB-CONTRACT.md) §9. Fix: FP-051.
- **F3: actions come from message text.** Failure actions are derived by reading error message text; the contract says clients act on the error's code and `action`. Fix: FP-051, with the protocol's error codes from FP-050.

## FP-049: the queue moves into `fetchpath-session`

Recorded 24 September 2026 on Windows 11 Pro 26200 x64. Plan:
[2026-09-24-fp-049-session-crate](plans/2026-09-24-fp-049-session-crate.md).

`crates/fetchpath-session` now holds the queue model, its persistence,
scheduling, automatic retry, rate estimates, history, settings, media
orchestration and the browser capture inbox. It has no Tauri or UI types. The
desktop keeps the Tauri commands, tray, single-instance guard, native
messaging host, media-tool setup and browser setup, and calls the session
in-process through `Session` (formerly `DesktopJobs`).

| Moved | From | To |
|---|---|---|
| Queue, persistence, scheduler, retry, rates, media jobs, and their tests | `apps/desktop/src-tauri/src/lib.rs` (its imports, lines 21–1995 and its test module) | `crates/fetchpath-session/src/lib.rs` |
| Settings | `apps/desktop/src-tauri/src/settings.rs` | `crates/fetchpath-session/src/settings.rs` (unchanged; re-exported as `fetchpath_desktop_lib::settings`) |
| Browser inbox store, validation, DPAPI | `apps/desktop/src-tauri/src/browser_bridge.rs` | `crates/fetchpath-session/src/browser_inbox.rs` (the host loop stays in `browser_bridge.rs` and re-exports the inbox types) |
| Characterization tests and fixtures | `apps/desktop/src-tauri/src/characterization.rs`, `tests/fixtures` | `crates/fetchpath-session/src/characterization.rs`, `tests/fixtures` |

Changes to the moved code, all checked by diffing it against the original:
`pub` on the types and methods the desktop calls and on the draft and
snapshot fields; two accessors (`settings_repaired`, `take_link_reviews`)
replacing the desktop's direct field reads; `browser_inbox::storage_reason` made `pub` for the host, which keeps its own copy of the one-line `invalid_data` helper; the type rename; the test
imports' module path; and one test fixture path made relative to the new
crate. No logic, message text, file name, serialized field or data folder
changed. The characterization tests differ from FP-048 only in the type
name. The desktop dropped its direct `uuid`, `url` and `windows-sys`
dependencies. `Cargo.lock` gained no third-party package.

### Commands and results

- Test names before and after, listed with `-- --list`: the same 71 (70 run, 1 ignored).
- `cargo test --workspace --locked`: all passed; `fetchpath-session` 58, `fetchpath-desktop` 12 and 1 ignored (70 and 1 before).
- `cargo clippy --workspace --all-targets --locked -- -D warnings`: clean.
- `cargo fmt --all --check`: clean.
- `node --test`: 39 passed, including the repository-structure check.
- `node tools/tasks.mjs check`: pass.

### Not yet done

- The desktop walkthrough (add, pause, resume, restart recovery, history)
  before and after. A running Fetchpath held `fetchpath-desktop.exe` during
  this work, so the app binary was not rebuilt or launched.

### Review

Independent strong-model review, 24 September 2026: **approve with nits**.
It diffed the parent commit against the new files and found that the queue
save, load fallback, settings, inbox write ordering, `mark_processed` against
the queue save, secret removal, lock scope, file names and data folder are
unchanged. The DPAPI code, entropy and flags are byte-identical. The session
has no Tauri or UI dependency, nothing secret is newly exposed, and the test
sets match (71 names). Its nits: a duplicated workspace member (fixed) and two
omissions in the list above (added).