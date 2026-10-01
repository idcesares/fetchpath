# Engine session

This record covers moving the desktop queue into `crates/fetchpath-session`
([platform design](../architecture/specs/2026-09-24-engine-platform-design.md)
§9). FP-048 pinned the current behavior first; FP-049 moves the code and must
leave these tests passing unchanged.

## FP-048: characterization of the desktop queue

Recorded 24 September 2026 on Windows 11 Pro 26200 x64. Plan:
[2026-09-24-fp-048-desktop-queue-characterization](ARCHIVE.md).

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

- **F1: a newer queue is lost.** A queue written by a newer schema version was treated as no queue; the first save moved it to the backup and the second deleted it. Fixed by FP-070 (below); the characterization test now pins the fixed behavior.
- **F2: unknown errors are retried.** Unrecognized and `internal.*` errors map to `retry` and are retried automatically, contrary to [the job contract](../architecture/JOB-CONTRACT.md) §9. Fixed in FP-051.
- **F3: actions come from message text.** Failure actions are derived by reading error message text; the contract says clients act on the error's code and `action`. Fixed in FP-051.
- **F4: a failed batch was half queued** (found while planning FP-051). A batch that failed validation on one link had already queued the links before it. Fixed in FP-051: every link is checked first.

## FP-049: the queue moves into `fetchpath-session`

Recorded 24 September 2026 on Windows 11 Pro 26200 x64. Plan:
[2026-09-24-fp-049-session-crate](ARCHIVE.md).

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
Each ran against an empty data folder and a local server that sends files at about 1 MB/s with a strong ETag and byte ranges. A driver worked the real window through WebView2's DevTools port, clicking the same
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
(retired with the scripts; see [the archive](ARCHIVE.md))
are byte-identical: the same states, row actions, saved queue records and
file checks at every step. The request logs have the same sequence, with every resume a range request carrying `If-Range`.
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
[2026-09-24-fp-051-ledger-and-events](ARCHIVE.md).

The session now carries out protocol v1 commands through `Engine` and the
in-process `EngineClient` (`fetchpath_session::engine`). The desktop still
calls the session directly and behaves as before; its walkthrough, re-run on
this tree, produced the same transcript as FP-049's.

| Part | What it does |
|---|---|
| Ledger | Mutating commands are looked up by `(client_id, command_id)` before anything else; a resend returns the stored result, a reused id with another request is `contract.idempotency_conflict`. Unseen commands older than 10 minutes or more than 2 minutes in the future are refused; entries are kept 12 minutes |
| Commit | One ledgered command at a time. Saves are held back while it runs (a guard lifts that even if the change panics); then its events are derived, its result built, its ledger entry added, and everything written. Only then is it acknowledged; a resend whose entry is not yet on disk is written first, and refused while the disk still fails |
| Files | `engine-v1.json` holds the ledger and retained events, each tagged with its commit generation; `queue-v1.json` names the generation and cursor it committed. The engine file is written first, the queue file second, and a load discards what the queue did not commit. Numbers never go backwards: when the engine file saw more than the queue committed (a crash between the writes, or a damaged queue file restored from its backup), cursors and sequences resume one past the highest used, so a resubscribing client gets a snapshot boundary. A 0.1.0 queue remains readable and is written as schema v2 on the next save for local torrent metadata; the ledger format remains v1 |
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

## FP-054: principals, agent policy and approval

Contract amendment D1 in [the job contract](../architecture/JOB-CONTRACT.md#14-decision-record)
came first; the code follows it.

- **Where.** The principal is declared in the pipe handshake (`Hello.principal`,
  absent = `user`) and checked once per connection; an invalid one fails the
  handshake. The engine enforces it: `Engine::execute_as` / `subscribe_as`, with
  the allowlist in `crates/fetchpath-session/src/policy.rs`. Only a `user`
  connection may ask the engine to restart.
- **State.** A record carries its creator's principal and, while it waits, an
  approval with reasons; both persist in the queue file only when not the
  default, so a 0.1.0 file still writes back byte for byte. A waiting record
  is shown as `awaiting_approval` whatever its prepared job reports, and
  nothing starts it but `ApproveJob`. Agent access is stored in
  `agents-v1.json` beside the queue; a missing or unreadable file grants
  nothing.
- **Size.** A running agent job whose stated total or received bytes pass its
  limit is cancelled to its checkpoint in `reconcile_locked` and held. Approval
  joins the stopped transfer before a new one resumes from the checkpoint.

### Checks

`crates/fetchpath-session/tests/policy.rs` (16 tests): grant inside and
outside, a sibling folder sharing the grant's prefix, `..`, a junction out of
a grant and a grant that is a junction, credentials in a link, a stored
`credential_ref` and in media inspection, `replace_existing` on create and on
resolve, another principal's jobs through every job command, a subscription
and a stale revision, person-only commands, the browser's single command,
approval and denial, the agent retrying a denied job, a restart while
waiting, a command id reused across principals, the hourly rate and the
pending cap, and a size stop with the length stated and with it unknown,
each finishing with the right bytes after approval; a withdrawn request retried
by its agent (outside a grant and past the rate) and an approved job given a
new link, and a refused refresh that must not shed the hold. Pipe:
`a_connection_declares_its_principal_in_the_handshake_and_a_bad_one_is_refused`.
End to end: `apps/cli/tests/engine.rs`
`an_agent_over_the_pipe_is_held_to_its_policy_and_cannot_restart_the_engine`.
Mutation checks: honouring a non-user restart, and forgetting a withdrawn
request's reasons, each fail a test.

### Independent review

A strong-model review of `9d557d0` found an agent could withdraw a waiting
job and retry it past the person (blocker), keep an approval after changing
the link, and have a decision racing publication misreport the result; also
that shutdown skipped size-stopped jobs and `EngineStatus` counted everyone's
jobs. All fixed with tests. The re-review found a refused retry dropped a
withdrawn request's hold before failing; `retry_as` now settles the approval
only after every check passes, and a second re-review found no blockers.

### Limitations

- An agent retrying its own approved job outside its grants asks again.
- The size limit is checked when the engine samples progress, so a download
  can pass it by one sampling interval, and one that publishes inside that
  interval completes (contract D1).
- A media download stopped for size restarts from zero after approval; media
  has no checkpoint.
- The hourly rate is counted in memory and restarts empty with the engine.
- A build from before D1 reading this queue would not know the approval gate
  and could start a waiting job; downgrades are not supported. FP-070 guards
  only a queue with a higher schema version, and D1 did not raise it.
- `EngineStatus` shows an agent queue-wide counts, not jobs.
- Approval surfaces in the terminal and desktop, and the agent-access page,
  are FP-066.

## FP-070: a queue from a newer build is kept

Recorded 25 September 2026 on Windows 11 Pro 26200 x64. Code:
`crates/fetchpath-session/src/lib.rs` (`load_persisted`, `QueueRecord::shown`,
the read-only guards), `crates/fetchpath-session/src/engine.rs`,
`EngineStatus.queue_read_only` in `crates/fetchpath-protocol`, the CLI's `ls`
and `engine status`, and the desktop's engine notice.

| Part | What it does |
|---|---|
| Loading | Both the queue and its backup are read by their `schemaVersion` first. If either is newer (queue first), the session starts read-only and shows that file; only otherwise is the queue, or the backup when the queue is missing or unreadable, loaded as before. A version present in a form this build never writes (a string, a fraction, beyond 64 bits, negative) counts as newer; a missing, null or 0 version is a damaged file, as before |
| What is shown | The newer build's records as it saved them, when they can still be parsed (unknown fields ignored); otherwise none. No download is prepared and no browser secret is opened |
| Never written | Routine saves do nothing and the write itself refuses while read-only, so the queue, its backup and the engine journal are never touched. Reconcile does nothing, no progress is sampled, captures stay in the browser inbox, and nothing counts as the engine's own work, so it still leaves when idle |
| Clients | Every change except `EngineShutdown` is refused with `storage.queue_from_newer_version` (action `update_software`), which the CLI maps to exit 6. `EngineStatus.queue_read_only` carries the explanation and `active_jobs` is 0. `engine status` prints it, `ls` prints it on standard error, and the desktop shows it in its engine notice |

### Checks

- `finding_f1_a_queue_from_a_newer_schema_is_kept_byte_for_byte` replaces the
  F1 pin. It opens, lists and tries to save three times; the queue and its
  backup stay byte-identical, and no journal or temporary file appears.
  Every listed state equals the saved one. Also
  `an_unreadable_newer_queue_and_a_newer_backup_are_kept_too`,
  `a_newer_backup_behind_a_current_queue_is_kept` and
  `a_version_this_build_cannot_read_is_treated_as_newer`. The engine test
  `a_queue_from_a_newer_build_is_served_read_only_and_never_written` covers
  listing, refusals for create, resume and remove with the stable code and
  action, the status field, shutdown, an unchanged file, and no destination
  or partial file.
- By hand against the dev build: `ls`, `engine status` and `add` (exit 6)
  explain; the queue file's hash is unchanged, and no backup or journal is
  written.
- `cargo test` for `fetchpath-session`, `fetchpath-protocol` (schema
  regenerated with one additive optional field), `fetchpath` and
  `fetchpath-desktop`, plus clippy and fmt: clean.

| Temporary change | Failing test |
|---|---|
| A newer queue treated as missing (the old behavior) | `finding_f1_…` and `an_unreadable_newer_queue_…` |

### Limitations

- The desktop shows the explanation, but its buttons stay enabled; pressing
  one shows the refusal.
- Settings and agent grants are separate files with their own versions and
  are not covered.
- A queue with the same schema version written by a later build (as with D1)
  is still read as current.
- `QueueStats` counts the shown records by their saved states, so a record the
  newer build saved as running is counted as running while nothing runs.
- A newer backup outranks a current queue. After a downgrade to a build from
  before FP-070, work saved there stays hidden behind the older newer-format
  list until the newer build is installed again; nothing is lost.

### Independent review

Strong-model review, 25 September 2026: **approve with nits**. Every writer
was confirmed guarded (queue, backup, temporary, journal, settings, agent
grants, browser inbox and secrets, downloads), along with shutdown, idle exit,
subscription replay and the backup cases. Fixed: a newer backup behind a queue
an older build wrote is now kept (it was deleted by the next save); a version
in a form this build does not write is treated as newer rather than as
damage; no progress is sampled for records saved as running; tests pin the
saved states and the absence of download files; the record date. Left as a
limitation: `QueueStats` counts.

## FP-088: command and transfer wakeups

Recorded 1 October 2026. The engine host and in-process client now wait on
one coalesced condition-variable signal. Successful command commits and
terminal file, media and torrent snapshots signal reconciliation; wakeups
remain pending until a driver consumes them. The fixed 250 ms deadline
still samples progress and catches timers, retries and browser captures.
Reconciliation takes the command serialization lock, preserving the ledger
commit boundary. Notifications run outside job snapshot locks; command
notifications follow the successful engine-then-queue write. Worker joins,
cache population and durable event delivery keep their existing ordering.

The CLI refreshes job state on durable events and reconnects on stream
failure. A quiet stream no longer causes a one-second `GetJob` poll. It
applies subscription snapshot boundaries before waiting, including a
completion whose event was compacted before subscription.

Validation: the no-ticker session test proves command wakeup coalescing,
completion, starting a resumed job in the freed slot, and completion on disk
before its publication event is read. Two CLI tests prove event-driven
completion after a quiet interval and completion from a subscription
boundary without a query or stream read. The full session suite passes
(78 unit, 16 engine, 1 LAN and 29 policy tests). CLI engine/queue suites
pass (9 and 10 tests), core 76 and media 17 pass; three existing tests requiring
external fixtures remain ignored. Torrent compilation and doc checks pass;
it has no package behavioral tests. Formatting and diff checks pass.

Independent Astra/low review approved the persistence and concurrency
boundary without findings. Media/torrent callback execution and sustained
wakeup sampling were checked statically. The PR readiness pass collapsed
three existing nested conditions in torrent request validation, helper stdin
handling and session torrent approval. Strict workspace all-target Clippy
now passes without lint exceptions; the conditions retain their ordering.

The release benchmark uses the FP-084 unshaped 64 MiB fixture, five seeded
pairs per build, a warmed engine and independent output verification.
Against unchanged `3f4f9ed`, median time to verified file changes from
497.3/954.6 ms (core/engine) to 332.2/502.7 ms. The engine-minus-core gap
falls from 457.3 to 170.5 ms (62.7%), passing the required halving. Median
within-pair gaps fall from 503.5 to 191.9 ms. Raw timing samples and settings:
[fp088-latency.json](evidence/engine-session/fp088-latency.json).
This is a five-repetition Windows/NTFS loopback comparison, with observed
timing noise, not an Internet throughput claim. Release build passed.
Repository checks pass: `node --test` (57 tests) and
`node tools/tasks.mjs check` (88 task contracts and reachable evidence).
The PR readiness pass also replaces timed fixtures in the CLI cancellation,
pause/resume/watch and MCP wait-capacity tests with guarded body gates.
Completion waits for cancellation, a watch subscription, or the excess-wait
refusal, respectively. This removes slow-runner races without weakening
exit-130, publication, event, byte-content or capacity assertions. All 10
queue tests, 3 MCP tests and their strict Clippy checks pass after those
test-only repairs; independent bounded reviews approve them. CI uses the
validated Rust 1.98.1 toolchain.
The HTTP/2 per-connection fixture uses 16 MiB so delayed CI sampling can
observe its baseline and both required 15% gains. Its proportional speed
target, two-request/socket limits and exact-content assertions are retained.
Both HTTP/2 scheduler tests and strict scheduler Clippy pass; an independent
bounded review approves the test-only adjustment.
