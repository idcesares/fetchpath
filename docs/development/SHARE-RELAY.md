# Share relay and encryption framing (FP-116, built in FP-117)

Resolves T-O1 and T-O2 of the [file transfer and sharing design](../architecture/specs/2026-10-31-file-transfer-and-sharing-design.md). Throwaway probes lived in the ignored `work/` and were deleted; nothing here is product code.

## Choice

- **Relay (T-O1): thin custom relay.** The engine dials out to the relay; the relay forwards HTTP Range requests and response bytes, stores nothing, and holds only the open dial-in connections. iroh's relay and wasm client are not adopted.
- **Framing (T-O2): chunked AEAD, AES-256-GCM, decrypted with WebCrypto.** Rust side uses `aes-gcm` and `hkdf`, both already in `Cargo.lock`. No new dependency is needed for the cipher; the relay may need an HTTP/stream crate, decided in the T3 relay task.

Why: Tunnel and Direct modes serve a plain HTTPS GET with Range to the recipient page, so the fragment-key decrypt path is required whichever relay is used. The iroh browser client would add a second, different transport path (wasm bundle, node ID in the link, app-level resume) for no property the thin relay lacks, and it needs relay access control for browser visitors on top of the link.

## Framing

Header, 22 bytes, sent in the clear and bound as AAD of every chunk: `"FPS1"`, version `1`, chunk size log2 (1 byte), 16-byte random salt. Keys: HKDF-SHA256 over the fragment key, salt from the header, info `fetchpath-share-v1`, 39 bytes = 32-byte AES key + 7-byte nonce prefix. Chunk nonce (12 bytes): prefix(7) + chunk index u32 big-endian + last-chunk flag (1 byte). A chunk is `ciphertext || 16-byte tag`. The final chunk sets the last flag, so truncation, reordering, replay of a chunk elsewhere and a swapped header all fail authentication. Chunk-aligned Range reads give resume: the recipient restarts at `22 + i * (chunkSize + 16)`.

Rules the format states (built and tested in FP-117; `crates/fetchpath-session/src/share/frame.rs`, vectors in `tests/fixtures/share-vectors.json`):

- **End of stream:** the stream ends right after the one chunk that authenticates with last=1. A last=0 chunk must be followed by another chunk; bytes after the last chunk are rejected; the page never tries both flag values or treats EOF or a short chunk as the signal. An empty file is one empty chunk with last=1, never a header alone. The probe's 1 GiB file was exactly 16384 full chunks, so these edges were not exercised.
- **Chunk size:** read from the unauthenticated header, so the page accepts only a fixed range (for example 2^16 to 2^22) and rejects the rest before buffering.
- **Frozen content:** salt, key and nonce prefix are fixed for a share, and Range resume reuses them. The engine must encrypt from a frozen copy, or bind the salt to the content hash and refuse to serve on mismatch; otherwise a file changed mid-share reuses a GCM nonce under one key, which leaks plaintext XOR and the authentication key. A check before serving does not cover a change during a response.
- **Page integrity:** the fragment keeps the key out of logs and headers and protects against passive reading. A relay or tunnel vendor that terminates TLS also serves the decrypting page and could serve a modified one that reads `location.hash`. The threat model must say so or name a page source the path cannot change.

## What FP-117 built

- **Format fixed:** 64 KiB chunks (`FPS1`, version 1); readers accept 2^16 to 2^22. The plain stream is `u16 name length || UTF-8 name || file bytes`, so the file name travels only inside the ciphertext and a response never carries it. Range is over ciphertext; resume offsets are chunk aligned.
- **Frozen content:** the engine opens the file once, denying write, rename and delete sharing while the link lives, hashes it, and encrypts only from that handle. A file therefore cannot change under a live link; one that cannot be read ends the link as `changed`.
- **Where it runs:** the ticket store and framing in `fetchpath-session`, the HTTP listener (hyper) and the page in `apps/cli/src/share`. No relay: Tunnel mode is a tunnel the person runs (Cloudflare Tunnel, Tailscale Funnel) to `127.0.0.1:<share port>`; the stored address must match the `Host` the tunnel sends. The thin custom relay stays a later step.
- **Tickets and keys are memory only.** A restart revokes every link.
- **Delivered means handed to the connection,** in order, not received. The setup test passes on that.
- **Decrypt in the page:** WebCrypto AES-GCM, buffered into a Blob and saved with a download link. There is no service worker and no streaming save, so a very large file is limited by the phone browser memory (not measured). The page uses the desktop design tokens (`design/tokens.css`); its script and style are allowed by hash in the CSP, with no style attribute, external request or other script, and the ended (410) page allows the style hash alone. The QR image is the whole link, key included: a client writes it only to a path the person chose (desktop save dialog, `share ... --qr FILE.svg|png`, never over an existing file), and says so.
- **Page integrity (the threat note):** the fragment keeps the key out of logs and request headers. A tunnel vendor or proxy that terminates TLS also serves the page and could serve a modified one that reads `location.hash`. Tunnel mode does not defend against that; it is out of scope in SECURITY.md. A link to a file is for people who trust the tunnel they chose.
- **Not verified:** a phone on mobile data through a real tunnel (X7), any phone browser, large files on a phone, and the page on browsers other than the desktop one used for testing.

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

## Relay design (FP-118)

Decided with the owner 6 November 2026: the relay is its own crate (`crates/fetchpath-relay`, binary `fetchpath-relay`, run as `fetchpath-relay serve`), because `apps/cli` is Windows-only (`windows-sys`) and the relay runs on a Linux VPS. The owner builds it from source on the VPS (`cargo install --path crates/fetchpath-relay`); a Linux release artifact is a separate release task. ACME is `instant-acme` over rustls (also the choice for FP-119).

- **One port, two roles.** The relay terminates TLS on 443. A browser (ALPN `http/1.1`) is a visitor; an engine (ALPN `fetchpath-relay/1`) is an enrolled engine. No second firewall port.
- **Byte pipe.** The relay reads only the request head of a visitor, routes `GET /s/<id>` and pipes the bytes both ways. It adds, removes and rewrites no header, so the engine's share handler (Host check, CSP hashes, Range, 410 page, no key in headers) behaves as in Tunnel mode. Everything else gets a fixed 404. One request per visitor connection: the engine closes after each response.
- **Enrollment.** After TLS the relay sends a 32-byte random challenge; the engine answers with its role, a name and `HMAC-SHA256(enrollment key, "fpr1" || role || name || challenge)`. The key never crosses the wire, is compared in constant time, and a failure closes the connection without saying why. The key is 32 random bytes the relay creates (`fetchpath-relay enroll`) and prints once as 64 hex characters; the relay stores only the key bytes in its config file (mode 0600), because an HMAC check needs the key. "Stateless" means no accounts and no files of shares, not no configuration.
- **Control and work connections.** Each engine holds one control connection (it announces the ids of its live tickets and pings) and a small pool of idle work connections that the engine opens. A visitor for an announced id takes one idle work connection of that engine; the engine tops the pool up when one is used. An engine that disappears loses its ids at once; a visitor that finds no engine or no idle connection within 5 s gets a fixed 502.
- **Limits.** Visitors served at once, per-engine work connections, request head size (8 KiB), head and idle timeouts, and a 4 hour connection lifetime, as on the local share listener. Backpressure is the socket's: `copy_bidirectional` buffers a few KiB per side and nothing else.
- **Logs.** The relay logs counters and states (engine enrolled, refused, visitor served) and never a path, header, id, key or byte of a body. A test sends a known marker through a transfer and asserts it is in no log line.
- **Threat note.** The relay terminates TLS and could serve a modified page. The relay's owner is the person running it, so the trust matches Tunnel mode: a link is for people who trust the relay the person chose. Say so in the guide.
- **Key at rest (owner decision, 6 Nov 2026):** the engine keeps the enrollment key in `lan/relay-key`, sealed with the existing DPAPI helper (`apps/cli/src/share/relay_key.rs`), so it survives restarts. It never appears in the sharing setup, logs or UI. The helper's entropy label is the LAN identity's; a magic prefix on the plaintext tells the two blobs apart.
- **Engine side (built).** `Command::ConfigureRelay { relay, key }` (person only, not ledgered, `key` optional) switches the stored setup between Tunnel and Relay and, with a key, replaces it. The relay's address is the stored `base_url`; in relay mode the engine listens on nothing and dials it. `ShareView` gains `relay` and `relay_key_stored`; the key is write-only (`RelayKey` is a 64-hex newtype that never prints). The session cannot seal secrets, so the host injects a `RelayKeyStore` (`KeyFile`, DPAPI) and the key is written before the setup file. Every change of address, port, mode or key bumps a generation, turns sharing off, clears the test and revokes links, and a relay report for an older generation is ignored. The engine enrolls under the fixed name `fetchpath`, verifies the relay with the Windows certificate store (`rustls-platform-verifier`, feature `tls-client`) and serves each relayed request with the share handler, keep-alive off. Desktop: Settings, Public links. CLI: `fetchpath share relay URL` (key from stdin, never an argument), `share relay off`.

### Built, verified, limits (FP-118)

Built: `crates/fetchpath-relay` (library: wire, server, client; binary `fetchpath-relay enroll|serve` behind the `serve` feature, which keeps the ACME/TLS stack out of the Windows CLI). TLS is rustls with aws-lc-rs. The certificate comes from `instant-acme` by HTTP-01 on a separate port, or from `--cert/--key`; it is renewed after 30 days and swapped without a restart. State is one directory, files 0600 on Unix: `enrollment.key`, `acme-account-{staging,production}.json`, `cert-chain.pem`, `cert-key.pem`, `cert-meta` (domain, CA and issue time; a stored certificate is reused only for the same domain and CA). Guide: docs/user/RELAY.md.

Verified: loopback tests for routing, enrollment refusal, pool top-up, replacement, idle recycling, large bodies, and the log observer (checked on both `Debug` and `Display`); over TLS, engine and visitor on one port by ALPN and a wrong-key engine refused (`cargo test -p fetchpath-relay --features serve`, also run in CI); a unit test for the stored-certificate decision.

Engine side verified: a loopback relay and a real engine in `apps/cli/src/share/tests.rs` (test link delivered through the relay with Range resume, sharing turns on, unknown ids and a foreign Host refused, a wrong key shown as a problem, leaving relay mode), session tests for the generation and key-store ordering, protocol tests for `RelayKey`, and a TLS connector test against the relay's TLS listener.

Not verified: a real Let's Encrypt issuance, a Linux build, a VPS run, a phone on mobile data (gate X7, the owner's); the Windows certificate store against a real certificate (the TLS test trusts its own root); the desktop relay section was not exercised by hand.

Limits that hold:
- The relay routes only the first request of a visitor connection; the engine serves relayed streams with keep-alive off, so each response closes the connection.
- The desktop's relay section (`configure_relay`, a password field cleared on use) is built and type-checked but has no automated UI test and was not clicked through; a web UI session cannot set it (settings stay with `user`).
- An enrollment key is typed or piped as text; the terminal echoes a pasted key unless it is piped.
- An engine announces at most 256 live ticket ids; extra ids are not routed.
- Idle work connections are recycled by the engine every 60 s, probed by the relay before use, and dropped after 90 s. A transfer that moves no byte for 5 min ends. Unauthenticated engine handshakes are capped at 32 at once, request heads at 128 and TLS handshakes at 256.
- Renewal runs after 30 days (short enough for 45 day certificates). A failed renewal keeps the old certificate; a restart during a CA outage reuses a stored one up to 40 days old. Only a first issuance with nothing stored is fatal. The certificate's own expiry is not parsed.
- No per-source cap: a client that opens many connections and stalls can fill the global handshake limits (32 enrollment, 256 TLS) for up to 10 s each and lock out engines and visitors. Mitigate at the firewall until a per-address limit is added.
- A network drop right after the engine's reply is reported as refused.
- The engine's detached relayed-stream handler keeps its `max_open` count per session, not across reconnects.
