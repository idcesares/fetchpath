# Engine protocol

This record covers protocol v1, the wire form of
[the job contract](../architecture/JOB-CONTRACT.md) between the engine and
its clients ([platform design](../architecture/specs/2026-09-24-engine-platform-design.md)
§4).

## FP-050: protocol v1 and the `EngineClient` interface

Recorded 24 September 2026 on Windows 11 Pro 26200 x64. Plan:
[2026-09-24-fp-050-protocol-v1](plans/2026-09-24-fp-050-protocol-v1.md).

`crates/fetchpath-protocol` defines the messages, framing, schema and client
interface. It holds no queue logic and no transport, and does not depend on
the session: the engine maps between session and wire types (FP-051, FP-053).

| Part | What it is |
|---|---|
| Identifiers | `ClientId`, `CommandId`, `JobId`, `AttemptId`, `CredentialRef`: separate types, lowercase UUID strings only. `Timestamp`: strict RFC 3339 UTC ending in `Z`, millisecond precision |
| Commands | `CommandEnvelope` with `schema_version`, `client_id`, `command_id`, `issued_at`, optional `expected_revision`. The contract's commands plus `RemoveJob`, queries, `SubscribeJob(after_seq)`, `SubscribeQueue(after_cursor)`, `EngineStatus`, `EngineShutdown`. `Command::is_mutating` separates what goes through the ledger |
| Replies | `Reply` with the `command_id` and either a `CommandResult` or a `ProtocolError`. `Pause` and `Cancel` return the matrix outcome (`accepted`, `no_op`, `too_late`, `too_late_to_cancel`, `already_terminal`, `cancel_in_progress`) |
| Events | Durable `JobEvent` with per-job `seq`, engine-wide `cursor`, `job_revision`, the contract's event families as `kind` and `public_payload`, and `correlation`. Ephemeral `ProgressSample` with `sample_cursor`, which never consumes `seq` |
| Snapshots | `JobSnapshot` in the contract's states; `scheduled` is `queued` with `not_before`, `needs_source` is `waiting_for_source` with a reason. Each carries `job_revision` and `last_seq` |
| Errors | Stable `code` (validated `family.name`), `message_key`, redacted `message`, `retryable`, `action`, `scope`, optional `retry_after_seconds` and `current_revision` |
| Links | `SensitiveUrl` carries the link whole but redacts it in `Debug` and display |
| Framing | 4-byte little-endian length, then UTF-8 JSON; `MAX_FRAME_BYTES` is 4 MiB; checked before allocation. `read_frame`/`write_frame` for streams, `FrameDecoder` for pieces |
| Schema | `schema/protocol-v1.schema.json`, JSON Schema 2020-12, keys sorted; a test fails when it drifts |
| Client | `EngineClient` (`execute`, `subscribe`, `send`) and `EventStream`, object-safe; implementations arrive in FP-051 (in-process) and FP-052 (pipe) |

### Compatibility rules

- `schema_version` is the major version. It is read before anything else, so
  a peer on another major gets `contract.unsupported_version` with the
  `update_software` action and, when readable, its `command_id`.
- Readers ignore fields they do not know. States, actions, scopes, kinds and
  other closed sets have an `unknown` value, and an unknown durable event
  kind reads as `Unknown` with its `seq` kept, so a newer engine does not
  break an older client or open a gap in its stream.
- An unknown command type is refused with `contract.unknown_command` and its
  `command_id`, not as unreadable. A known command with a bad field is
  `contract.malformed_message`.
- The contract gained the codes `contract.malformed_message`,
  `contract.message_too_large`, `contract.unknown_command`,
  `contract.unknown_job` and `contract.engine_unavailable` (§9), and its §12
  create-job example now shows the v1 wire form.

### Decisions

- **The link crosses the protocol whole.** A pasted signed link has no stored
  secret to refer to, so `CreateJob` carries it as a `SensitiveUrl`. The
  engine must keep only a fingerprint in the ledger and persist the link only
  when it has no query or fragment, as the queue does today (FP-051). A
  browser capture refers to its stored context by `credential_ref` instead.
- **Settings are a wire type of their own** (`EngineSettings`), mirroring the
  session's, so the protocol does not depend on the session.
- **Destination is a full path** with a conflict policy (`ask` or
  `replace_existing`), as the queue takes it today. Directory references and
  network profiles are additive later.
- **Not in v1 yet**: principals and approvals (FP-054), rules (FP-064), and
  the browser's link-review handoff, which becomes an event with FP-055.
  The desktop's "retry with a corrected checksum" is not a contract command,
  because the contract makes the expected identity immutable. FP-055 decides
  between a replacement job and a contract amendment.

### Commands and results

- `cargo test -p fetchpath-protocol`: 20 passed (6 unit, 14 integration).
- `cargo test --workspace --locked`: all passed; the schema drift test also
  passes under the whole workspace's feature set.
- `cargo clippy --workspace --all-targets --locked -- -D warnings`: clean.
- `cargo fmt --all --check`: clean.
- `node tools/licenses/generate.mjs`: 568 packages (adds `schemars_derive`
  1.2.2, MIT, and `serde_derive_internals` 0.30.0, MIT OR Apache-2.0).

The schema test checks every sample message against the exported schema with
a small checker in the test file. It fails on any schema keyword it does not
implement, so it cannot silently skip a constraint. It does not check
`pattern` or `format`; the identifier and timestamp types enforce those
themselves, with their own tests.

### Mutation checks

Each change was made, `cargo test -p fetchpath-protocol --test protocol`
run, and the change reverted.

| Temporary change | Failing test |
|---|---|
| `"CreateJob"` renamed in the checked-in schema | `the_checked_in_schema_is_current` |
| Command version check removed | `another_major_version_is_refused_with_its_command_id` |
| Frame size cap removed | `oversize_empty_and_truncated_frames_are_refused_before_allocation` |
| Unknown-command check removed | `malformed_json_and_unknown_commands_are_refused_with_contract_codes` |

The forward-compatibility test also caught a real gap during development:
serde's fallback for unknown enum values rejected an unknown event kind that
carried a payload. `JobEvent` now decodes the kind first and falls back to
`Unknown` itself.

## FP-052: the authenticated per-user named pipe

Recorded 24 September 2026 on Windows 11 Pro 26200 x64. Code:
`crates/fetchpath-protocol/src/pipe` (Windows only). Plan:
[2026-09-24-fp-052-pipe-transport](plans/2026-09-24-fp-052-pipe-transport.md).

| Part | What it does |
|---|---|
| Name | `\\.\pipe\fetchpath-engine-v1-<user SID>`, so users on one machine never share a name |
| Access | Protected DACL `D:P(A;;GA;;;<SID>)`: this user only, nothing inherited. Remote clients refused. No explicit label, so the default medium no-write-up label keeps lower-integrity processes from writing |
| Claim | The engine creates the first instance with `FILE_FLAG_FIRST_PIPE_INSTANCE`; if any process already holds the name it refuses to start (`contract.engine_already_running`) instead of running behind an impostor |
| Secret | 32 random bytes in a file the engine creates once, with `D:P(A;;FA;;;<SID>)S:(ML;;NRNWNX;;;ME)`: owner-only and unreadable by lower-integrity processes. Wiped from memory on drop; never printed. Where the engine keeps it is FP-053's choice |
| Handshake | `hello` (client nonce) → `challenge` (server nonce and server proof) → `proof` (client proof) → `welcome`. Proofs are HMAC-SHA256 under the secret over both nonces, with a different label per direction. The client checks the engine's proof before sending its own, so a squatter on the name learns nothing. Constant-time comparison. Handshake frames are transport, not protocol schema |
| Deadlines | Overlapped I/O throughout. Handshake 5 s in total; a started frame must finish in 10 s; idle connections close after 10 minutes (configurable, off for subscriptions); a write that cannot finish in 10 s closes the connection |
| Limits | Frame size (≤ 4 MiB), 32 unanswered commands, 64 connections authenticated or not (further clients are disconnected at once). Every limit is per connection; the engine keeps serving others |
| Client | `PipeClient` (one connection) and `PipeEngineClient`, the pipe `EngineClient`: commands share a connection that is reopened once on loss, resending the same envelope (the ledger makes that safe, FP-051); each subscription has its own connection |
| Refusals | An oversize, empty or unreadable frame is answered with its code and then closes that connection; an unknown command is answered and the connection continues |

The per-install secret defends against other users and lower-integrity
processes. A program already running as the same user at the same integrity
can read the secret and connect as the user; that is out of scope, as the
platform design says.

### Commands and results

- `cargo test -p fetchpath-protocol --test pipe`: 16 passed, 1 ignored (the
  helper process for the killed-client test), eight consecutive runs clean.
- `cargo test --workspace --locked`, `cargo clippy --workspace --all-targets --locked -- -D warnings`, `cargo fmt --all --check`: clean.
- `Cargo.lock` gained no package: `hmac` 0.13, `sha2` 0.11 and `getrandom`
  0.3 were already locked. Notices unchanged at 568.

| Verification item | Test |
|---|---|
| Wrong secret | `a_client_with_the_wrong_secret_learns_it_is_not_talking_to_its_engine`, `the_engine_refuses_a_wrong_proof_and_a_replayed_one` |
| Missing secret file | `the_secret_file_is_created_once_private_and_checked_on_load` (also damaged files and the file's access list and label) |
| Replayed handshake | `the_engine_refuses_a_wrong_proof_and_a_replayed_one` |
| Oversize and truncated frames | `an_oversize_frame_is_refused_and_closes_only_that_connection`, `a_truncated_frame_ends_that_connection_and_the_engine_carries_on` |
| Flood of connections | `a_flood_of_connections_is_capped_and_the_engine_recovers` |
| Client killed mid-frame | `a_client_killed_mid_frame_ends_only_its_own_connection` (a real child process, killed) |
| Other limits | `a_silent_connection_is_closed_at_the_handshake_deadline`, `an_idle_connection_is_closed_after_the_idle_limit`, `too_many_unanswered_commands_close_that_connection` |
| Access and claim | `only_this_user_may_open_the_pipe`, `a_second_engine_cannot_claim_the_pipe_name` |
| Client behavior | `an_authenticated_client_sends_commands_and_follows_events`, `a_lost_connection_is_reopened_and_the_same_command_resent`, `a_client_reports_a_missing_engine_plainly`, `accepting_with_no_time_left_reports_nothing_rather_than_failing` |

### Mutation checks

| Temporary change | Failing test |
|---|---|
| Engine accepts any client proof | `the_engine_refuses_a_wrong_proof_and_a_replayed_one` |
| Client trusts any engine proof | `a_client_with_the_wrong_secret_learns_it_is_not_talking_to_its_engine` |
| Server nonce fixed | `the_engine_refuses_a_wrong_proof_and_a_replayed_one` |
| First-instance flag dropped | `a_second_engine_cannot_claim_the_pipe_name` |
| Everyone allowed on the pipe | `only_this_user_may_open_the_pipe` |
| Connection cap removed | `a_flood_of_connections_is_capped_and_the_engine_recovers` |
| Pending-command limit removed | `too_many_unanswered_commands_close_that_connection` |
| Secret file's integrity label dropped | `the_secret_file_is_created_once_private_and_checked_on_load` |
| Zero-timeout fix reverted | `accepting_with_no_time_left_reports_nothing_rather_than_failing` |

### Found during development

- `GetOverlappedResultEx` with a zero timeout reports `ERROR_IO_INCOMPLETE`,
  not `WAIT_TIMEOUT`. Accepting with an expired deadline failed the whole
  listener, in about half of the flood test runs; it is now treated as a
  timeout, with its own test.
- A cancelled read can complete just before the cancellation reaches it; its
  bytes are now returned instead of dropped, so a quiet period on a
  subscription never loses part of a frame.
- A frame refused for its size or shape is now answered before the
  connection closes; the first version closed without saying why.