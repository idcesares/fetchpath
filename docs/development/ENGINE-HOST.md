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
| Stopping | Once it decides to stop (idle, `EngineShutdown`, a restart request, Ctrl+C, or a failure after startup began), every further command is refused with `contract.engine_unavailable`, the pipe closes, the session starts no more jobs, and running work is cancelled to its checkpoints and saved, as quitting the desktop does |
| Launch or attach | `launch::attach_or_launch` connects and asks for `EngineStatus` (a stopping engine answers unavailable), or starts `fetchpath engine` detached with no console window, in its own folder, outside the caller's job object where allowed. It retries for up to 10 seconds, treats a dropped handshake as transient, and starts the engine again each second while none answers, so a client arriving while an engine winds down reaches the next one. Racing clients may start two engines; one keeps the lock and both clients reach it |
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

- **Not for release yet.** Until FP-055 moves the desktop onto the engine,
  the two exclude each other through the shared lock: while one runs, the
  other exits (the desktop without a message, since it looks for a window
  that does not exist). No build given to people should offer the engine
  before FP-055, as the design requires.
- Browser captures are not ingested by the engine yet (FP-056); they wait in
  the inbox.
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

- **Release constraint unchanged.** Until FP-055 the desktop keeps its own
  queue; while it runs the engine cannot take the lock, so `download` and
  every queue command fail with exit 1 ("did not start in time"). The user
  guide says to close the app. No build goes to people before FP-055.
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
