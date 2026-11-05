//! Public link tickets (FP-117): what the share listener serves.
//!
//! The session owns the tickets, the frozen file and the encryption; the
//! listener in the CLI only moves bytes between a socket and a [`Transfer`].
//! Nothing here is written to disk: an engine restart ends every link.

pub mod frame;
mod store;

pub use store::{
    Created, DEFAULT_EXPIRY, MAX_EXPIRY, MAX_READ, NewShare, Phase, Piece, ShareError, ShareEvent,
    ShareEventKind, ShareInfo, ShareKind, ShareStore, Source, Transfer,
};
