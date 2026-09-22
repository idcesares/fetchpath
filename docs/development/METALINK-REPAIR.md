# FP-019 Metalink and verified multi-source repair

Date: 22 September 2026
Acceptance: A02, A03, A05

## Delivered behavior

`fetchpath-metalink` parses the Metalink 4 ([RFC 5854]) subset this project needs and owns trusted piece verification. `fetchpath-core` gains an additive `download_verified` path that fetches one representation from a mirror list, verifies it, repairs it selectively where trusted piece hashes allow it, and publishes only after the required verification passes. Every existing `fetchpath-core` export (`download`, `download_with_faults`, `FileJob`, `DownloadRequest`, `DownloadedFile`, `DownloadError`) is unchanged.

### Parsing

- The XML reader is small, hand-written, and bounded. It has no third-party dependency; `sha2` is the crate's only dependency.
- A `<!DOCTYPE ...>` or `<!ENTITY ...>` is a hard typed error, so there is no entity expansion and no external entity resolution to attack. Only the five predefined entities and numeric character references are resolved.
- Input size, nesting depth, element count, attributes per element, name length, and text length are all capped by `ParseLimits`, as are files per document, mirror URLs per file, whole-file hashes per file, and piece hashes. Every malformed, hostile, or oversized input returns a typed `MetalinkError`; none of them panics.
- `<file name=...>` is validated before it can decide anything on disk: absolute paths, drive-qualified paths, backslashes, `..` and `.` components, empty components, and control characters (including NUL) are all refused.
- Parsed: file name, `<size>`, `<url>` with `priority` and `location`, whole-file `<hash type=...>`, and `<pieces length=N type=sha-256>` with ordered `<hash>` children.
- A `<pieces>` map is only built when its piece count is exactly `ceil(size / length)`. A piece map with no declared size is refused, because there is nothing to check the count against. A `<pieces type=...>` this crate cannot verify is a typed error rather than a silent downgrade to final-hash-only behavior.

### Verification and repair

- `PieceMap::verify_file` reports exactly which piece indices fail, and `PieceMap::piece_range` maps an index to the precise `(start, length)` byte range, so a caller re-fetches only the damaged bytes. `StreamingPieceVerifier` checks each piece the moment its bytes complete, which lets a hopeless mirror be abandoned before the whole file arrives.
- A file that is short fails every piece it never covered, and a file that is long fails the size check even when every piece matches. Neither is quietly treated as complete.
- `download_verified` stages through the existing `CheckpointStore`, repairs failing pieces from a mirror other than the one that produced the bad bytes where one is available, then commits a publication intent and publishes through the unchanged create-only fence and cancellation race gate.
- `VerifiedDownload::verification` distinguishes three different promises: `PieceHashes`, `FinalHashOnly`, and `Unverified`. `repaired_pieces` is only ever non-empty for `PieceHashes`.
- Mirror health is measured, and measurement outranks the advisory `priority`: corrupt sinks furthest, then offline, then slow, then merely unhelpful. `MirrorReport` records attempts, bytes delivered, each failure count, whether the mirror was de-prioritised, and its outcome. Its `redacted_url` has any query string and fragment removed, so a signed mirror URL never reaches a report or the recorded evidence.

### Resource discipline (A05)

Every mirror request reserves from the same process-wide `GlobalBudget` the adaptive HTTP path uses. Each loop is bounded: at most `MAX_MIRRORS` (32) mirrors considered, `MAX_WHOLE_FILE_ATTEMPTS` (6) whole-file attempts in total, `MAX_REPAIR_ROUNDS` (3) verify/repair passes, and `MAX_REPAIR_MIRRORS_PER_PIECE` (3) mirrors per damaged piece. A mirror leaves rotation after two strikes' worth of penalty or three attempts. `mirror_attempt_timeout` is enforced both as a hard libcurl ceiling and in the progress callback, so a stalled mirror cannot hold the transfer open. Repair hashes the staged range in place with a 64 KiB buffer instead of buffering a whole piece.

## Honesty rules this slice is bound by

- A digest computed here is an **observed local digest** compared against a digest supplied by whoever wrote the Metalink document. It is not publisher authenticity, and nothing in the API, the reports, or the evidence says otherwise. The field is named `observed_sha256` for exactly this reason.
- Trusted piece hashes are what make a repair claim possible. They localize damage to a byte range, that range is re-fetched, and that range is re-verified before it counts.
- With only a whole-file digest there is **no fault localization at all**. A mismatch discards the whole staged file and restarts conservatively from another mirror; `repaired_pieces` stays empty and `conservative_restarts` increments. The code never reports a piece it did not verify.
- When both a piece map and a whole-file digest exist, both must pass. Pieces that all match while the whole-file digest does not is a refusal, not a publication.
- Nothing reaches the destination unless the required verification passed. The storage layer independently re-validates the staged bytes against the committed publication intent before it links the destination.

## Verification

All of the following were run from the repository root on 22 September 2026 and passed.

```powershell
cargo test -p fetchpath-metalink -p fetchpath-core
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
cargo build --workspace
powershell -NoProfile -ExecutionPolicy Bypass -File tests/compatibility/metalink/run.ps1
```

`cargo test -p fetchpath-metalink -p fetchpath-core` reported 29 passed in `fetchpath-metalink` and 33 passed in `fetchpath-core`, with the mirror fixture test correctly ignored outside its harness. `cargo build --workspace` confirms the CLI, desktop, and media consumers of `fetchpath-core` still compile against the additive API.

### Unit coverage

`fetchpath-metalink`: the valid subset including prefixed namespaces and multiple files; billion-laughs and external-entity documents (both refused at the DTD); undeclared entity references; malformed, unterminated, duplicate-attribute, non-UTF-8, and control-character inputs; oversized documents and each of the size, depth, element, attribute, name, and text budgets; unsafe file names (absolute, drive-qualified, backslash, `..`, `.`, empty component, NUL); inconsistent piece counts; a piece map with no declared size; an unverifiable piece digest type; and piece verification across damaged, short, long, and chunk-boundary-varying inputs.

`fetchpath-core`, against loopback `TcpListener` mirrors in the style already used in `transfer.rs`: a mirror that serves one corrupt piece being selectively repaired from a healthy mirror; an offline mirror failing over; a slow mirror being de-prioritised without hanging the transfer; a final-hash-only mismatch restarting conservatively while claiming no repair; every mirror failing verification so nothing is published; a whole-file digest mismatch refused even though every piece matched; bytes with no trusted digest reported as `Unverified` with the mirror URL's query redacted; every mirror offline; a non-HTTP mirror list refused before anything is created; and an existing destination never replaced.

### Real-fixture matrix

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File tests/compatibility/metalink/run.ps1
```

The harness starts real loopback HTTP mirrors from `tests/compatibility/metalink/mirror_servers.py` using only the Python standard library, so it needs no virtual environment and downloads nothing. It generates the Metalink 4 documents it then feeds back through `fetchpath-metalink`, and the ignored `crates/fetchpath-core/tests/metalink_mirrors.rs` integration test records what happened to [the mirror matrix](evidence/metalink/fp019-mirror-matrix.json).

The recorded run used a 512 KiB payload, a 64 KiB piece length, 8 pieces, and piece 3 damaged:

| Scenario | Result |
| --- | --- |
| Offline, slow, and corrupt mirrors ranked ahead of the healthy one | Published in 6,221 ms. `PieceHashes`, `repaired_pieces: [3]`, no conservative restarts. Outcomes: `Offline`, `Slow`, `Corrupt`, `Repaired`, with the first three de-prioritised. The healthy mirror delivered exactly 65,536 repair bytes. |
| The same corrupt mirror with no piece map | Published in 139 ms. `FinalHashOnly`, `repaired_pieces: []`, `conservative_restarts: 1`. The corrupt mirror is recorded as `Corrupt`; no piece-level claim is made. |
| Every mirror damaged in the same piece | Nothing published in 112 ms. `verification.failed: piece 3 could not be repaired from any mirror`. The destination was never created and the staging directory was left empty. |

The offline mirror is a released loopback port. On this machine a connection to it is refused after about two seconds rather than immediately, which is why the attempt ceiling in that test is 4 s and why a mirror that never begins a response is classified `Offline` however that failure surfaces.

## Limits carried forward

- **No publisher authenticity claim of any kind.** Metalink documents are unsigned here. Matching a digest in a document only proves the bytes agree with that document; whoever could substitute the bytes could substitute the document. OpenPGP `<signature>` elements are parsed by nobody in this slice and no signature is verified.
- **Final-hash-only mismatches are not localized and never will be by this path.** The whole staged file is discarded. On a large file that is expensive, and it is still the only honest option.
- **Mirror fan-out is sequential.** `MAX_CONCURRENT_MIRROR_ATTEMPTS` is 1: this path issues one mirror request at a time. Parallel multi-source downloading is not implemented, and no speed claim is made for it. The global budget gates every request regardless.
- **No resume in the verified path.** Each whole-file attempt resets staging and starts at byte zero. It therefore never reuses retained bytes and never faces the strong-validator resume question, which is left entirely to the existing `download` path.
- **Repair writes land in staging before they are verified.** A repair range is streamed into the staging file and then hashed in place. Staging is never published unverified, and a failed repair is overwritten by the next attempt, but a caller inspecting staging mid-repair can see unverified bytes there.
- **Parsing subset only.** `<metaurl>`, `<signature>`, `<publisher>`, `<description>`, `<language>`, `<os>`, dynamic mirror updates, and Metalink 3 are not parsed. Piece maps must be SHA-256; a `sha-1` or `md5` piece map is refused rather than downgraded. Whole-file hashes of other types are kept as declared but only SHA-256 is checked.
- **Not wired into the CLI, desktop, or queue.** This slice is the crate, the core path, and its evidence. Driving it from a file job, choosing a destination from a Metalink file name, and multi-file Metalink documents are separate work.
- **Loopback fixtures do not model the internet.** They establish that bad, slow, and offline mirrors are handled correctly and that nothing unverified is published. They say nothing about real mirror networks, RTT, loss, or interoperability with any particular Metalink publisher.

[RFC 5854]: https://www.rfc-editor.org/rfc/rfc5854
