//! Send jobs, the manifest and the inbox (contract D8, FP-115).
//!
//! A send hands a file that already exists on this machine to another
//! principal of the same engine. There is no network path, link or
//! off-machine destination in this slice.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::ids::{JobId, Timestamp};
use crate::principal::Principal;

/// Shown wherever a manifest's hash is shown. It must not be reworded to
/// suggest the publisher vouched for the file (contract D8).
pub const HASH_NOTICE: &str = "SHA-256 computed by the sending Fetchpath engine. It shows that what you receive matches what was offered. It does not show who published the original file.";

/// Shown beside the sender label.
pub const SENDER_NOTICE: &str = "Declared by the program that connected, not verified.";

/// What an offer points at. The engine decides whether the source is
/// offerable; the caller only names it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OfferSource {
    /// The published output of a completed download.
    Job { job_id: JobId },
    /// A file by path: a person's pick, or for an agent a file inside a
    /// folder granted to it.
    Path { path: String },
}

/// Who attested the manifest's hash.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum HashSource {
    SenderEngine,
    #[serde(other)]
    Unknown,
}

/// What the recipient is told about an offer. Built by the engine from the
/// file, never from the caller's words. It holds no filesystem path.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Manifest {
    pub file_name: String,
    pub size: u64,
    pub sha256: String,
    pub hash_source: HashSource,
    /// The download's link without query or user info; absent when unknown
    /// or when the file was picked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
    /// `user` or `agent:<name>`. A declared label, not proof of identity.
    pub sender_label: String,
    pub offered_at: Timestamp,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OfferState {
    Offered,
    Copying,
    Completed,
    Declined,
    Withdrawn,
    Expired,
    Failed,
    #[serde(other)]
    Unknown,
}

impl OfferState {
    pub fn is_open(self) -> bool {
        matches!(self, Self::Offered | Self::Copying)
    }
}

/// One send job as a client sees it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct OfferView {
    pub job_id: JobId,
    pub state: OfferState,
    pub manifest: Manifest,
    pub sender: Principal,
    pub recipient: Principal,
    /// Where the copy was saved. Shown to a person; to an agent only when it
    /// lies inside the agent's granted folders.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub saved_to: Option<String>,
    /// Why the offer ended without a copy (`send.offer_declined`,
    /// `send.offer_expired`, `send.changed`, `send.source_missing`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}
