//! Errors and the actions clients take on them (job contract §9).
//!
//! Clients render buttons, prompts and exit codes from `code` and `action`,
//! never from `message`, which is fallback text for a person to read.

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::fmt;

/// A stable error code: a family and a name, such as `input.invalid_url`.
#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ErrorCode(Cow<'static, str>);

impl ErrorCode {
    // Protocol and contract failures.
    pub const UNSUPPORTED_VERSION: Self = Self::known("contract.unsupported_version");
    pub const MALFORMED_MESSAGE: Self = Self::known("contract.malformed_message");
    pub const MESSAGE_TOO_LARGE: Self = Self::known("contract.message_too_large");
    pub const UNKNOWN_COMMAND: Self = Self::known("contract.unknown_command");
    pub const IDEMPOTENCY_CONFLICT: Self = Self::known("contract.idempotency_conflict");
    pub const COMMAND_EXPIRED: Self = Self::known("contract.command_expired");
    pub const CLOCK_SKEW: Self = Self::known("contract.clock_skew");
    pub const REVISION_CONFLICT: Self = Self::known("contract.revision_conflict");
    pub const INVALID_TRANSITION: Self = Self::known("contract.invalid_transition");
    pub const UNKNOWN_JOB: Self = Self::known("contract.unknown_job");
    /// A client that cannot reach the engine at all.
    pub const ENGINE_UNAVAILABLE: Self = Self::known("contract.engine_unavailable");
    /// The unrecognized error the contract requires clients to treat as not
    /// retryable.
    pub const INTERNAL_UNKNOWN: Self = Self::known("internal.unknown");

    const fn known(code: &'static str) -> Self {
        Self(Cow::Borrowed(code))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The part before the dot, which decides a client's fallback behavior
    /// for a code it does not know.
    pub fn family(&self) -> ErrorFamily {
        match self.as_str().split_once('.').map(|(family, _)| family) {
            Some("input") => ErrorFamily::Input,
            Some("auth") => ErrorFamily::Auth,
            Some("source") => ErrorFamily::Source,
            Some("integrity") => ErrorFamily::Integrity,
            Some("storage") => ErrorFamily::Storage,
            Some("resource") => ErrorFamily::Resource,
            Some("media") => ErrorFamily::Media,
            Some("contract") => ErrorFamily::Contract,
            Some("policy") => ErrorFamily::Policy,
            _ => ErrorFamily::Internal,
        }
    }
}

fn is_valid_code(value: &str) -> bool {
    let Some((family, name)) = value.split_once('.') else {
        return false;
    };
    let word = |part: &str| {
        !part.is_empty()
            && part.len() <= 64
            && part.starts_with(|c: char| c.is_ascii_lowercase())
            && part
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
    };
    word(family) && name.split('.').all(word)
}

impl TryFrom<String> for ErrorCode {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        if is_valid_code(&value) {
            Ok(Self(Cow::Owned(value)))
        } else {
            Err("an error code is a lowercase family.name".into())
        }
    }
}

impl From<ErrorCode> for String {
    fn from(value: ErrorCode) -> Self {
        value.0.into_owned()
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl JsonSchema for ErrorCode {
    fn schema_name() -> Cow<'static, str> {
        "ErrorCode".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "pattern": "^[a-z][a-z0-9_]{0,63}(\\.[a-z][a-z0-9_]{0,63})+$"
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ErrorFamily {
    Input,
    Auth,
    Source,
    Integrity,
    Storage,
    Resource,
    Media,
    Contract,
    /// Not permitted for this principal, or refused by the person
    /// (contract D1).
    Policy,
    Internal,
}

/// The next step a person or the queue takes. A client that meets `unknown`
/// (a value from a newer engine) offers no action rather than guessing.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    /// Try again; the engine may already be doing so automatically.
    Retry,
    /// Correct the submitted input.
    CorrectInput,
    /// Paste a refreshed link; private query values were not kept.
    EditLink,
    /// Send the download from the browser again.
    Recapture,
    /// Pick another destination.
    ChooseNewPath,
    /// Explicitly replace the existing file. Only a person may choose this.
    ReplaceExisting,
    /// Check the expected SHA-256 that was supplied.
    CheckChecksum,
    /// Set up or repair the media helpers.
    ConfigureMediaTools,
    /// Inspect the media page again for a fresh set of choices.
    RefreshMediaChoices,
    /// The source's session expired; refresh the source and inspect it again.
    RefreshSource,
    /// The selected format vanished; create a linked replacement job.
    CreateReplacementJob,
    /// Refresh the sign-in or other authorization.
    RefreshAuthorization,
    /// Free disk space.
    FreeSpace,
    /// Lower concurrency or change policy.
    ChangePolicy,
    /// Restart the engine or update Fetchpath so versions agree.
    UpdateSoftware,
    /// Fetch a fresh snapshot and try again.
    RefreshClient,
    /// Wait; the engine will continue on its own.
    Wait,
    /// Wait for the person to approve or deny the job in Fetchpath. An
    /// agent relays this rather than retrying.
    AwaitApproval,
    #[serde(other)]
    Unknown,
}

/// What an error is about.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ErrorScope {
    /// One frame or connection; other connections are unaffected.
    Connection,
    /// One command; nothing was changed.
    Command,
    /// One job's recorded failure.
    Job,
    /// The engine as a whole.
    Engine,
    #[serde(other)]
    Unknown,
}

/// A protocol error. Diagnostics are redacted before they reach this type:
/// no cookies, authorization headers, signed query strings or private paths.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ProtocolError {
    pub code: ErrorCode,
    /// Stable key for localized text.
    pub message_key: String,
    /// Plain-language fallback text, already redacted.
    pub message: String,
    pub retryable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<Action>,
    pub scope: ErrorScope,
    /// A bounded wait before retrying, when the error is retryable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_after_seconds: Option<u64>,
    /// With `contract.revision_conflict`: the job's current revision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_revision: Option<u64>,
}

impl ProtocolError {
    /// An error with `message_key` equal to its code and no action.
    pub fn new(code: ErrorCode, scope: ErrorScope, message: impl Into<String>) -> Self {
        Self {
            message_key: code.as_str().to_owned(),
            code,
            message: message.into(),
            retryable: false,
            action: None,
            scope,
            retry_after_seconds: None,
            current_revision: None,
        }
    }

    pub fn with_action(mut self, action: Action) -> Self {
        self.action = Some(action);
        self
    }

    pub fn unsupported_version(received: u32) -> Self {
        Self::new(
            ErrorCode::UNSUPPORTED_VERSION,
            ErrorScope::Connection,
            format!(
                "This Fetchpath speaks protocol version {}, but the other side sent version \
                 {received}. Restart the Fetchpath engine or update Fetchpath so both match.",
                crate::SCHEMA_VERSION
            ),
        )
        .with_action(Action::UpdateSoftware)
    }

    pub fn malformed(detail: impl fmt::Display) -> Self {
        Self::new(
            ErrorCode::MALFORMED_MESSAGE,
            ErrorScope::Connection,
            format!("The message could not be read: {detail}"),
        )
    }

    pub fn too_large(length: usize) -> Self {
        Self::new(
            ErrorCode::MESSAGE_TOO_LARGE,
            ErrorScope::Connection,
            format!(
                "A {length}-byte message exceeds the {}-byte limit.",
                crate::frame::MAX_FRAME_BYTES
            ),
        )
    }

    pub fn unknown_command(name: &str) -> Self {
        // The name comes from the peer; it is bounded and printable before
        // it is echoed.
        let shown: String = name
            .chars()
            .filter(|c| c.is_ascii_graphic())
            .take(64)
            .collect();
        Self::new(
            ErrorCode::UNKNOWN_COMMAND,
            ErrorScope::Command,
            format!("This engine does not know the command {shown:?}."),
        )
        .with_action(Action::UpdateSoftware)
    }
}

impl fmt::Display for ProtocolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for ProtocolError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_validated_and_grouped_by_family() {
        assert_eq!(
            ErrorCode::try_from("storage.disk_full".to_owned())
                .unwrap()
                .family(),
            ErrorFamily::Storage
        );
        assert_eq!(
            ErrorCode::try_from("vendor.thing".to_owned())
                .unwrap()
                .family(),
            ErrorFamily::Internal
        );
        for bad in ["", "input", "Input.bad", "input.", ".x", "input.bad-name"] {
            assert!(ErrorCode::try_from(bad.to_owned()).is_err(), "{bad}");
        }
        assert_eq!(
            ErrorCode::UNSUPPORTED_VERSION.family(),
            ErrorFamily::Contract
        );
    }

    #[test]
    fn an_action_from_a_newer_engine_reads_as_unknown() {
        let action: Action = serde_json::from_str("\"teleport\"").unwrap();
        assert_eq!(action, Action::Unknown);
        let known: Action = serde_json::from_str("\"choose_new_path\"").unwrap();
        assert_eq!(known, Action::ChooseNewPath);
    }

    #[test]
    fn unknown_command_names_are_bounded_before_they_are_echoed() {
        let error = ProtocolError::unknown_command(&format!("{}\u{1b}[2J", "A".repeat(500)));
        assert!(!error.message.contains('\u{1b}'));
        assert!(error.message.len() < 200);
    }
}
