# FP-011 checkpoint and recovery evidence

Date: 20 September 2026
Acceptance: A02, A03, A04
Durability profile: `recoverable`

## Delivered behavior

- A destination-scoped storage session keeps payload bytes in a same-volume staging file and checkpoint metadata in checksum-protected, immutable generations. A newer generation is visible only after its temporary metadata file is written, flushed, synced, and renamed.
- Payload bytes are written and synced before a checkpoint can cover them. Recovery truncates bytes beyond the last committed range and verifies the retained range with its local SHA-256 before reuse.
- Checkpoint identity persists a redacted source key, not the URL query or fragment. Only a strong quoted ETag authorizes an HTTP suffix request. Weak or missing validators restart from byte zero.
- A resumed response is appended only when status `206`, `Content-Range` start, byte count, total length when present, and ETag agree. Changed validators, a full `200` response to a range, malformed ranges, and truncated responses never append to retained bytes; they restart or fail safely.
- Publication records a synced intent before crossing the create-only hard-link fence. A restart reconciles an already-created destination by length and SHA-256 before removing staging and metadata. An unrelated existing destination remains a conflict and is never replaced.
- Fault injection is exposed at payload write/flush, metadata write/flush/commit, and publication fence/reconciliation boundaries. The disk-full case uses `ErrorKind::StorageFull`.

## Evidence

`cargo test --workspace --locked --offline` passed:

- 20 `fetchpath-core` tests, including matching-validator resume, changed ETag, ignored Range, truncated resumed response, disk full, payload flush, metadata commit, publication-after-fence restart, cancellation, and destination races.
- 3 `fetchpath-storage` tests, including metadata generation fallback after write/flush/commit faults, retained-byte corruption detection, and publication reconciliation.
- 3 desktop command-path tests plus all workspace doc tests.

`cargo clippy -p fetchpath-storage -p fetchpath-core --all-targets --offline -- -D warnings` passed.

`node tools/tasks.mjs check` passed after the task evidence update.

## Durability boundary

This proves process/application interruption behavior with deterministic fault injection and a fresh invocation of the public download path. It does not claim OS-crash or power-loss durability. Directory-entry persistence and supported-filesystem power testing remain prerequisites for the contract's future `durable` profile.
