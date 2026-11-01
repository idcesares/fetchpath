# Share relay and encryption framing (FP-116)

Resolves T-O1 and T-O2 of the [file transfer and sharing design](../architecture/specs/2026-10-31-file-transfer-and-sharing-design.md). Throwaway probes lived in the ignored `work/` and were deleted; nothing here is product code.

## Choice

- **Relay (T-O1): thin custom relay.** The engine dials out to the relay; the relay forwards HTTP Range requests and response bytes, stores nothing, and holds only the open dial-in connections. iroh's relay and wasm client are not adopted.
- **Framing (T-O2): chunked AEAD, AES-256-GCM, decrypted with WebCrypto.** Rust side uses `aes-gcm` and `hkdf`, both already in `Cargo.lock`. No new dependency is needed for the cipher; the relay may need an HTTP/stream crate, decided in the T3 relay task.

Why: Tunnel and Direct modes serve a plain HTTPS GET with Range to the recipient page, so the fragment-key decrypt path is required whichever relay is used. The iroh browser client would add a second, different transport path (wasm bundle, node ID in the link, app-level resume) for no property the thin relay lacks, and it needs relay access control for browser visitors on top of the link.

## Framing

Header, 22 bytes, sent in the clear and bound as AAD of every chunk: `"FPS1"`, version `1`, chunk size log2 (1 byte), 16-byte random salt. Keys: HKDF-SHA256 over the fragment key, salt from the header, info `fetchpath-share-v1`, 39 bytes = 32-byte AES key + 7-byte nonce prefix. Chunk nonce (12 bytes): prefix(7) + chunk index u32 big-endian + last-chunk flag (1 byte). A chunk is `ciphertext || 16-byte tag`. The final chunk sets the last flag, so truncation, reordering, replay of a chunk elsewhere and a swapped header all fail authentication. Chunk-aligned Range reads give resume: the recipient restarts at `22 + i * (chunkSize + 16)`.

Rules the format must state (found in review, not yet tested):

- **End of stream:** the stream ends right after the one chunk that authenticates with last=1. A last=0 chunk must be followed by another chunk; bytes after the last chunk are rejected; the page never tries both flag values or treats EOF or a short chunk as the signal. An empty file is one empty chunk with last=1, never a header alone. The probe's 1 GiB file was exactly 16384 full chunks, so these edges were not exercised.
- **Chunk size:** read from the unauthenticated header, so the page accepts only a fixed range (for example 2^16 to 2^22) and rejects the rest before buffering.
- **Frozen content:** salt, key and nonce prefix are fixed for a share, and Range resume reuses them. The engine must encrypt from a frozen copy, or bind the salt to the content hash and refuse to serve on mismatch; otherwise a file changed mid-share reuses a GCM nonce under one key, which leaks plaintext XOR and the authentication key. A check before serving does not cover a change during a response.
- **Page integrity:** the fragment keeps the key out of logs and headers and protects against passive reading. A relay or tunnel vendor that terminates TLS also serves the decrypting page and could serve a modified one that reads `location.hash`. The threat model must say so or name a page source the path cannot change.

The sketch is what was measured, not a frozen format. T3 fixes the chunk size and the rules above, and the X8 review covers them.

## What was measured

Same 1 GiB deterministic file (1,073,741,824 bytes plain), loopback, the Claude desktop app's built-in browser pane (Chromium), 64 KiB chunks unless noted. Native servers on this Windows PC.

| Path | Throughput | Peak JS heap | Notes |
|---|---|---|---|
| Direct HTTP control | 132 MiB/s (64 KiB), 108 (1 MiB) | 62 to 76 MiB | limit not identified: decrypt alone ran 2.6 to 4 times faster (microbench below) |
| Thin custom relay | 133 to 141 MiB/s (64 KiB), 113 (1 MiB) | 62 to 76 MiB | not distinguishable from the control |
| iroh 1.4.1 relay + wasm client (`iroh-relay --dev`, plain HTTP) | 26.9 MiB/s | 105 MiB | one 38 s run |

- Relay process RSS went 128 to 136 MB after 3.2 GB forwarded (one Node process that also hosted the engine and control server, so an upper bound).
- Resume: stopped at chunk 6500 of 16384 and resumed from there, on both paths, and the plain bytes summed exactly on both (iroh: 425,984,000 + 647,757,824 = 1,073,741,824); over-fetch on the custom path 1.7 MB. On iroh resume is app-level only (a new stream with a start offset, which the probe implements); iroh has no HTTP Range.
- Negative tests, both paths: flipped ciphertext byte, a chunk shifted to the wrong index and a wrong start index all fail authentication; truncated stream does not report the last chunk as valid.
- Decrypt-only microbench: WebCrypto AES-GCM 349 to 529 MiB/s; `@noble/ciphers` 1.3.0 ChaCha20-Poly1305 in JS about 30 MiB/s. ChaCha is not in WebCrypto, so it would need JS or wasm in the page.
- iroh costs: wasm bundle 4,258,353 bytes (1,386,840 gzipped) plus 47 KB glue; init 50 ms, endpoint spawn 21 to 27 ms (loopback). The wasm is not on npm, so shipping it needs our own wasm-bindgen wrapper crate, a pinned wasm-bindgen (0.2.122 here) and a clang toolchain for `ring` on wasm32. Building it needed one (LLVM 23.1.4 from github.com/llvm/llvm-project releases, portable archive, checksum checked against the published one, kept in `work/`, nothing installed).
- iroh's relay has access modes (everyone, allowlist, denylist, http callback, shared token); read from source, not run beyond `--dev`. Browser visitors would need relay access too, so a share would need a relay-side admission rule in addition to the link.

## Not verified

- iroh throughput is a loopback number from my wrapper (256 KiB reads through a bounded channel); it may be improvable, so "iroh is slower" is not claimed beyond this probe. The decision does not rest on it.
- No real network, no internet-speed claim, no hole punching (iroh browser is relay-only per its docs), no production TLS relay for either option, no browser other than the pane's. The pane may throttle when hidden.
- Abort and over-fetch were checked on loopback only. The relay's behavior with many concurrent shares, slow clients and an engine that vanishes mid-stream is not measured.
- iroh-relay statelessness and self-hosting effort are from source and the `--dev` run, not a production deployment.
- The framing is not reviewed. X8 (independent strong review of the final framing) still applies before T3 ships.
