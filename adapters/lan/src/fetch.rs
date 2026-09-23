//! Fetching an entry from one paired peer.
//!
//! A peer is a source, never an authority. This module only moves bytes into
//! a file the caller names and enforces the caller's size ceiling; the caller
//! must verify them against its own trusted digest before publishing anything.

use crate::frame::kind;
use crate::identity::{DeviceIdentity, PeerKey};
use crate::serve::{BUDGET_EXHAUSTED, NOT_AVAILABLE};
use crate::session::{SessionError, connect_session};
use fetchpath_cache::ContentId;
use std::fmt;
use std::fs::File;
use std::io::{self, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const TRANSFER_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug)]
pub enum FetchError {
    Unreachable(io::Error),
    Session(SessionError),
    /// The peer does not hold a shareable copy, or will not say.
    NotAvailable,
    BudgetExhausted,
    /// The peer offered more than the caller will accept. Nothing was read.
    Oversize {
        offered: u64,
        ceiling: u64,
    },
    Protocol(&'static str),
    Io(io::Error),
}

impl fmt::Display for FetchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unreachable(error) => write!(f, "peer.unreachable:{error}"),
            Self::Session(error) => write!(f, "peer.{error}"),
            Self::NotAvailable => f.write_str("peer.not_available"),
            Self::BudgetExhausted => f.write_str("peer.budget_exhausted"),
            Self::Oversize { offered, ceiling } => {
                write!(f, "peer.oversize:{offered}>{ceiling}")
            }
            Self::Protocol(reason) => write!(f, "peer.protocol:{reason}"),
            Self::Io(error) => write!(f, "peer.io:{error}"),
        }
    }
}

impl std::error::Error for FetchError {}

impl From<io::Error> for FetchError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<SessionError> for FetchError {
    fn from(error: SessionError) -> Self {
        Self::Session(error)
    }
}

#[derive(Clone, Debug)]
pub struct PeerClient {
    identity: Arc<DeviceIdentity>,
    address: SocketAddr,
    peer: PeerKey,
}

impl PeerClient {
    /// `peer` must already be pinned by the caller; it is the only key this
    /// client will accept from `address`.
    pub fn new(identity: Arc<DeviceIdentity>, address: SocketAddr, peer: PeerKey) -> Self {
        Self {
            identity,
            address,
            peer,
        }
    }

    pub fn peer(&self) -> PeerKey {
        self.peer
    }

    /// Writes the peer's copy of `id` into `into`, reading at most `ceiling`
    /// bytes. On error the file's contents are meaningless; discard them.
    pub fn fetch(&self, id: &ContentId, ceiling: u64, into: &Path) -> Result<u64, FetchError> {
        let stream = TcpStream::connect_timeout(&self.address, CONNECT_TIMEOUT)
            .map_err(FetchError::Unreachable)?;
        let mut channel = connect_session(stream, &self.identity, &self.peer)?;
        channel.set_read_timeout(Some(TRANSFER_TIMEOUT))?;

        channel.send(kind::GET, id.render().as_bytes())?;
        let (reply, payload) = channel.receive()?;
        let offered = match reply {
            kind::OFFER => {
                let bytes: [u8; 8] = payload
                    .as_slice()
                    .try_into()
                    .map_err(|_| FetchError::Protocol("malformed offer"))?;
                u64::from_be_bytes(bytes)
            }
            kind::REFUSE if payload == NOT_AVAILABLE.as_bytes() => {
                return Err(FetchError::NotAvailable);
            }
            kind::REFUSE if payload == BUDGET_EXHAUSTED.as_bytes() => {
                return Err(FetchError::BudgetExhausted);
            }
            _ => return Err(FetchError::Protocol("unexpected reply")),
        };
        if offered > ceiling {
            // Closing drops the transfer; nothing is read into memory or disk.
            return Err(FetchError::Oversize { offered, ceiling });
        }

        let mut file = File::create(into)?;
        let mut received = 0_u64;
        loop {
            let (frame, chunk) = channel.receive()?;
            match frame {
                kind::DATA => {
                    received += chunk.len() as u64;
                    if received > offered {
                        return Err(FetchError::Protocol("more bytes than offered"));
                    }
                    file.write_all(&chunk)?;
                }
                kind::END if received == offered => break,
                kind::END => return Err(FetchError::Protocol("fewer bytes than offered")),
                _ => return Err(FetchError::Protocol("unexpected frame during transfer")),
            }
        }
        file.flush()?;
        let _ = channel.send(kind::BYE, &[]);
        Ok(received)
    }
}
