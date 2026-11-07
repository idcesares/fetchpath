//! Public link tickets (FP-117): what the share listener serves.
//!
//! The session owns the tickets, the frozen file and the encryption; the
//! listener in the CLI only moves bytes between a socket and a [`Transfer`].
//! Nothing here is written to disk: an engine restart ends every link.

pub mod frame;
mod store;

/// Where the engine's host keeps the relay enrollment key (FP-118). The
/// session cannot seal a secret itself; the host seals it with the user's
/// credentials and the session only learns whether one is stored. The key is
/// never read back through this trait.
pub trait RelayKeyStore: Send + Sync {
    fn present(&self) -> bool;
    /// Seals and stores `key` (64 hex characters), replacing any earlier
    /// one. Never log the argument.
    fn store(&self, key: &str) -> Result<(), String>;
}

pub use store::{
    Created, DEFAULT_EXPIRY, MAX_EXPIRY, MAX_READ, NewShare, Phase, Piece, ShareError, ShareEvent,
    ShareEventKind, ShareInfo, ShareKind, ShareStore, Source, Transfer,
};
