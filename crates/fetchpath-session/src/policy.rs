//! Principals and agent policy (contract D1).
//!
//! The engine asks this module two questions: may this principal send this
//! command at all, and does an agent's new download fall inside what the
//! person granted. Commands are allowed by an explicit list, so a command
//! added later is closed to agents until someone decides otherwise.

use crate::MAX_DESTINATION_LENGTH;
use fetchpath_protocol::command::{Command, DestinationDecision, JobInput};
use fetchpath_protocol::error::{Action, ErrorCode, ErrorScope, ProtocolError};
use fetchpath_protocol::principal::{AgentName, AgentPolicy, ApprovalReason, Principal};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::{Component, Path, PathBuf};

/// Approvals one agent may have waiting at once. Beyond this a misled agent
/// is refused rather than burying the person in requests.
pub const MAX_PENDING_APPROVALS: usize = 20;
/// The window the new-job rate is counted over.
const RATE_WINDOW_MS: u64 = 60 * 60 * 1_000;
pub(crate) const AGENTS_FILE: &str = "agents-v1.json";
const AGENTS_SCHEMA_VERSION: u32 = 1;

fn policy_error(code: &'static str, message: impl Into<String>) -> ProtocolError {
    ProtocolError::new(
        ErrorCode::try_from(code.to_owned()).expect("a valid built-in code"),
        ErrorScope::Command,
        message,
    )
    .with_action(Action::ChangePolicy)
}

pub(crate) fn not_permitted(principal: &Principal, command: &Command) -> ProtocolError {
    policy_error(
        "policy.not_permitted",
        format!(
            "{} cannot use {}; only the person can.",
            match principal {
                Principal::Browser => "The browser extension".to_owned(),
                Principal::Agent(name) => format!("The agent {name}"),
                Principal::User => "This client".to_owned(),
            },
            command.name()
        ),
    )
}

pub(crate) fn credentials_not_allowed() -> ProtocolError {
    policy_error(
        "policy.credentials_not_allowed",
        "An agent cannot send a link with a user name or password, or use a stored browser \
         capture. Ask the person to add this download themselves.",
    )
}

pub(crate) fn replace_not_allowed() -> ProtocolError {
    policy_error(
        "policy.replace_not_allowed",
        "An agent cannot replace an existing file. Choose another file name.",
    )
}

pub(crate) fn too_many_pending() -> ProtocolError {
    policy_error(
        "policy.too_many_pending",
        format!(
            "{MAX_PENDING_APPROVALS} downloads from this agent are already waiting for the \
             person's approval. Wait for them to be decided."
        ),
    )
}

/// May `principal` send `command` at all? Ownership of the job it names is
/// checked separately, and answers as if the job did not exist.
pub(crate) fn authorize(principal: &Principal, command: &Command) -> Result<(), ProtocolError> {
    let allowed = match principal {
        Principal::User => true,
        Principal::Browser => matches!(
            command,
            Command::CreateJob { .. } | Command::TakeBrowserCaptures
        ),
        Principal::Agent(_) => match command {
            Command::CreateJob { .. }
            | Command::Start { .. }
            | Command::Pause { .. }
            | Command::Resume { .. }
            | Command::Cancel { .. }
            | Command::Retry { .. }
            | Command::UpdatePolicy { .. }
            | Command::RefreshSource { .. }
            | Command::RemoveJob { .. }
            | Command::ListJobs { .. }
            | Command::GetJob { .. }
            | Command::JobDetails { .. }
            | Command::InspectMedia { .. }
            | Command::InspectLink { .. }
            | Command::History { .. }
            | Command::SubscribeJob { .. }
            | Command::EngineStatus => true,
            Command::ResolveDestination { decision, .. } => {
                !matches!(decision, DestinationDecision::ReplaceExisting)
            }
            _ => false,
        },
    };
    if allowed {
        return Ok(());
    }
    if let Command::ResolveDestination {
        decision: DestinationDecision::ReplaceExisting,
        ..
    } = command
    {
        return Err(replace_not_allowed());
    }
    Err(not_permitted(principal, command))
}

/// True when the link carries a user name or password, which an agent may
/// not send.
pub(crate) fn has_userinfo(url: &str) -> bool {
    let Some((_, rest)) = url.split_once("://") else {
        return false;
    };
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    rest[..authority_end].contains('@')
}

/// Refuses what an agent may never send, even for approval.
pub(crate) fn check_agent_input(input: &JobInput) -> Result<(), ProtocolError> {
    match input {
        JobInput::CredentialRef { .. } => Err(credentials_not_allowed()),
        JobInput::Url { url } if has_userinfo(url.expose()) => Err(credentials_not_allowed()),
        JobInput::Url { .. } => Ok(()),
    }
}

/// Resolves every link and junction on the way to `path`: the deepest
/// existing ancestor is canonicalized and the missing rest appended. `None`
/// when nothing on the path exists or it contains `..`.
fn resolved(path: &Path) -> Option<PathBuf> {
    if path
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return None;
    }
    let mut existing = path;
    let mut missing = Vec::new();
    loop {
        if let Ok(canonical) = std::fs::canonicalize(existing) {
            let mut full = canonical;
            for part in missing.iter().rev() {
                full.push(part);
            }
            return Some(full);
        }
        missing.push(existing.file_name()?.to_owned());
        existing = existing.parent()?;
    }
}

/// True when `destination` lies inside one of the granted folders once links
/// and junctions are resolved. A grant that does not exist grants nothing.
pub(crate) fn inside_grants(destination: &Path, folders: &[String]) -> bool {
    let Some(directory) = destination.parent().and_then(resolved) else {
        return false;
    };
    folders.iter().any(|folder| {
        std::fs::canonicalize(folder)
            .ok()
            .filter(|grant| grant.is_dir())
            .is_some_and(|grant| directory.starts_with(&grant))
    })
}

/// Checks a policy the person set before it is stored.
pub(crate) fn validated(mut policy: AgentPolicy) -> Result<AgentPolicy, String> {
    if policy.folders.len() > AgentPolicy::MAX_FOLDERS {
        return Err(format!(
            "An agent can be given at most {} folders.",
            AgentPolicy::MAX_FOLDERS
        ));
    }
    for folder in &policy.folders {
        let path = Path::new(folder);
        let full =
            path.is_absolute() && matches!(path.components().next(), Some(Component::Prefix(_)));
        if folder.is_empty()
            || folder.len() > MAX_DESTINATION_LENGTH
            || folder.chars().any(char::is_control)
            || !full
            || path
                .components()
                .any(|component| matches!(component, Component::ParentDir))
        {
            return Err(format!(
                "{folder:?} is not a full folder path. Choose a folder including its drive."
            ));
        }
    }
    if policy.max_bytes == 0 {
        return Err("An agent's size limit must be at least one byte.".into());
    }
    policy.max_new_jobs_per_hour = policy
        .max_new_jobs_per_hour
        .clamp(1, AgentPolicy::MAX_NEW_JOBS_PER_HOUR_LIMIT);
    Ok(policy)
}

/// New jobs each agent created in the last hour. In memory: after a restart
/// the window starts empty.
#[derive(Default)]
pub(crate) struct RateWindow(HashMap<AgentName, VecDeque<u64>>);

impl RateWindow {
    /// True when one more job now would exceed `per_hour`.
    pub fn exceeded(&mut self, agent: &AgentName, per_hour: u32, now_ms: u64) -> bool {
        let times = self.0.entry(agent.clone()).or_default();
        while times
            .front()
            .is_some_and(|created| now_ms.saturating_sub(*created) >= RATE_WINDOW_MS)
        {
            times.pop_front();
        }
        times.len() >= per_hour as usize
    }

    pub fn record(&mut self, agent: &AgentName, now_ms: u64) {
        self.0.entry(agent.clone()).or_default().push_back(now_ms);
    }
}

/// The agents file: each configured agent and its policy.
#[derive(Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AgentsFile {
    schema_version: u32,
    #[serde(default)]
    agents: BTreeMap<AgentName, AgentPolicy>,
}

impl AgentsFile {
    pub fn new(agents: BTreeMap<AgentName, AgentPolicy>) -> Self {
        Self {
            schema_version: AGENTS_SCHEMA_VERSION,
            agents,
        }
    }

    /// Reads the file. A missing, unreadable or newer file grants nothing,
    /// so every agent request waits for the person rather than passing.
    pub fn load(path: &Path) -> BTreeMap<AgentName, AgentPolicy> {
        std::fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Self>(&bytes).ok())
            .filter(|file| file.schema_version == AGENTS_SCHEMA_VERSION)
            .map(|file| {
                file.agents
                    .into_iter()
                    .filter_map(|(name, policy)| {
                        validated(policy).ok().map(|policy| (name, policy))
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// Why a new agent job must wait, given its destination and the agent's
/// policy. Empty when it may start.
pub(crate) fn creation_reasons(
    destination: &Path,
    policy: &AgentPolicy,
    rate_exceeded: bool,
) -> Vec<ApprovalReason> {
    let mut reasons = Vec::new();
    if !inside_grants(destination, &policy.folders) {
        reasons.push(ApprovalReason::OutsideGrantedFolders);
    }
    if rate_exceeded {
        reasons.push(ApprovalReason::RateLimit);
    }
    reasons
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_info_is_found_only_in_the_authority() {
        assert!(has_userinfo("https://u:p@example.test/a"));
        assert!(has_userinfo("https://token@example.test"));
        assert!(!has_userinfo("https://example.test/a@b"));
        assert!(!has_userinfo("https://example.test/a?x=a@b"));
        assert!(!has_userinfo("https://example.test#a@b"));
    }

    #[test]
    fn the_rate_window_forgets_jobs_older_than_an_hour() {
        let agent = AgentName::try_from("a").unwrap();
        let mut window = RateWindow::default();
        window.record(&agent, 0);
        window.record(&agent, 1_000);
        assert!(window.exceeded(&agent, 2, 2_000));
        assert!(!window.exceeded(&agent, 2, RATE_WINDOW_MS));
        assert!(!window.exceeded(&agent, 2, RATE_WINDOW_MS + 1_000));
    }

    #[test]
    fn a_policy_needs_full_folders_and_a_positive_limit() {
        let good = AgentPolicy {
            folders: vec![r"C:\Users\me\Downloads".into()],
            max_bytes: 1,
            max_new_jobs_per_hour: 0,
        };
        assert_eq!(validated(good).unwrap().max_new_jobs_per_hour, 1);
        for folder in ["relative", r"\no-drive", r"C:\a\..\b", ""] {
            let policy = AgentPolicy {
                folders: vec![folder.into()],
                ..AgentPolicy::default()
            };
            assert!(validated(policy).is_err(), "{folder:?}");
        }
        let zero = AgentPolicy {
            max_bytes: 0,
            ..AgentPolicy::default()
        };
        assert!(validated(zero).is_err());
    }
}
