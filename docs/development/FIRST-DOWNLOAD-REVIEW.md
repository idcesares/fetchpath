# FP-009 implementation review

Date: 20 September 2026

An independent read-only review held the first implementation for six issues. The corrected slice was then checked against each finding and expanded with adversarial tests.

| Finding | Resolution |
|---|---|
| HTTP error or partial bodies could publish | Publication now requires final status 200; 404, 500, unsolicited 206, and truncated-body tests leave no destination or staging file |
| A disk write error could pause forever | The callback captures the I/O error and returns a short write, aborting the transfer; no pause path remains |
| Cancellation raced publication | Cancellation admission and hard-link publication use one gate with an observable winner; job state includes `Cancelling` and late cancellation returns `TooLate` |
| Cleanup errors misreported the outcome | All prepublication exits use one cleanup policy and carry a retained path when deletion fails; postpublication cleanup is a successful download with `staging_cleanup_pending` |
| CLI cancellation was unreachable | Ctrl-C now requests cancellation through `FileJob` and waits for its terminal cleanup |
| The blocking primitive lacked the FP-009 job surface | `FileJob` now supplies UUID identity, create/start/cancel, revisions, snapshots, ordered process-local events, progress snapshots, duplicate-start rejection, and join |
| Hand-written CLI JSON missed control characters | CLI output now uses `serde_json` |

The task still does not claim durable command/event recovery. That work stays with the persistence and restart slice. This review covers the process-local FP-009 contract, transfer correctness, and publication boundary.

Validation commands:

- `cargo fmt --all -- --check`
- `cargo check --workspace --locked`
- `cargo clippy --workspace --all-targets --locked -- -D warnings`
- `cargo test --workspace --locked`
- `cargo build --workspace --locked`
- `node work/fp009-smoke.mjs`
- `node --test`
- `node tools/tasks.mjs check`
- `git diff --check`
