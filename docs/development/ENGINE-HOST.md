# Engine host

This record covers `fetchpath engine`, the one owner of the queue behind the
authenticated pipe
([platform design](../architecture/specs/2026-09-24-engine-platform-design.md)
§5).

## FP-053: single owner, launch or attach, idle exit

Recorded 24 September 2026 on Windows 11 Pro 26200 x64. Plan:
[2026-09-24-fp-053-engine-host](ARCHIVE.md).
Code: `apps/cli/src/engine`, `crates/fetchpath-protocol/src/launch.rs`.

| Part | What it does |
|---|---|
| Data folder | `FETCHPATH_APP_DATA_DIR` when set, otherwise `%APPDATA%\app.fetchpath.desktop`. The engine adds its secret and endpoint files beside the session's |
| One owner | Holds the desktop's `instance.lock` with a deny-sharing handle. A second engine exits at once with status 0. Unlike the desktop, any other failure to open the lock stops the engine, because two owners corrupt the queue |
| Startup | Lock, then the session (restart recovery runs as it loads), then one reconcile, then the secret, a fresh pipe name, the pipe, and last the endpoint. No client can reach it before recovery is done |
| Serving | A thread per connection. Commands are answered in order; a subscription gets a forwarding thread for events and progress, and its connection's idle limit is lifted. `EngineShutdown` stops the engine after replying |
| Staying up | While a client is connected (counted from acceptance, before its handshake), or a job is queued, scheduled (including an automatic retry) or running. Otherwise it stops after a 60-second grace period |
| Stopping | Once it decides to stop (idle, `EngineShutdown`, a restart request, Ctrl+C, or a failure after startup began), it removes its endpoint file at once, while it still holds the lock (FP-055: a client that finds no endpoint knows the stop was deliberate); every further command is refused with `contract.engine_unavailable`, the pipe closes, the session starts no more jobs, and running work is cancelled to its checkpoints and saved, as quitting the desktop does |
| Launch or attach | `launch::attach_or_launch` connects and asks for `EngineStatus` (a stopping engine answers unavailable), or starts `fetchpath engine` detached with no console window, in its own folder, outside the caller's job object where allowed, inheriting no handles (FP-055 found that an inherited pipe kept a script reading a client's output waiting for the engine's idle grace). It retries for up to 10 seconds, treats a dropped handshake as transient, and starts the engine again each second while none answers, so a client arriving while an engine winds down reaches the next one. Racing clients may start two engines; one keeps the lock and both clients reach it |
| Versions | Another protocol major is refused with `contract.unsupported_version`. `launch::request_restart` stops the engine through the handshake (`hello` with `intent: "restart"`), which works across protocol versions and needs the secret |
| Sign-in start | Setting `startEngineAtSignIn`, off by default and written only when on. Changing it adds or removes `HKCU\...\Run` value `Fetchpath engine` = `"<fetchpath.exe>" engine`, serialized with the change; the engine only ever adds it at startup, never removes it, and touches it only when using the default data folder. On the wire the field is optional and absent means unchanged |
| Commands | `fetchpath engine`, `fetchpath engine status [--json]`, `fetchpath engine stop`. In `--help` and the user guide since FP-058; see its section for the release constraint |

### Decisions on the design's open points (§12)

- **Idle grace: 60 seconds.** Long enough that a person closing one window
  and opening another does not restart it.
- **What keeps it up.** "Non-terminal" is read as "has work of its own".
  Paused jobs and failures waiting for a person do not keep it running:
  nothing happens to them until a person acts, which starts a client, which
  starts the engine, which restores them as they were.
- **Sign-in start wording** is left to the settings surfaces (FP-055,
  FP-062); the setting exists and is off.

### Commands and results

- `cargo test -p fetchpath --test engine`: 6 passed, repeated runs clean. The
  sixth, added after review: once a shutdown is acknowledged another client's
  command is refused, and a client arriving meanwhile gets a fresh engine.
  The other five: two engines started together (one owner; the other exits 0); a
  client with no engine starts a windowless one and a second client reaches
  the same engine; idle exit after the grace period, a scheduled job and a
  listening client each keeping it up; an engine killed during a download
  and started again finishing it from its checkpoint (the server saw an
  `If-Range` resume, and the file matches); another protocol version refused
  and a restart request stopping it, the next attach starting a new engine
  under a new pipe name.
- `cargo test --workspace --locked`, `cargo clippy --workspace --all-targets --locked -- -D warnings`, `cargo fmt --all --check`, `node --test` (39): clean. No test process was left running.
- `Cargo.lock` gained only dependency edges; notices unchanged.

### Mutation checks

| Temporary change | Failing test |
|---|---|
| Lock opened with sharing | `two_engines_started_together_leave_exactly_one_owner` |
| Own work ignored | `the_engine_leaves_when_idle_but_stays_for_a_scheduled_job` |
| Restart request ignored | `another_protocol_version_is_refused_and_can_still_restart_the_engine` |
| Connections not counted | The suite hangs (the counter wraps), so the tests do not pass |

Removing the save on stop changes nothing a test sees: a row saved as
running is restored as queued and resumed anyway.

### Limitations

- Since FP-055 the desktop is a client and holds only its own window lock;
  since FP-057 setup stops a running engine before replacing files.
- The engine takes in browser captures on every tick, but only while it
  runs; the browser host still wakes the desktop window, which starts the
  engine, rather than the engine itself (FP-056).
- The engine's own diagnostics go to standard error, which is discarded
  when a client starts it.

### Independent review

Strong-model review, 24 September 2026: **approve with nits**. Ownership and
recovery ordering confirmed sound: the fail-closed lock excludes a second
engine and the desktop; a losing engine touches nothing; the endpoint is
published last; a stale endpoint reads as unavailable; a restart request is
honored only after authentication. Findings and fixes:

| Finding | Fix |
|---|---|
| Medium: connections kept carrying out commands after shutdown began (104 in a probe), which could start queued jobs during the wind-down | A stopping engine refuses every command; the pipe closes before work is cancelled; `Session::halt` stops new starts. Test: `once_stopping_the_engine_refuses_commands_and_a_new_client_gets_a_new_engine` (fails with the refusal removed) |
| Low-medium: a client arriving during the wind-down got a hard error; a connection accepted in the last tick was not counted | Connections counted from acceptance; `attach` probes with `EngineStatus`; dropped handshakes are transient; the engine is started again each second while none answers. Same test |
| Low: two settings changes could race the Run key | Serialized |
| Low: a client omitting the sign-in field turned it off | Optional on the wire; absent keeps the current value |
| Low: a failure after the first reconcile, or Ctrl+C, skipped the clean stop | Both wind down |
| Nits | The launched engine runs in its own folder; the Run key is touched only with the default data folder |
| Paused jobs do not keep it up though "non-terminal" | Lead decision, above; the task's acceptance now says "has work of its own" |

Not changed: a subscription forwarder for a client that has gone stays until
its next send fails or the engine stops; if the desktop holds the lock, a
launching client reports that the engine did not start in time (until
FP-055).
## FP-058: the command line through the engine

Recorded 25 September 2026 on Windows 11 Pro 26200 x64. Code:
`apps/cli/src/{client,queue,wait,when}.rs`, `apps/cli/src/download.rs`;
tests `apps/cli/tests/queue.rs`. User guide: [CLI](../user/CLI.md).

| Part | What it does |
|---|---|
| Connection | Every queue command calls `launch::attach_or_launch` with its own executable, so the first command starts the engine. `engine status` only attaches |
| Commands | `add`, `batch`, `ls`, `show`, `pause`, `resume`, `cancel`, `retry`, `rm`, `watch [JOB]`, `inspect`, `history`, `settings [NAME [VALUE]]`, `engine status/stop`. Each is one or a few protocol commands; no queue logic in the client |
| References | Up to four digits is a 1-based index in `ls` order (the engine's, newest first); anything else is an id prefix that must match one job. All references of one command resolve against one listing |
| `download` | `CreateJob` then follow the job, keeping its arguments, streams, `--json` record, `--quiet` and exit codes. Relative destinations are made absolute in the client, since the engine runs in its own folder. Ctrl+C sends `Cancel` and waits for the outcome |
| Waiting | `download`, `add --wait` and `watch JOB` subscribe from the snapshot's `last_seq`, re-read the job on events and every second, draw progress from samples, and reattach (starting an engine) if the stream breaks. Settled = completed, cancelled, waiting for a link, or failed with no automatic retry due |
| Exit codes | From the error code only: `input.*`, unknown job, invalid transition → 2; `*.destination_conflict` → 3; `source.*`, `auth.*` → 4; `integrity.*` → 5; `storage.*` → 6; cancelled → 130; engine unreachable or `internal.*` → 1. `download` keeps its own table (unknown codes → 4) and adds 1 for `contract.*` |
| `--json` | Protocol types as sent: one `CommandResult` per line (per job for multi-job commands), `ServerMessage` events and progress for `watch`, `{"error": ProtocolError}` on failure. `download --json` keeps its 0.1.0 record |
| `--at` | `HH:MM` (next occurrence), `YYYY-MM-DD HH:MM` local through the Windows time-zone rules, `+Ns/m/h/d`, or RFC 3339 UTC |

### Decisions

- **`download` waits through automatic retries.** It is "add, then wait" in
  the shared queue, so the person's `auto-retry` setting applies to it as to
  any job; each retry is noted on standard error. Exiting at the first failure
  would leave the engine retrying behind a script that was told it failed.
- **Human output is not a contract; `--json` and exit codes are.** Tests pin
  those, not table layout.
- **`engine status` drops the client count** from its plain output: it counts
  subscriptions whose client has gone but not yet been noticed (FP-053
  limitation); `--json` still carries it.

### Commands and results

- `cargo test -p fetchpath --test queue`: 4 passed, each against its own
  engine and data folder over the pipe: exit codes 0/2/3/4/5/6 for
  `download` and 130 for a download (and a `watch`) cancelled from another
  client; `--json` shapes of `download`, `ls`, `history`, `show`, `add`,
  `pause`, `resume`, `cancel`, `rm`, `retry`, `watch`, `settings`, `inspect`,
  `engine status`; index and prefix references; `batch` from a file;
  `add --wait`; default destination. Twelve consecutive runs with the engine
  suite clean after fixing a test race (polling before the other process had
  created its job).
- Unit tests: job references, exit-code mapping, `--at` parsing.
- `cargo test --workspace --locked`, `cargo clippy --workspace --all-targets --locked -- -D warnings`, `cargo fmt --all --check`, `node --test` (38 pass, 1 skipped): clean.
- `Cargo.lock` gained only the `serde` edge for the CLI; notices unchanged.

| Temporary change | Failing test |
|---|---|
| Relative destination passed through unchanged | `download_keeps_its_exit_codes_and_joins_the_shared_queue` |
| A cancelled job ends a wait with 1 instead of 130 | `a_download_cancelled_from_another_client_exits_130` (the `watch` it runs) |

### Limitations

- A 0.1.0 desktop still running (it held `instance.lock` itself) keeps a
  new engine from starting until it exits. Setup closes it on upgrade
  (FP-057); only a 0.1.0 window left running from another folder would do this.
- Ctrl+C itself is not exercised by a test (a console control event cannot
  be sent to a child without a shared console); the 130 path is covered by a
  cancellation from another client, which ends the wait the same way.
- `inspect` and `--quality` depend on configured media tools; without them
  the engine refuses with `input.invalid_request` and desktop wording.
- `cancel` on a failed job with an automatic retry due answers "already
  ended" and does not stop the retry; `rm` does.
- A batch is one `CreateJob` per line, so a bad line does not stop the others
  (unlike the desktop's all-or-nothing batch).
- `lan`, `cache` and `fetch-verified` are unchanged (FP-032/FP-033).

## FP-057: setup with a running engine

Recorded 25 September 2026 on Windows 11 Pro 26200 x64. Code:
`apps/desktop/src-tauri/installer-hooks.nsh`, `apps/cli/src/engine/mod.rs`
(`stop_for_update`), `crates/fetchpath-protocol/src/launch.rs` (update hold,
`is_default`), `apps/cli/src/wait.rs`. Tests: `apps/cli/tests/engine.rs`,
`tests/installer/engine-stop.test.mjs`,
`tests/compatibility/windows/engine-lifecycle.ps1`.

| Part | What it does |
|---|---|
| Stopping for setup | Before the bundler replaces or deletes any file, the install and uninstall hooks run the installed `fetchpath.exe engine stop --for-update`. It writes `engine-update-hold-v1` in the data folder, asks the engine to stop through the restart handshake (any protocol version), and returns once the single-owner lock is free. The engine keeps that lock until it has saved the queue, so the save is done by then. With no data folder it does nothing and creates nothing |
| The hold | While the file is younger than ten minutes (a future time counts only as far as ten minutes), `attach_or_launch` answers at once with `contract.engine_unavailable` "Fetchpath is being updated or removed", and an engine started directly leaves without serving. Setup deletes it when it finishes, fails or is cancelled (`.onInstFailed`, `un.onUninstFailed` and Modern UI's cancel functions) |
| Other holders of the file | The hooks then wait up to 20 seconds for `fetchpath.exe` to open for writing. A terminal command such as `watch` also runs from it; under the hold it exits 1 with the explanation. After that, only the `fetchpath.exe` processes whose image is `$INSTDIR\fetchpath.exe` are ended. Setup is 32-bit, so they are found by WMI's `ExecutablePath`, not `Get-Process`. Ending one is a crash the engine recovers from |
| 0.1.0 | Its `fetchpath.exe` does not know the command (exit 2, ignored) and had no engine; the bundler closes its window, which held the queue lock |
| Uninstall | Also removes the `Fetchpath engine` Run value when it points at this install. Downloads, the queue, history and settings stay, as before |
| Sign-in start (fix) | FP-053 wrote the Run value only when `FETCHPATH_APP_DATA_DIR` was unset, but a client always sets it for the engine it starts, so the value was never written. The engine now compares its folder with `%APPDATA%\app.fetchpath.desktop` |
| `watch` (fix) | A refresh that met a stopping engine ended the wait with "The Fetchpath engine is stopping" instead of reconnecting as a broken event stream does (an FP-058 race found by the lifecycle run) |

### Commands and results

- `tests/compatibility/windows/engine-lifecycle.ps1`, with real per-user
  installs of a 0.1.0 installer built from `ce6615a` and of this tree as
  0.1.1 and 0.1.2 (`tauri build --config src-tauri/tauri.release.conf.json
  --config '{"version":"0.1.1"}'`), passed; see the
  [evidence](evidence/windows/engine-lifecycle.json).
  - From 0.1.0, with its window running and the FP-048 queue and settings,
    setup closed the window and left both files byte-identical. The engine
    served all 14 jobs, both completed ones with their hashes, plus history
    and settings.
  - Over a running engine 1 MiB into a 24 MiB download, with `watch`
    attached, setup took 3.3 s (the clean path; the fallback would take more
    than 20 s). The old engine was gone, and `watch` had exited 1 with the
    update message. The installed `fetchpath.exe` was the one the installer
    carried. After setup the server sent only the remaining 24,117,247 bytes,
    in one ranged request, and the file matched.
  - Uninstall ran with the engine mid-download, sign-in start on, and
    `fetchpath batch -` blocked on input both from the install folder and
    from a copy elsewhere. It took 25.6 s. The blocked command in the install
    folder was ended and the copy was not. Nothing from the install folder
    ran 15 s later. The finished download, the queue (the interrupted job
    saved as queued with its bytes) and PATH were as expected, and the Run
    value and the hold were gone.
- `cargo test -p fetchpath --test engine`: 9 passed, three runs.
  `cargo test -p fetchpath-protocol --lib launch`: the default-folder test.
  `node --test tests/installer`: the hook guards. The generated installer
  script compiles with `makensis -WX`.
- In three workspace runs,
  `a_subscriber_that_never_reads_cannot_hold_up_the_queue` failed the same
  way on `e42bde5` without these changes: about 92 s against its 60 s budget.
  A later run took 42 s. Tracked as FP-072.

| Temporary change | Failing test |
|---|---|
| Clients ignore the hold | `stopping_for_an_update_waits_for_the_engine_and_holds_new_ones_off` |
| `stop --for-update` returns without waiting for the lock | None reliably. The engine drops its listening pipe before it saves the queue, so the lock wait is what guarantees the save before setup continues; the test only catches the change when the save loses the race, and the hooks' executable wait is the second guard |
| The fallback matches `Get-Process` `Path` (the first version) | The lifecycle run's uninstall timed out with the blocked command still running |

### Limitations

- `fetchpath-browser-host.exe` is not waited for, so a browser keeping it
  open during setup can still leave a file busy (FP-056 changes the host).
- Clients older than this build do not know the hold. The loop stops an
  engine one of them starts during setup, and the hook ends it if it keeps
  the file busy.
- If policy blocks PowerShell or WMI, the fallback ends nothing. The bundler's
  file copy then fails on the busy `fetchpath.exe`: interactively with a retry
  dialog, silently by aborting, which lifts the hold.
- An upgrade that uninstalls the old version first removes the Run value
  until the engine next starts and adds it back.
- With `FETCHPATH_APP_DATA_DIR` set in the user's environment (development
  only), the hold is written there and setup deletes a different file, so it
  lapses after ten minutes.
- Only the silent path ran. The interactive installer, whose reinstall page
  runs the old uninstaller first, a clean machine, and a 0.1.0 window with a
  download running were not exercised.

### Independent review

Strong-model review, 25 September 2026: **approve with nits**. It confirmed
that files are never replaced while the engine runs, the queue is saved
first, no two engines own it, the lock probe is harmless, 0.1.0's unknown
subcommand is handled, and the NSIS mechanics hold. Findings and fixes:

| Finding | Fix |
|---|---|
| Medium: a cancelled or failed setup left the hold for ten minutes | Deleted in the failure and cancel callbacks; a future time is honored only up to the limit |
| Medium-low: the fallback ended every `fetchpath.exe` by name | Ended by path; the lifecycle run proves the copy elsewhere is spared |
| Low: the record misexplained the surviving mutation | Corrected above |
| Low: the script could not tell a clean stop from the fallback | Phase B asserts under 20 s; phase C exercises the fallback |
| Nits: a stray control byte in the record, the development-only hold location | Fixed; listed above |

The run that exercised the fallback found it ended nothing (32-bit
PowerShell), and a later run found the `watch` race; both are fixed.
