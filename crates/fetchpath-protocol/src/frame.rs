//! Framing and decoding.
//!
//! A frame is a 4-byte little-endian length followed by that many bytes of
//! UTF-8 JSON. A frame larger than [`MAX_FRAME_BYTES`], a zero length, or
//! unreadable JSON is a connection error: the peer is answered once where
//! possible and that connection is closed. Other connections are unaffected.

use crate::SCHEMA_VERSION;
use crate::command::CommandEnvelope;
use crate::error::ProtocolError;
use crate::ids::CommandId;
use crate::message::{Reply, ServerMessage};
use serde::Serialize;
use serde_json::{Map, Value};
use std::io::{self, Read, Write};

/// The largest frame either side accepts. Big enough for a full queue
/// listing of several thousand jobs; small enough that a hostile length
/// cannot make a peer allocate without bound.
pub const MAX_FRAME_BYTES: usize = 4 * 1024 * 1024;

const PREFIX_BYTES: usize = 4;

/// Command types this build understands. An envelope naming anything else is
/// refused with `contract.unknown_command` and its `command_id`, instead of
/// failing as unreadable.
pub const KNOWN_COMMANDS: &[&str] = &[
    "CreateJob",
    "CreateJobs",
    "Start",
    "Pause",
    "Resume",
    "Cancel",
    "Retry",
    "UpdatePolicy",
    "ResolveDestination",
    "SelectMedia",
    "RefreshSource",
    "RefreshMediaChoices",
    "RemoveJob",
    "ListJobs",
    "GetJob",
    "JobDetails",
    "InspectMedia",
    "InspectLink",
    "QueueStats",
    "History",
    "GetSettings",
    "TakeLinkReviews",
    "TakeBrowserCaptures",
    "UpdateSettings",
    "ApproveJob",
    "DenyJob",
    "GetAgentPolicies",
    "SetAgentPolicy",
    "ListRules",
    "AddRule",
    "RemoveRule",
    "CacheStatus",
    "ClearCache",
    "SubscribeJob",
    "SubscribeQueue",
    "EngineStatus",
    "EngineShutdown",
];

/// Serializes one message into a frame, refusing one over the limit.
pub fn encode_frame(message: &impl Serialize) -> Result<Vec<u8>, ProtocolError> {
    let body = serde_json::to_vec(message).map_err(ProtocolError::malformed)?;
    if body.len() > MAX_FRAME_BYTES {
        return Err(ProtocolError::too_large(body.len()));
    }
    let mut frame = Vec::with_capacity(PREFIX_BYTES + body.len());
    frame.extend_from_slice(&(body.len() as u32).to_le_bytes());
    frame.extend_from_slice(&body);
    Ok(frame)
}

/// A failure reading or writing frames on a stream.
#[derive(Debug)]
pub enum FrameError {
    Io(io::Error),
    Protocol(ProtocolError),
}

impl std::fmt::Display for FrameError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "{error}"),
            Self::Protocol(error) => write!(formatter, "{error}"),
        }
    }
}

impl std::error::Error for FrameError {}

impl From<io::Error> for FrameError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<ProtocolError> for FrameError {
    fn from(error: ProtocolError) -> Self {
        Self::Protocol(error)
    }
}

fn check_length(length: usize) -> Result<(), ProtocolError> {
    if length == 0 {
        Err(ProtocolError::malformed("an empty frame"))
    } else if length > MAX_FRAME_BYTES {
        Err(ProtocolError::too_large(length))
    } else {
        Ok(())
    }
}

/// Reads one frame's body. `Ok(None)` is a clean end of stream between
/// frames; an end inside a frame is an error. The length is checked before
/// anything is allocated.
pub fn read_frame(reader: &mut impl Read) -> Result<Option<Vec<u8>>, FrameError> {
    let mut prefix = [0_u8; PREFIX_BYTES];
    let mut filled = 0;
    while filled < PREFIX_BYTES {
        match reader.read(&mut prefix[filled..]) {
            Ok(0) if filled == 0 => return Ok(None),
            Ok(0) => return Err(io::Error::from(io::ErrorKind::UnexpectedEof).into()),
            Ok(read) => filled += read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error.into()),
        }
    }
    let length = u32::from_le_bytes(prefix) as usize;
    check_length(length)?;
    let mut body = vec![0; length];
    reader.read_exact(&mut body)?;
    Ok(Some(body))
}

pub fn write_frame(writer: &mut impl Write, message: &impl Serialize) -> Result<(), FrameError> {
    writer.write_all(&encode_frame(message)?)?;
    writer.flush()?;
    Ok(())
}

/// Splits frames out of bytes that arrive in arbitrary pieces, for a
/// non-blocking transport. After an error it refuses everything, because the
/// stream position can no longer be trusted.
#[derive(Debug, Default)]
pub struct FrameDecoder {
    buffer: Vec<u8>,
    failed: bool,
}

impl FrameDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds received bytes. Fails as soon as a length prefix is known to be
    /// invalid, without buffering the oversized body.
    pub fn push(&mut self, bytes: &[u8]) -> Result<(), ProtocolError> {
        self.ensure_usable()?;
        self.buffer.extend_from_slice(bytes);
        if self.buffer.len() >= PREFIX_BYTES {
            self.check_prefix()?;
        }
        Ok(())
    }

    /// The next complete frame's body, if one has fully arrived.
    pub fn next_frame(&mut self) -> Result<Option<Vec<u8>>, ProtocolError> {
        self.ensure_usable()?;
        if self.buffer.len() < PREFIX_BYTES {
            return Ok(None);
        }
        let length = self.check_prefix()?;
        if self.buffer.len() < PREFIX_BYTES + length {
            return Ok(None);
        }
        let body = self.buffer[PREFIX_BYTES..PREFIX_BYTES + length].to_vec();
        self.buffer.drain(..PREFIX_BYTES + length);
        if self.buffer.len() >= PREFIX_BYTES {
            self.check_prefix()?;
        }
        Ok(Some(body))
    }

    /// Bytes received but not yet returned as a frame.
    pub fn pending(&self) -> usize {
        self.buffer.len()
    }

    fn check_prefix(&mut self) -> Result<usize, ProtocolError> {
        let mut prefix = [0_u8; PREFIX_BYTES];
        prefix.copy_from_slice(&self.buffer[..PREFIX_BYTES]);
        let length = u32::from_le_bytes(prefix) as usize;
        if let Err(error) = check_length(length) {
            self.failed = true;
            self.buffer = Vec::new();
            return Err(error);
        }
        Ok(length)
    }

    fn ensure_usable(&self) -> Result<(), ProtocolError> {
        if self.failed {
            Err(ProtocolError::malformed("the stream failed earlier"))
        } else {
            Ok(())
        }
    }
}

/// A command the engine could not accept, with as much identity as could be
/// read so the reply can still be correlated.
#[derive(Clone, Debug, PartialEq)]
pub struct Rejected {
    pub command_id: Option<CommandId>,
    pub error: ProtocolError,
}

impl Rejected {
    pub fn into_reply(self) -> Reply {
        Reply::error(self.command_id, self.error)
    }
}

/// Reads a frame body as a JSON object.
fn object(body: &[u8]) -> Result<Map<String, Value>, ProtocolError> {
    match serde_json::from_slice(body).map_err(ProtocolError::malformed)? {
        Value::Object(object) => Ok(object),
        _ => Err(ProtocolError::malformed("a message is a JSON object")),
    }
}

/// Checks the major version before anything else, so a peer on another
/// version gets `contract.unsupported_version` rather than a parse error.
fn check_version(object: &Map<String, Value>) -> Result<(), ProtocolError> {
    match object.get("schema_version").and_then(Value::as_u64) {
        Some(version) if version == u64::from(SCHEMA_VERSION) => Ok(()),
        Some(version) => Err(ProtocolError::unsupported_version(
            u32::try_from(version).unwrap_or(u32::MAX),
        )),
        None => Err(ProtocolError::malformed(
            "schema_version is missing or not a number",
        )),
    }
}

/// Decodes a command frame on the engine side. Whatever is refused keeps the
/// `command_id` when one can be read, including from another version.
// Rejection is rare and carries the full error the peer is answered with.
#[allow(clippy::result_large_err)]
pub fn decode_command(body: &[u8]) -> Result<CommandEnvelope, Rejected> {
    let object = object(body).map_err(|error| Rejected {
        command_id: None,
        error,
    })?;
    let command_id = object
        .get("command_id")
        .and_then(Value::as_str)
        .and_then(|text| CommandId::try_from(text).ok());
    let reject = |error| Rejected {
        command_id: command_id.clone(),
        error,
    };
    check_version(&object).map_err(reject)?;
    let name = object
        .get("payload")
        .and_then(|payload| payload.get("type"))
        .and_then(Value::as_str)
        .ok_or_else(|| reject(ProtocolError::malformed("payload.type is missing")))?;
    if !KNOWN_COMMANDS.contains(&name) {
        return Err(reject(ProtocolError::unknown_command(name)));
    }
    serde_json::from_value(Value::Object(object))
        .map_err(|error| reject(ProtocolError::malformed(error)))
}

/// Decodes an engine frame on the client side.
pub fn decode_server_message(body: &[u8]) -> Result<ServerMessage, ProtocolError> {
    let object = object(body)?;
    check_version(&object)?;
    serde_json::from_value(Value::Object(object)).map_err(ProtocolError::malformed)
}
