# Engine host

This record covers `fetchpath engine`, the one owner of the queue behind the
authenticated pipe
([platform design](../architecture/specs/2026-09-24-engine-platform-design.md)
§5).

## FP-053: single owner, launch or attach, idle exit

Recorded 24 September 2026 on Windows 11 Pro 26200 x64. Plan:
[2026-09-24-fp-053-engine-host](plans/2026-09-24-fp-053-engine-host.md).
Code: `apps/cli/src/engine`, `crates/fetchpath-protocol/src/launch.rs`.

| Part | What it does |
|---|---|
| Data folder | `FETCHPATH_APP_DATA_DIR` when set, otherwise `%APPDATA%\app.fetchpath.desktop`. The engine adds its secret and endpoint files beside the session's |
| One owner | Holds the desktop's `instance.lock` with a deny-sharing handle. A second engine exits at once with status 0. Unlike the desktop, any other failure to open the lock stops the engine, because two owners corrupt the queue |
| Startup | Lock, then the session (restart recovery runs as it loads), then one reconcile, then the secret, a fresh pipe name, the pipe, and last the endpoint. No client can reach it before recovery is done |
| Serving | A thread per connection. Commands are answered in order; a subscription gets a forwarding thread for events and progress, and its connection's idle limit is lifted. `EngineShutdown` stops the engine after replying |
| Staying up | While a client is connected, or a job is queued, scheduled (including an automatic retry) or running. Otherwise it stops after a 60-second grace period, cancelling running work to its checkpoints and saving, as quitting the desktop does |
| Launch or attach | `launch::attach_or_launch` connects, or starts `fetchpath engine` detached with no console window, outside the caller's job object where allowed, then retries for up to 10 seconds. Racing clients may start two engines; one keeps the lock and both clients reach it |
| Versions | Another protocol major is refused with `contract.unsupported_version`. `launch::request_restart` stops the engine through the handshake (`hello` with `intent: "restart"`), which works across protocol versions and needs the secret |
| Sign-in start | Setting `startEngineAtSignIn`, off by default and written only when on. Changing it adds or removes `HKCU\...\Run` value `Fetchpath engine` = `"<fetchpath.exe>" engine`; the engine only ever adds it at startup, never removes it |
| Commands | `fetchpath engine`, `fetchpath engine status`, `fetchpath engine stop`. Left out of `--help` and the user guide until FP-055 |

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

- `cargo test -p fetchpath --test engine`: 5 passed, five consecutive runs
  clean: two engines started together (one owner; the other exits 0); a
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
