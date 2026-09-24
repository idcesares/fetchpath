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
- **F2: unknown errors are retried.** Unrecognized and `internal.*` errors map to `retry` and are retried automatically, contrary to [the job contract](../architecture/JOB-CONTRACT.md) §9. Fixed in FP-051.
- **F3: actions come from message text.** Failure actions are derived by reading error message text; the contract says clients act on the error's code and `action`. Fixed in FP-051.
- **F4: a failed batch was half queued** (found while planning FP-051). A batch that failed validation on one link had already queued the links before it. Fixed in FP-051: every link is checked first.

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

### Desktop walkthrough, before and after

Recorded 24 September 2026. The desktop was built twice with its frontend
embedded (`cargo build -p fetchpath-desktop --features tauri/custom-protocol`):
"before" from `d7376aa` in a separate worktree, "after" from the FP-049 tree.
Each ran against an empty data folder and a local server
([`server.mjs`](evidence/engine-session/fp-049-walkthrough/server.mjs))
that sends files at about 1 MB/s with a strong ETag and byte ranges. A
driver ([`drive.mjs`](evidence/engine-session/fp-049-walkthrough/drive.mjs))
worked the real window through WebView2's DevTools port, clicking the same
buttons a person would:

1. First start: empty queue, onboarding dismissed.
2. Add a small file: it completes.
3. Add a 12 MB file, pause it while it runs, resume it, pause it again.
4. Add an 8 MB file and, while it runs, kill the process (`taskkill /F`).
5. Restart: the killed download continues from its checkpoint; the paused
   one is still paused. Resume it; both complete.
6. History: searching "second" shows only that row; the Completed filter
   shows all three.
7. Every file's SHA-256 matches the source.
8. Kill and restart again: all three are still listed as completed.

The two transcripts
([before](evidence/engine-session/fp-049-walkthrough/before-transcript.json),
[after](evidence/engine-session/fp-049-walkthrough/after-transcript.json))
are byte-identical: the same states, row actions, saved queue records and
file checks at every step. The request logs
([before](evidence/engine-session/fp-049-walkthrough/before-requests.txt),
[after](evidence/engine-session/fp-049-walkthrough/after-requests.txt)) have
the same sequence, with every resume a range request carrying `If-Range`.
They differ only in the byte offset of the final resume, which depends on
how much arrived before the second pause. Screenshots at each step matched;
they show local paths, so they are not checked in.

### Review

Independent strong-model review, 24 September 2026: **approve with nits**.
It diffed the parent commit against the new files and found that the queue
save, load fallback, settings, inbox write ordering, `mark_processed` against
the queue save, secret removal, lock scope, file names and data folder are
unchanged. The DPAPI code, entropy and flags are byte-identical. The session
has no Tauri or UI dependency, nothing secret is newly exposed, and the test
sets match (71 names). Its nits: a duplicated workspace member (fixed) and two
omissions in the list above (added).
## FP-051: command ledger, revisions and event sequencing

Recorded 24 September 2026 on Windows 11 Pro 26200 x64. Plan:
[2026-09-24-fp-051-ledger-and-events](plans/2026-09-24-fp-051-ledger-and-events.md).

The session now carries out protocol v1 commands through `Engine` and the
in-process `EngineClient` (`fetchpath_session::engine`). The desktop still
calls the session directly and behaves as before; its walkthrough, re-run on
this tree, produced the same transcript as FP-049's.

| Part | What it does |
|---|---|
| Ledger | Mutating commands are looked up by `(client_id, command_id)` before anything else; a resend returns the stored result, a reused id with another request is `contract.idempotency_conflict`. Unseen commands older than 10 minutes or more than 2 minutes in the future are refused; entries are kept 12 minutes |
| Commit | One ledgered command at a time. Saves are held back while it runs (a guard lifts that even if the change panics); then its events are derived, its result built, its ledger entry added, and everything written. Only then is it acknowledged; a resend whose entry is not yet on disk is written first, and refused while the disk still fails |
| Files | `engine-v1.json` holds the ledger and retained events, each tagged with its commit generation; `queue-v1.json` names the generation and cursor it committed. The engine file is written first, the queue file second, and a load discards what the queue did not commit. Numbers never go backwards: when the engine file saw more than the queue committed (a crash between the writes, or a damaged queue file restored from its backup), cursors and sequences resume one past the highest used, so a resubscribing client gets a snapshot boundary. A 0.1.0 queue reads and writes back unchanged |
| Revisions | Each durable event advances the job's `job_revision`; `expected_revision` mismatches return `contract.revision_conflict` with the current revision, changing nothing |
| Events | Derived at every commit from what changed since the job's last report: `job_created`, `state_changed`, `error_recorded`, `publication_completed`, `policy_changed`, `job_removed`. Per-job `seq`, engine-wide `cursor`, correlated to the command that caused them. The last 512 are kept |
| Streams | `SubscribeJob` and `SubscribeQueue` replay what was missed, counting only events on disk, or answer with an atomic snapshot boundary when it was compacted, with the subscriber registered under the same lock. A subscriber more than 1,024 events behind is closed with `resource.subscriber_lagging`; progress is coalesced to the latest sample per job |
| Errors | F2 and F3 fixed: actions come from the failure's code; `internal.*` and uncoded failures are never retried automatically and are `retryable: false` on the wire |

### Found during the work

- Keeping the ledger and events inside `queue-v1.json` made every save
  rewrite them, including every progress poll: 358 KB after 241 commands,
  and a test that took 28.7 s for 240 commands. The engine file and
  generation scheme replaced it; the same test takes 2.2 s, and saves with no
  new events cost what they did in 0.1.0.
- F4, above.

### Commands and results

- `cargo test -p fetchpath-session --test engine`: 10 passed (duplicate and
  conflicting commands, stale and future commands, a revision race between
  two clients, replay after reconnect, compaction to a snapshot boundary, a
  stalled subscriber under load with real downloads, a failed commit, a crash
  before commit with the engine file written, errors as codes).
- `cargo test --workspace --locked`, `cargo clippy --workspace --all-targets --locked -- -D warnings`, `cargo fmt --all --check`, `node --test` (39): clean.
- The FP-048 characterization tests pass; F2 and F3's were changed on purpose.
- Desktop walkthrough (FP-049's driver) on this tree: transcript identical.

### Mutation checks

| Temporary change | Failing tests |
|---|---|
| Ledger lookup disabled | resend, conflict, recorded-command, failed-commit and crash tests |
| Saves not held back during a command | failed-commit, reconnect and stalled-subscriber tests |
| Revision check removed | `two_clients_racing_on_one_revision_cannot_both_win` |
| Uncommitted engine data kept on load | `a_crash_before_commit_loses_the_command_so_the_resend_creates_one_job` |
| Queue file written before the engine file | crash-before-commit and failed-commit tests |
| Subscriber queue unbounded | `a_subscriber_that_never_reads_cannot_hold_up_the_queue` |

### Limitations

- The HTTP status of a transport failure is still read from the fixed
  `HTTP status NNN` detail the transport writes; the core does not report it
  structurally yet.
- `SelectMedia`, `RefreshMediaChoices`, replacing an existing file and
  keeping a partial file on cancel answer `contract.unsupported`.
- A command whose change starts a transfer before its commit fails leaves
  that transfer running in memory; its record and ledger entry are written
  together by the next successful save, so a resend still answers once.
- While the desktop still calls the session directly (until FP-055), its
  changes made during a ledgered command are saved with that command's
  commit rather than at once, and the revision check does not see them
  until then.
- If `engine-v1.json` itself is damaged, the engine starts with an empty
  ledger and event log; a resend of a command from the last 12 minutes could
  then apply again. The file is only ever replaced by an atomic rename.
- When a damaged queue file falls back to its backup (at most one save old),
  the ledger entries of the lost save go with it. Their changes are lost too,
  so a resend re-applies something that is really missing; a file already
  published by then is protected by the destination-conflict check, which
  asks for a new path instead of overwriting.
- The numbers skipped after such a load come from the retained events (512).
  If one lost save had produced more events than that, the oldest of them
  would be missing from the per-job numbers; the queue cursor is unaffected.
- Queue cursors can therefore have gaps. Clients must not treat a cursor gap
  as a lost event; per-job `seq` is contiguous except across such a load,
  where the gap sends the client to a snapshot boundary. Every crash between
  the two writes costs resubscribing clients one snapshot.

### Independent review

Strong-model review, 24 September 2026: **changes required**. Confirmed
sound: lock order (queue, then engine state, everywhere), the two-file
commit ordering at every crash point, broadcast only after a successful
write, sequence numbering, replay and boundary edges, subscriber bounds, the
ledger rules, and F2 to F4. Findings and fixes:

| Finding | Fix | Test |
|---|---|---|
| Medium: a resend was answered from memory while its entry's write had failed | A resend whose entry is newer than the committed generation is written first, or refused | `a_failed_commit_leaves_nothing_behind_and_the_resend_applies_once` |
| Medium-low: restoring the queue from its backup reused cursors and sequences subscribers had seen | Numbers resume one past the highest the engine file used | `a_queue_restored_from_its_backup_never_reuses_numbers_a_subscriber_saw`, `durable::tests::loading_an_older_queue_skips_past_every_number_already_used` |
| Low: a panic during a command left saves deferred for good | Drop guard | Reviewed |
| Low: a new subscriber could be shown events not yet written | Replay and positions count committed events only | `durable::tests::only_written_events_count_as_committed` |
| Low: a job removed during a command's commit returned early without writing | The change is written before the error returns | Reviewed |
| Nit: ledger retention one tick short | `<=` | Reviewed |
| Nit: revision check not atomic with desktop changes | Documented above | — |

Re-review of the fixes, 24 September 2026: **approve with nits**. Every fix was re-probed, including a per-job case: after a backup fallback, `SubscribeJob` from the last seen `seq` gets a snapshot boundary, and a command with the old `expected_revision` gets `contract.revision_conflict`. The nits are the last three limitations above.