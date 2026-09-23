# Desktop checksum verification

Task FP-031. Recorded 23 September 2026.

## What a person can now do

Under **Advanced options** in Add link, paste the SHA-256 a publisher lists for
a file. Fetchpath saves the file only if its bytes match. The field accepts
surrounding spaces, either case, and a `sha256:` or `SHA256=` prefix. Anything
that is not then exactly 64 hex digits is refused with a plain explanation
instead of being guessed at. A blank field means no checksum.

A completed download with a checksum reads "SHA-256 · matches the checksum you
entered". The fine print says the file is exactly what the checksum describes,
to be trusted as far as the checksum's source is. It never claims who
published the file.

A mismatch saves nothing. The row reads "Needs attention", explains that the
file does not match, shows the expected and received values side by side, and
offers **Edit checksum** and **Retry**. **Edit checksum** reopens the composer
with the checksum filled in and the address optional, so a typo is corrected
without re-pasting a link that may carry private values. A mismatch is never
retried automatically: the checksum or the source is wrong, and a person has to
decide which.

The field is hidden for video and audio, which are assembled locally, so no
published checksum describes the output. A checksum is refused on a batch
because it describes one file.

## How it is enforced

The check sits in the engine, not the interface. `DownloadRequest` carries an
optional `expected_sha256`, and `crates/fetchpath-core/src/transfer.rs` compares
it at each of the three publication points:

1. **Normal completion**, after the final digest of the staged bytes. On a
   mismatch the staging file and checkpoint are removed, so no resume can build
   on known-bad bytes.
2. **Recovery of a pending publication intent**, before that intent is
   published.
3. **Recovery that finds the file already published** by a run that crashed
   before recording completion. The file is reported with its actual digest and
   **never deleted**: a published file is the person's.

Pause, resume, progress and the existing checkpoint path are unchanged. The
checksum is part of the queue row, persisted with it, and applied at every
place the desktop creates a file job: add, retry, resume, restart recovery,
automatic retry and shutdown recovery. Creating a job fails closed. A checksum
that cannot be read produces no job, never one that downloads unchecked. A
saved queue whose checksum is corrupted restores that row as "Needs attention",
not as a running download.

The guided media-tool setup now passes its recorded SHA-256 to the engine too,
so a mismatched helper is refused before it is published. The existing check
after download remains.

## Commands actually run

```
cargo test --workspace --locked --offline
cargo clippy --workspace --all-targets --offline -- -D warnings
cargo fmt --check
npx tsc --noEmit -p apps/desktop
npx vite build            (in apps/desktop)
node tools/tasks.mjs check
```

Results on 23 September 2026, Windows 11 Pro 26200 x64:

- `cargo test --workspace --locked` — 223 passed, 0 failed, 4 ignored (already
  ignored before this work). The baseline before this task was 210.
- `fetchpath-core` — 57 tests, up from 51: the three publication points, the
  pasted-checksum normalizer, and the job builder that accepts a checksum
  only before the job starts.
- `fetchpath-desktop` — 42 tests, up from 35: match, mismatch with no
  automatic retry, a malformed checksum refused, a blank checksum meaning
  none, a batch refused, retry with a corrected checksum, and a corrupted
  saved checksum failing closed on restart.
- Clippy with `-D warnings`, `cargo fmt --check`, the TypeScript check and the
  production frontend build — clean.
- `node tools/tasks.mjs check` — PASS.

## Verified adversarially

Each guard was removed in turn to confirm that its test detects the gap:

- Removing the pending-recovery check fails
  `a_pending_publication_is_checked_before_it_is_completed_on_recovery`.
- Removing the published-recovery check fails
  `an_already_published_file_that_does_not_match_is_reported_not_deleted`.
- Treating a mismatch as a plain transport failure fails
  `a_checksum_mismatch_saves_nothing_and_is_never_retried_automatically` and
  `retrying_with_a_corrected_checksum_completes`.

## Claims this work does not make

**No publisher-authenticity claim.** A match proves the bytes are the ones the
checksum describes. It does not prove who published them, and the interface
says so.

**SHA-256 only.** MD5 and SHA-1 listings are refused, not accepted with a
weaker guarantee.

## Known limitations

1. **The UI walkthrough has not been run for this change.**
   `apps/desktop/tools/ui-smoke.ps1` now includes a mismatch and a match
   scenario, but running it drives the real window and injects keystrokes. It
   waits for the person's go-ahead.
2. **A mismatch after a crash that already published** is reported, not
   repaired. The file stays where it was, with its real digest shown.
3. **Browser-captured downloads carry no checksum.** The extension does not
   offer the field.
4. **No cache reuse yet.** A checksum makes a download eligible for the bounded
   cache, but the desktop does not use the cache until FP-032.
