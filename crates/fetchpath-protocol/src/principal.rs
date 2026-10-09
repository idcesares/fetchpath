//! Who is asking (platform design §6, contract D1).
//!
//! A connection declares its principal once, in the transport handshake.
//! The engine enforces what each principal may do; the declaration only says
//! which rules apply.

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::fmt;

/// Longest agent name accepted.
pub const MAX_AGENT_NAME: usize = 64;

/// The name a person gave an agent host, such as `claude-code`: 1 to 64
/// lowercase letters, digits, `-`, `_` or `.`, starting with a letter or
/// digit.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct AgentName(String);

impl AgentName {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for AgentName {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        let valid = !value.is_empty()
            && value.len() <= MAX_AGENT_NAME
            && value.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
            && value.chars().all(|c| {
                c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_' | '.')
            });
        if valid {
            Ok(Self(value))
        } else {
            Err(format!(
                "an agent name is 1 to {MAX_AGENT_NAME} lowercase letters, digits, '-', '_' or '.'"
            ))
        }
    }
}

impl TryFrom<&str> for AgentName {
    type Error = String;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::try_from(value.to_owned())
    }
}

impl From<AgentName> for String {
    fn from(value: AgentName) -> Self {
        value.0
    }
}

impl fmt::Display for AgentName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl JsonSchema for AgentName {
    fn schema_name() -> Cow<'static, str> {
        "AgentName".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "pattern": "^[a-z0-9][a-z0-9._-]{0,63}$"
        })
    }
}

/// Longest device id accepted.
pub const MAX_DEVICE_ID: usize = 64;

/// The engine-made id of one signed-in device, such as a browser session of
/// the local web UI (contract D6): 1 to 64 lowercase letters or digits. The
/// engine derives it from a credential; a connection never declares it.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct DeviceId(String);

impl DeviceId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for DeviceId {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        let valid = !value.is_empty()
            && value.len() <= MAX_DEVICE_ID
            && value
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
        if valid {
            Ok(Self(value))
        } else {
            Err(format!(
                "a device id is 1 to {MAX_DEVICE_ID} lowercase letters or digits"
            ))
        }
    }
}

impl From<DeviceId> for String {
    fn from(value: DeviceId) -> Self {
        value.0
    }
}

impl fmt::Display for DeviceId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Who a connection acts for. On the wire: `user`, `browser`,
/// `agent:<name>` or `device:<id>`.
#[derive(Clone, Debug, Default, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub enum Principal {
    /// The person, through the desktop, the command line or the terminal.
    #[default]
    User,
    /// The browser extension's native host: submits captures, nothing else.
    Browser,
    /// An agent host, such as an MCP client, under the policy the person
    /// granted it.
    Agent(AgentName),
    /// A device the person signed in, such as a local web UI browser session
    /// (contract D6). Acts as the person for viewing and submitting, but
    /// never approves, and settings and access stay with `user`.
    Device(DeviceId),
}

impl Principal {
    /// The person on this computer, through a client that may change
    /// settings, access and approvals.
    pub fn is_user(&self) -> bool {
        matches!(self, Self::User)
    }

    /// The person, locally or through a signed-in device: sees every job
    /// and is not held by agent policy.
    pub fn is_person(&self) -> bool {
        matches!(self, Self::User | Self::Device(_))
    }

    pub fn device(&self) -> Option<&DeviceId> {
        match self {
            Self::Device(id) => Some(id),
            _ => None,
        }
    }

    pub fn agent(&self) -> Option<&AgentName> {
        match self {
            Self::Agent(name) => Some(name),
            _ => None,
        }
    }
}

impl TryFrom<String> for Principal {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        match value.as_str() {
            "user" => Ok(Self::User),
            "browser" => Ok(Self::Browser),
            other => match (other.strip_prefix("agent:"), other.strip_prefix("device:")) {
                (Some(name), _) => AgentName::try_from(name).map(Self::Agent),
                (_, Some(id)) => DeviceId::try_from(id.to_owned()).map(Self::Device),
                _ => Err("a principal is user, browser, agent:<name> or device:<id>".into()),
            },
        }
    }
}

impl TryFrom<&str> for Principal {
    type Error = String;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::try_from(value.to_owned())
    }
}

impl From<Principal> for String {
    fn from(value: Principal) -> Self {
        value.to_string()
    }
}

impl fmt::Display for Principal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::User => formatter.write_str("user"),
            Self::Browser => formatter.write_str("browser"),
            Self::Agent(name) => write!(formatter, "agent:{name}"),
            Self::Device(id) => write!(formatter, "device:{id}"),
        }
    }
}

impl JsonSchema for Principal {
    fn schema_name() -> Cow<'static, str> {
        "Principal".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "pattern": "^(user|browser|agent:[a-z0-9][a-z0-9._-]{0,63}|device:[a-z0-9]{1,64})$",
            "description": "Who a connection acts for: user, browser, agent:<name> or device:<id>."
        })
    }
}

/// What the person allows one agent to do without asking.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AgentPolicy {
    /// Folders the agent may save into, as full paths. A destination counts
    /// as inside one only after links and junctions are resolved.
    #[serde(default)]
    pub folders: Vec<String>,
    /// Largest download, in bytes, before it stops and waits for approval.
    pub max_bytes: u64,
    /// New downloads per hour before further ones wait for approval.
    pub max_new_jobs_per_hour: u32,
    /// Automatic mode (contract D7): inside its folders the agent's
    /// downloads never wait for the person. The size limit and the hourly
    /// rate do not apply, and a torrent may contact peers. Saving outside
    /// its folders still waits; credentials, replacing files and uploading
    /// to peers stay refused or held as before. Off unless the person turns
    /// it on.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub automatic: bool,
}

impl AgentPolicy {
    pub const DEFAULT_MAX_BYTES: u64 = 1024 * 1024 * 1024;
    pub const DEFAULT_MAX_NEW_JOBS_PER_HOUR: u32 = 20;
    pub const MAX_FOLDERS: usize = 32;
    pub const MAX_NEW_JOBS_PER_HOUR_LIMIT: u32 = 1_000;
}

impl Default for AgentPolicy {
    /// An agent the person has not configured: no folders, so everything it
    /// asks for waits for approval.
    fn default() -> Self {
        Self {
            folders: Vec::new(),
            max_bytes: Self::DEFAULT_MAX_BYTES,
            max_new_jobs_per_hour: Self::DEFAULT_MAX_NEW_JOBS_PER_HOUR,
            automatic: false,
        }
    }
}

/// One agent and its access.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AgentAccess {
    pub agent: AgentName,
    pub policy: AgentPolicy,
}

/// Why a job waits for the person.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalReason {
    /// The destination is not inside a folder granted to the agent.
    OutsideGrantedFolders,
    /// The download passed the agent's size limit and stopped.
    SizeLimit,
    /// The agent asked for more new downloads per hour than it may.
    RateLimit,
    /// A torrent request would contact peers, trackers or the DHT.
    PeerDiscovery,
    /// A torrent request would serve pieces to other peers.
    PeerUpload,
    #[serde(other)]
    Unknown,
}

/// A job's wait for approval, as clients see it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ApprovalRequest {
    pub reasons: Vec<ApprovalReason>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn principals_round_trip_and_reject_anything_else() {
        for text in [
            "user",
            "browser",
            "agent:claude-code",
            "agent:a.b_c-1",
            "device:0f3a",
        ] {
            let principal = Principal::try_from(text).unwrap();
            assert_eq!(principal.to_string(), text);
        }
        for text in [
            "",
            "User",
            "agent:",
            "agent:Claude",
            "agent:-x",
            "agent:a b",
            "agent:a/b",
            "agent:user\u{0}",
            "root",
            "device:",
            "device:AB",
            "device:a-b",
        ] {
            assert!(Principal::try_from(text).is_err(), "{text:?}");
        }
        assert!(Principal::try_from(format!("agent:{}", "a".repeat(65))).is_err());
        assert!(Principal::try_from(format!("device:{}", "a".repeat(65))).is_err());
        assert!(Principal::try_from("device:0f3a").unwrap().is_person());
        assert!(!Principal::try_from("device:0f3a").unwrap().is_user());
        assert_eq!(Principal::default(), Principal::User);
    }
}
