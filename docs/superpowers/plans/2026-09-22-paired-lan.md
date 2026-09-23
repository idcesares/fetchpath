# Paired LAN Mode Implementation Plan

**Goal:** Let a user's own explicitly paired devices reuse credential-free,
trusted-digest cache entries over a LAN, behind a default-off flag, without a
content hash ever acting as permission to redistribute an object.

**Spec:** `docs/superpowers/specs/2026-09-22-content-cache-and-paired-lan-design.md`, section 5.

**Backlog task:** FP-020, owner `cache_lan`. This is the second half; the
bounded cache landed in `docs/superpowers/plans/2026-09-22-bounded-content-cache.md`.

## Global constraints

- **No new downloads.** Every crate must already be in `Cargo.lock`:
  `ed25519-dalek 3.0.0`, `curve25519-dalek 5.0.0` (X25519 through
  `MontgomeryPoint`), `sha2 0.11.0`, `hmac 0.13.0`, `hkdf 0.13.0`,
  `aes-gcm 0.11.1`, `getrandom 0.3.4`, `zeroize 1.9.0`, `windows-sys 0.61.2`.
  All arrive today through `russh`. The lockfile gains only the new package.
- `adapters/lan` depends on `fetchpath-cache` only, not on `fetchpath-core`.
  Core defines a `PeerSource` trait; `apps/cli` adapts the LAN client to it.
- Clippy with `-D warnings`, `cargo fmt --check`, `cargo test --workspace`.
- No speed claim, no publisher-authenticity claim, no claim that pairing is a
  PAKE.
- Secrets never reach logs, errors, evidence or `Debug` output: the identity
  signing key, the pairing code, shared secrets and session keys.

## Decisions this plan makes that the spec left open

Recorded here and in the evidence document with the date 22 September 2026.

1. **Transport.** Blocking `std::net` TCP to an explicit address. Discovery is
   deferred (mDNS follow-up), so the user supplies `host:port`.
2. **Framing.** `kind: u8`, `len: u32 BE`, payload. A length above
   `MAX_FRAME_BYTES` (64 KiB plus the AEAD tag) is refused *before* any
   allocation. Every handshake read has a 10 second timeout.
3. **Pairing.** The code is 10 Crockford base32 characters, 50 bits, shown as
   `XXXXX-XXXXX`. It is consumed by the first connection that presents a hello,
   whatever the outcome, and expires after two minutes. Each side sends an
   X25519 ephemeral key, its ed25519 identity key and a nonce. Confirmation keys
   are `HKDF-SHA256(salt = transcript hash, ikm = shared || code)`. The joiner
   confirms first with `HMAC(k_joiner, th)` and a signature over the transcript;
   the host verifies both before pinning and replying in kind. An all-zero
   shared secret is refused.
4. **Session.** Mutual authentication against pinned keys only. The server
   checks the client's identity key against its pins on the *first* frame and
   refuses before sending anything else. The client refuses a server whose key
   is not the one it asked for. Both sign the transcript. Traffic keys come from
   `HKDF-SHA256(salt = th, ikm = shared)`, one per direction, and every
   post-handshake frame is AES-256-GCM with a per-direction counter nonce and
   the frame kind as associated data. The spec asked for authentication; this
   adds confidentiality at no dependency cost.
5. **Non-disclosure.** An absent entry, a `Credentialed` entry and a request
   made while LAN mode is off all produce the same `not_available` refusal, so
   a paired peer cannot probe for private content.
6. **Budget.** A per-session byte budget is checked against the offered size
   before any data is sent. Upload rate is paced per transfer, holding the
   average since the transfer began to the configured limit.
7. **Peer bytes are not re-inserted into the cache.** The receiver cannot
   verify the sender's provenance claim, so it does not create an entry that a
   later setting could share.
8. **Pins** live in a plain file written by temporary file and rename. They are
   public keys, not secrets; the identity key is DPAPI-protected with its own
   entropy label `fetchpath-lan-identity-v1`.

## File structure

Created: `adapters/lan/{Cargo.toml, src/lib.rs, src/identity.rs, src/pins.rs,
src/frame.rs, src/handshake.rs, src/pairing.rs, src/session.rs, src/serve.rs
(including the budget), src/fetch.rs, src/protect.rs, tests/lan_behaviour.rs}`,
and `apps/cli/src/lan.rs`.

Modified: `Cargo.toml` (member), `crates/fetchpath-cache/src/lib.rs`
(`config()` accessor), `crates/fetchpath-core/src/verified.rs` and `lib.rs`
(`PeerSource`, `DeliverySource::Peer`, `download_verified_shared`),
`apps/cli` (`lan` and `cache` commands), evidence documents, backlog.

## Tasks

- [x] 1. Crate skeleton, identity (generate, fingerprint, DPAPI persistence), pin store.
- [x] 2. Frame codec with the pre-allocation length check; malformed-frame tests.
- [x] 3. Pairing: code generation, host and joiner handshakes. Tests: success
      pins both sides; wrong code fails and pins nothing; expired code fails;
      a consumed code cannot be reused.
- [x] 4. Session handshake and AEAD channel. Tests: paired peers authenticate;
      unpaired client refused before any byte of content; wrong server key refused;
      tampered ciphertext refused.
- [x] 5. Serving and fetching with the budget. Tests: paired peer retrieves a
      public entry byte-identically; credentialed entry and LAN-off both refused
      as `not_available`; session budget enforced; rate pacing observed; a
      peer offering more than the receiver's ceiling is abandoned.
- [x] 6. Core `PeerSource` path: cache, then peers, then mirrors. Tests: peer
      bytes verified and published with `DeliverySource::Peer`; corrupt peer bytes
      discarded unpublished, peer deprioritised, mirrors used; peer bytes are not
      inserted into the cache.
- [x] 7. CLI: `lan id | enable | disable | pair-host | pair-join | peers | serve`
      and `cache status`.
- [x] 8. Evidence document, matrix JSON, backlog update, full check run.
