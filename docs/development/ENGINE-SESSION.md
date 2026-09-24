# Engine session

This record covers moving the desktop queue into `crates/fetchpath-session`
([platform design](../architecture/specs/2026-09-24-engine-platform-design.md)
§9). FP-048 pinned the current behavior first; FP-049 moves the code and must
leave these tests passing unchanged.

## FP-048: characterization of the desktop queue

Recorded 24 September 2026 on Windows 11 Pro 26200 x64. Plan:
[2026-09-24-fp-048-desktop-queue-characterization](plans/2026-09-24-fp-048-desktop-queue-characterization.md).

Twenty-one tests in `apps/desktop/src-tauri/src/characterization.rs` pin, on
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
