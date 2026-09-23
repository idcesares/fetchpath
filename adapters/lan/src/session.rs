//! Sessions between paired devices: mutual authentication against pinned keys
//! only, then AES-256-GCM in each direction.
//!
//! There is no trust-on-first-use path. The server checks the client's key
//! against its pins on the first frame and refuses before sending anything
//! else; the client refuses any server whose key is not the one it asked for.

use crate::frame::{MAX_CHUNK_BYTES, kind, read_frame, write_frame};
use crate::handshake::{
    Ephemeral, HANDSHAKE_TIMEOUT, Hello, Received, derive_pair, expect, invalid, refuse,
    send_hello, transcript,
};
use crate::identity::{DeviceIdentity, PeerKey, random_bytes};
use crate::pins::PinStore;
use aes_gcm::aead::{Aead, KeyInit, Nonce, Payload};
use aes_gcm::{Aes256Gcm, Key};
use std::fmt;
use std::io;
use std::net::TcpStream;
use std::time::Duration;
use zeroize::Zeroizing;

const TRANSCRIPT_LABEL: &[u8] = b"fetchpath-lan-session-v1";
const TRAFFIC_INFO: &[u8] = b"fetchpath-lan-traffic-v1";
const CLIENT: &[u8] = b"client";
const SERVER: &[u8] = b"server";

#[derive(Debug)]
pub enum SessionError {
    /// The server does not have this device pinned.
    NotPaired,
    /// The server answered with a key other than the one that was pinned.
    UnexpectedPeer,
    /// A signature over the transcript did not verify.
    AuthenticationFailed,
    Refused(String),
    Io(io::Error),
}

impl fmt::Display for SessionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotPaired => f.write_str("session.not_paired"),
            Self::UnexpectedPeer => f.write_str("session.unexpected_peer"),
            Self::AuthenticationFailed => f.write_str("session.authentication_failed"),
            Self::Refused(reason) => write!(f, "session.refused:{reason}"),
            Self::Io(error) => write!(f, "session.io:{error}"),
        }
    }
}

impl std::error::Error for SessionError {}

impl From<io::Error> for SessionError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// An authenticated, encrypted channel. Frame kinds travel as associated
/// data, so a kind cannot be altered without failing decryption.
pub struct SecureChannel {
    stream: TcpStream,
    peer: PeerKey,
    send: Aes256Gcm,
    receive: Aes256Gcm,
    sent: u64,
    received: u64,
}

impl fmt::Debug for SecureChannel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SecureChannel")
            .field("peer", &self.peer.fingerprint())
            .finish_non_exhaustive()
    }
}

impl SecureChannel {
    pub fn peer(&self) -> PeerKey {
        self.peer
    }

    pub fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.stream.set_read_timeout(timeout)
    }

    pub fn send(&mut self, kind: u8, plaintext: &[u8]) -> io::Result<()> {
        if plaintext.len() > MAX_CHUNK_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "chunk too large",
            ));
        }
        let nonce = counter_nonce(&mut self.sent)?;
        let ciphertext = self
            .send
            .encrypt(
                &nonce,
                Payload {
                    msg: plaintext,
                    aad: &[kind],
                },
            )
            .map_err(|_| io::Error::other("encryption failed"))?;
        write_frame(&mut self.stream, kind, &ciphertext)
    }

    pub fn receive(&mut self) -> io::Result<(u8, Vec<u8>)> {
        let (kind, ciphertext) = read_frame(&mut self.stream)?;
        let nonce = counter_nonce(&mut self.received)?;
        let plaintext = self
            .receive
            .decrypt(
                &nonce,
                Payload {
                    msg: &ciphertext,
                    aad: &[kind],
                },
            )
            .map_err(|_| invalid("frame failed authentication"))?;
        Ok((kind, plaintext))
    }
}

/// A nonce is never reused under one key: each direction has its own key and
/// its own counter, and the counter refuses to wrap.
fn counter_nonce(counter: &mut u64) -> io::Result<Nonce<Aes256Gcm>> {
    let mut bytes = [0_u8; 12];
    bytes[4..].copy_from_slice(&counter.to_be_bytes());
    *counter = counter
        .checked_add(1)
        .ok_or_else(|| invalid("nonce counter exhausted"))?;
    Ok(Nonce::<Aes256Gcm>::from(bytes))
}

fn signed_message(role: &[u8], th: &[u8; 32]) -> Vec<u8> {
    let mut message = Vec::with_capacity(TRANSCRIPT_LABEL.len() + 1 + role.len() + 32);
    message.extend_from_slice(TRANSCRIPT_LABEL);
    message.push(b'/');
    message.extend_from_slice(role);
    message.extend_from_slice(th);
    message
}

fn authenticated(payload: &[u8], role: &[u8], th: &[u8; 32], signer: &PeerKey) -> bool {
    let Ok(signature) = <[u8; 64]>::try_from(payload) else {
        return false;
    };
    signer.verify(&signed_message(role, th), &signature)
}

fn channel(
    stream: TcpStream,
    peer: PeerKey,
    th: &[u8; 32],
    shared: &[u8; 32],
    is_client: bool,
) -> SecureChannel {
    let okm = derive_pair(th, shared, TRAFFIC_INFO);
    let client_to_server = Aes256Gcm::new(&Key::<Aes256Gcm>::from(
        <[u8; 32]>::try_from(&okm[..32]).expect("fixed-size slice"),
    ));
    let server_to_client = Aes256Gcm::new(&Key::<Aes256Gcm>::from(
        <[u8; 32]>::try_from(&okm[32..]).expect("fixed-size slice"),
    ));
    let (send, receive) = if is_client {
        (client_to_server, server_to_client)
    } else {
        (server_to_client, client_to_server)
    };
    SecureChannel {
        stream,
        peer,
        send,
        receive,
        sent: 0,
        received: 0,
    }
}

/// Opens a session to a server that must present `expected` as its key.
pub fn connect_session(
    mut stream: TcpStream,
    identity: &DeviceIdentity,
    expected: &PeerKey,
) -> Result<SecureChannel, SessionError> {
    stream.set_read_timeout(Some(HANDSHAKE_TIMEOUT))?;
    stream.set_write_timeout(Some(HANDSHAKE_TIMEOUT))?;
    let ephemeral = Ephemeral::generate()?;
    let client_bytes = send_hello(
        &mut stream,
        &Hello {
            ephemeral: ephemeral.public,
            identity: identity.public_key(),
            nonce: random_bytes::<32>()?,
        },
    )?;
    let server_bytes = match expect(&mut stream, kind::HELLO)? {
        Received::Frame(bytes) => bytes,
        Received::Refused(reason) if reason == "not_paired" => {
            return Err(SessionError::NotPaired);
        }
        Received::Refused(reason) => return Err(SessionError::Refused(reason)),
    };
    let server = Hello::decode(&server_bytes)?;
    if server.identity != *expected {
        return Err(SessionError::UnexpectedPeer);
    }
    let th = transcript(TRANSCRIPT_LABEL, &client_bytes, &server_bytes);
    let shared: Zeroizing<[u8; 32]> = ephemeral.agree(&server.ephemeral)?;

    write_frame(
        &mut stream,
        kind::AUTH,
        &identity.sign(&signed_message(CLIENT, &th)),
    )?;
    let server_auth = match expect(&mut stream, kind::AUTH)? {
        Received::Frame(bytes) => bytes,
        Received::Refused(_) => return Err(SessionError::AuthenticationFailed),
    };
    if !authenticated(&server_auth, SERVER, &th, &server.identity) {
        return Err(SessionError::AuthenticationFailed);
    }
    Ok(channel(stream, server.identity, &th, &shared, true))
}

/// Accepts a session from a pinned client. An unpinned client receives one
/// refusal frame and nothing else.
pub fn accept_session(
    stream: TcpStream,
    identity: &DeviceIdentity,
    pins: &PinStore,
) -> Result<SecureChannel, SessionError> {
    accept_session_with(stream, identity, |key| pins.is_pinned(key))
}

/// As [`accept_session`], consulting pins through `is_pinned` at the moment
/// the client's key is known, so no lock is held across a handshake read.
pub(crate) fn accept_session_with(
    mut stream: TcpStream,
    identity: &DeviceIdentity,
    is_pinned: impl Fn(&PeerKey) -> bool,
) -> Result<SecureChannel, SessionError> {
    stream.set_read_timeout(Some(HANDSHAKE_TIMEOUT))?;
    stream.set_write_timeout(Some(HANDSHAKE_TIMEOUT))?;
    let Received::Frame(client_bytes) = expect(&mut stream, kind::HELLO)? else {
        return Err(SessionError::Refused("client_refused".to_owned()));
    };
    let client = Hello::decode(&client_bytes)?;
    if !is_pinned(&client.identity) {
        refuse(&mut stream, "not_paired");
        return Err(SessionError::NotPaired);
    }

    let ephemeral = Ephemeral::generate()?;
    let server_bytes = send_hello(
        &mut stream,
        &Hello {
            ephemeral: ephemeral.public,
            identity: identity.public_key(),
            nonce: random_bytes::<32>()?,
        },
    )?;
    let th = transcript(TRANSCRIPT_LABEL, &client_bytes, &server_bytes);
    let shared = ephemeral.agree(&client.ephemeral)?;

    let Received::Frame(client_auth) = expect(&mut stream, kind::AUTH)? else {
        return Err(SessionError::AuthenticationFailed);
    };
    // A client that merely copied a pinned public key fails here.
    if !authenticated(&client_auth, CLIENT, &th, &client.identity) {
        refuse(&mut stream, "authentication_failed");
        return Err(SessionError::AuthenticationFailed);
    }
    write_frame(
        &mut stream,
        kind::AUTH,
        &identity.sign(&signed_message(SERVER, &th)),
    )?;
    Ok(channel(stream, client.identity, &th, &shared, false))
}
