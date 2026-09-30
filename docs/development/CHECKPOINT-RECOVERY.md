# FP-011 checkpoint and recovery evidence

Date: 20 September 2026, updated 30 September 2026 for FP-085
Acceptance: A02, A03, A04
Durability profile: `recoverable`

## Delivered behavior

- A destination-scoped storage session keeps payload bytes in a same-volume staging file and checkpoint metadata in checksum-protected, immutable generations. A newer generation is visible only after its temporary metadata file is written, flushed, synced, and renamed. The store remembers the generation it wrote last, so a commit does not scan the parent folder; the persisted format is unchanged.
- Payload bytes are written and synced before a checkpoint can cover them. Recovery truncates bytes beyond the last committed range and verifies the retained range with its local SHA-256 before reuse.
- Checkpoint identity persists a redacted source key, not the URL query or fragment. Only a strong quoted ETag authorizes an HTTP suffix request. Weak or missing validators restart from byte zero.
- A resumed response is used only when status `206`, the `Content-Range` start, the total length and the ETag agree with the checkpoint, and all of it is checked before the first byte is written. The total must equal the checkpoint's `expected_total`. A changed validator or total, a `200` or `416` in answer to a range, and a malformed range restart from byte zero once (as one plain request) and discard the retained bytes before any new byte is written. A truncated response cannot publish an incomplete file and retains only its validated checkpoint on recovery. A download makes at most two attempts: after a transport failure the second resumes from the committed prefix.
- Publication records a synced intent before crossing the create-only hard-link fence. A restart reconciles an already-created destination by length and SHA-256 before removing staging and metadata. An unrelated existing destination remains a conflict and is never replaced.
- Fault injection is exposed at payload write/flush, metadata write/flush/commit, and publication fence/reconciliation boundaries. The disk-full case uses `ErrorKind::StorageFull`.

### Stream-first writes and the loss bound (FP-085)

- Lanes write at their own offsets, out of order, with positional writes that loop over short writes. A checkpoint covers only the contiguous prefix `[0, committed_len)` and its SHA-256, exactly as before. The digest is extended as the prefix grows: bytes that land at the prefix are hashed from the buffer, and bytes that arrived earlier and are now covered by the prefix are read back from staging. The set of written extents starts as `[0, committed_len)` on each attempt. Completion is never inferred from the file's length, because after a retry stale bytes of the same identity can sit past the prefix.
- A checkpoint is queued when at least one second has passed and at least 64 KiB more of the prefix is contiguous. A commit thread fsyncs the payload through an independently opened staging-file handle and writes the record. This avoids serializing positional writes behind a flush on the same Windows file object; storage contention can still delay writes. A newer checkpoint replaces one that has not started. An error on that thread stops the transfer with a storage error.
- **What a pause or crash discards.** Recovery keeps only the committed prefix. Non-prefix claims and callback writes beyond the contiguous written prefix are bounded by 3 s of recent aggregate goodput, with a 64 KiB floor; file size and minimum range size cannot enlarge it. Slow links retain the prefix stream until another minimum range fits. With checkpoints finishing at the one-second cadence, interruption loses about 4 s of recent transfer or 64 KiB, whichever is larger. A blocked storage flush can extend checkpoint lag; this timing estimate is not a strict bound under arbitrary storage stalls. Before FP-085 the loss was at most one round held in memory (up to 4 ranges, 32 MiB).
- **Pause and cancel.** Pause is a cancel that keeps staging. The scheduler stops every lane, the commit thread finishes, and only then the payload is synced and the prefix committed, all before `download_with_faults` returns. A cancel that removes staging skips the sync and the commit, since nothing can resume, but it also waits for every thread. So the removal always runs last and no thread can recreate a checkpoint after it.
- Bytes past the prefix are untrusted. The receiver does not preallocate the file: FP-084 measured that setting the length first does not avoid NTFS zero-fill, and the scheduler places claims near the prefix instead.

## Evidence

`cargo test -p fetchpath-http -p fetchpath-core -p fetchpath-storage --locked` validates the affected crates. Tests that pin this behavior:

- `fetchpath-core` (`transfer.rs`): matching-validator resume, changed ETag, ignored Range, truncated resumed response, disk full, payload flush, metadata commit, publication-after-fence restart, cancellation, destination races (all as before FP-085); a resume of a large remainder uses ranges; a total that differs from the checkpoint restarts from zero; a `200` on a resume discards the prefix before the first write; a dropped connection resumes from the committed prefix; fault-injected interruption checks checkpoint cadence and accounts for all accepted bytes beyond the last durable prefix; pause commits the prefix and resume continues from it; a cancel that removes staging leaves no metadata behind.
- `fetchpath-core` (`checkpoint.rs`): the prefix digest follows out-of-order bytes by read-back; a byte delivered twice is refused; the commit thread keeps the newest queued checkpoint and reports a failed commit.
- `fetchpath-storage`: metadata generation fallback after write/flush/commit faults, retained-byte corruption detection, publication reconciliation, the remembered generation keeps one metadata file, positional writes land out of order.
- `fetchpath-http`: clipping at a claim end, one test per first-response rule, cancellation at different moments, stalls and retries (see [adaptive HTTP](ADAPTIVE-HTTP.md)).

The [forced process-stop record](evidence/http-adaptive/fp085-interruption.json)
shows exact checkpoint-offset resume and final hash verification without
graceful commit draining. The repeatable scratch harness is
`node tools/bench/interruption.mjs`. This remains a process interruption test.

`node tools/tasks.mjs check` validates the task evidence paths.

## Durability boundary

This proves process/application interruption behavior with deterministic fault injection and a fresh invocation of the public download path. It does not claim OS-crash or power-loss durability. Directory-entry persistence and supported-filesystem power testing remain prerequisites for the contract's future `durable` profile (FP-077).
