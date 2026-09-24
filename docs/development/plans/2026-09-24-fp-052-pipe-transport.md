# FP-052: protocol v1 over an authenticated per-user named pipe

**Goal:** Carry protocol v1 between the engine and its clients over a Windows named pipe that only the current user can reach, with mutual proof of a per-install secret and per-connection limits, as [the platform design](../../architecture/specs/2026-09-24-engine-platform-design.md) §3 and §6 require.

**Constraints:** Unsafe code only in one FFI module, each call wrapped with its safety argument. No new third-party packages where locked ones serve. A bad peer affects only its own connection. Independent strong-model review of the FFI and the handshake before done.

## Decisions

- **Name:** `\\.\pipe\fetchpath-engine-v1-<SID>-<random>`, fresh per engine run and published in a private endpoint file. (First planned as the SID alone; the review showed a predictable name can be claimed first, including by a lower-integrity process that lists pipes.)
- **Security:** protected DACL for the user's SID only; `PIPE_REJECT_REMOTE_CLIENTS`; `FILE_FLAG_FIRST_PIPE_INSTANCE` on the first instance, so an earlier claimant makes the engine refuse to start. The default medium no-write-up label covers lower-integrity writers; the secret file adds an explicit no-read-up label, because by default a low-integrity process can read a medium file.
- **Handshake:** client nonce, then server nonce and server proof, then client proof, then welcome; HMAC-SHA256 (`hmac` 0.13, `sha2` 0.11, already locked) with per-direction labels; nonces from `getrandom`. The client verifies the engine first. JSON handshake frames on the same framing, outside the protocol schema.
- **I/O:** overlapped reads and writes with deadlines; a timeout cancels and then waits for the operation to settle, so buffers stay valid, and keeps bytes that arrived just before the cancellation. A timeout before a frame starts is a quiet period, not an error.
- **Limits:** frame size, pending commands, connections (counted from accept, so unauthenticated floods count), handshake, frame, idle and write deadlines.
- **Client:** `PipeClient` for one connection; `PipeEngineClient` implements `EngineClient`, reusing one command connection (reopened once on loss with the same envelope) and a connection per subscription.

## Steps

1. FFI wrappers: handles, SDDL descriptors, SID lookup, pipe creation and connect, overlapped read and write with deadlines, client open with busy wait, security read-back for tests.
2. Secret file and handshake crypto, with an RFC 4231 HMAC vector and reflection and replay unit tests.
3. Listener, pending and authenticated connections, sender; client and `EngineClient`.
4. Tests against real pipes for every item in the task's verification, including a child process killed mid-frame.
5. Mutation-check the security properties; repeat the suite for flakiness.
6. Record in [the engine protocol record](../ENGINE-PROTOCOL.md); strong review.
