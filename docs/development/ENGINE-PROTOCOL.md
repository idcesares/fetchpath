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
