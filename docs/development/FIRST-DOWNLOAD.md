# First real download slice (FP-009)

Checked on native Windows on 20 September 2026. This is the first production vertical slice: a sequential `http://` or `https://` URL flows through an in-memory file job, the Rust transfer core, a unique staging file, a bounded libcurl callback buffer, an observed SHA-256 digest, and a no-overwrite publication step. It implements the narrow FP-005 contract subset for one file source; it does not implement persistence, resume, the durable command ledger, expected-digest verification, range assembly, or H2/H3.

## Behavior

`FileJob` creates a lowercase UUID job in `Queued`, starts one worker exactly once, exposes snapshots and ordered state events, updates received bytes as an ephemeral snapshot field, accepts cancellation in `Queued` or `Running`, and offers a join point. Each state mutation increments `job_revision`; each process-local event has the job ID, revision, and gap-detecting `seq`. The implemented path is `Queued → Running → Completed`, `Queued → Cancelled`, `Running → Cancelling → Cancelled`, or `Running → Failed`. Events are in memory in this slice; the durable command and event transaction remains future work.

The lower-level `DownloadRequest` carries the cancellation token and cleanup policy. It creates staging in the destination directory with create-new semantics. libcurl writes sequentially through a 16 KiB configured receive buffer; each chunk is written, counted, and added to an SHA-256 hasher. A write failure records its I/O error and returns a short callback write so libcurl aborts; it is reported as `storage.failed`, never as a paused transfer. Only a final HTTP `200` response is eligible for publication; 4xx/5xx responses, unsolicited `206` bodies, and truncated responses never publish.

The file is flushed and then published with a same-directory hard link followed by removal of the staging link. Creating that hard link fails if the destination appeared after the initial conflict check, so Fetchpath never replaces an existing file. Cancellation and this link operation share a serialization gate: cancellation that obtains it first wins and cleanup runs; a successfully linked destination wins and a later cancellation returns `TooLate`. If removing the redundant staging link fails after successful publication, the operation still succeeds and reports the retained path in `staging_cleanup_pending`. Every prepublication error either removes staging or reports its retained path.

The resulting integrity state is `downloaded_observed`: the SHA-256 is what this client received, not publisher-authenticity evidence. Cancellation before publication always prevents publication. With `RemoveStaging`, it removes the unpublished staging file; with `RetainStaging`, it returns the staging path and leaves it for inspection. Neither retained file is treated as resumable yet.

The CLI exposes the slice as:

```powershell
target\debug\fetchpath.exe download URL DESTINATION
```

It writes serializer-produced JSON containing `result: downloaded_observed`, job ID, destination, byte count, observed digest, and an optional `staging_cleanup_pending` path. It rejects existing destinations instead of overwriting them. Ctrl-C requests cancellation through the same job and publication gate, then waits for cleanup and the terminal state.

## Validation

The workspace uses Rust 1.98.1 on `x86_64-pc-windows-msvc`. All commands used the installed Cargo binary and the task-local Cargo cache from the packaging spike:

```powershell
$cargo = Join-Path $env:USERPROFILE '.cargo\bin\cargo.exe'
$env:CARGO_HOME = Join-Path (Get-Location) 'work\http-backend-cargo-home'
& $cargo fmt --all -- --check
& $cargo check --workspace --locked
& $cargo test --workspace --locked
& $cargo clippy --workspace --all-targets --locked -- -D warnings
& $cargo build --workspace --locked
node work\fp009-smoke.mjs
```

Formatting, locked checks/build, and warning-denying Clippy passed. `cargo test --workspace --locked` passed 11 core tests covering streamed bytes/hash/publication; existing and concurrently-created destinations; removal and retention cancellation policies; queued and running job cancellation; UUID snapshots, revisions, ordered events, and duplicate start rejection; non-HTTP input; 404, 500, unsolicited 206, and truncated responses; and truthful cleanup reporting.

The end-to-end smoke run starts the existing 1 MiB local fixture and invokes the production CLI. It exited 0, reported 1,048,576 bytes, and produced observed SHA-256 `f616fddc4f999bd2b1f22fd8447eee9ecdbba01e70f43278c6021cea91af3cf4`, exactly matching the fixture's known hash. The raw local result is retained in ignored `work/fp009/smoke.json`.

## Limits carried forward

The core intentionally uses the static libcurl candidate proven in FP-004. That candidate reported HTTP/2 and HTTP/3 disabled, so this slice makes no HTTP/2, HTTP/3, throughput, or fallback claim. It is sequential only. Durable command/event recovery, redirect identity policy, expected hashes, resumable checkpoints, pause/resume, and restart recovery remain later slices. Hard-link publication is a no-replace fence on a same-volume filesystem that supports hard links; other filesystems need an explicit capability/fallback decision before release.
