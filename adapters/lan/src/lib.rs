//! Paired LAN mode: a user's own explicitly paired devices reuse
//! credential-free, trusted-digest cache entries over a local network.
//!
//! A content hash is never permission to redistribute an object. Only entries
//! whose provenance was recorded `Public` at insertion may leave the machine,
//! only to a pinned and authenticated peer, and only while LAN mode is on.
//! Bytes received from a peer must be re-verified against the receiver's own
//! trusted digest before publication: a peer is a source, never an authority.
//!
//! Peer retrieval is sequential and is not presented as acceleration.
//! Discovery is out of scope; peers are reached at an address the user gives.

mod fetch;
mod frame;
mod handshake;
mod identity;
mod pairing;
mod pins;
mod protect;
mod serve;
mod session;

pub use fetch::{FetchError, PeerClient};
pub use frame::{MAX_CHUNK_BYTES, MAX_FRAME_BYTES, read_frame, write_frame};
pub use identity::{DeviceIdentity, Fingerprint, PeerKey};
pub use pairing::{
    CODE_BITS, CODE_LIFETIME, PairingCode, PairingError, host_pairing, join_pairing, normalize_code,
};
pub use pins::PinStore;
pub use protect::{Dpapi, SecretProtector};
pub use serve::{PeerServer, SessionSummary, UploadBudget};
pub use session::{SecureChannel, SessionError, accept_session, connect_session};
