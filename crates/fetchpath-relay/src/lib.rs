//! The share relay (FP-118): a stateless byte pipe between visitors on the
//! internet and Fetchpath engines enrolled by the relay's owner.
//!
//! The relay stores no file, has no account, and reads nothing of a visitor
//! but the request head it needs to pick an engine. It adds, removes and
//! rewrites no header, so the engine's share handler behaves as it does in
//! Tunnel mode. The design is in `docs/development/SHARE-RELAY.md`.
//!
//! The crate works on already-secured streams: the binary terminates TLS and
//! tells engines from visitors by ALPN, and tests run on plain sockets.

#[cfg(feature = "serve")]
pub mod acme;
pub mod client;
pub mod server;
#[cfg(feature = "serve")]
pub mod tls;
pub mod wire;
