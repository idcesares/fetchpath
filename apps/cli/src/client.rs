//! Reaching the engine from a command, and what every queue command shares:
//! job references, exit codes, and plain or `--json` output (FP-058).

use crate::download::{
    EXIT_CANCELLED, EXIT_CHECKSUM, EXIT_CONFLICT, EXIT_NETWORK, EXIT_STORAGE, EXIT_USAGE,
};
use fetchpath_protocol::client::Subscription;
use fetchpath_protocol::command::{Command, CommandEnvelope, JobFilter};
use fetchpath_protocol::error::{ErrorCode, ErrorFamily, ErrorScope};
use fetchpath_protocol::launch::{self, EngineHome, LAUNCH_WAIT};
use fetchpath_protocol::message::CommandResult;
use fetchpath_protocol::model::{JobState, Progress};
use fetchpath_protocol::pipe::{Limits, PipeEngineClient};
use fetchpath_protocol::principal::Principal;
use fetchpath_protocol::{ClientId, EngineClient, JobSnapshot, ProtocolError};
use serde::Serialize;
use std::sync::atomic::{AtomicBool, Ordering};

/// Any failure that is not the person's input, a file, the network or a
/// checksum: the engine could not be reached, or something inside failed.
pub const EXIT_ENGINE: i32 = 1;

/// A connection to the engine, started if none is running.
pub struct Engine {
    client: PipeEngineClient,
    id: ClientId,
}

impl Engine {
    pub fn connect() -> Result<Self, ProtocolError> {
        Self::connect_as(Principal::User)
    }

    /// Connects as `principal`, which the engine then holds this
    /// connection to (the MCP server connects as its agent).
    pub fn connect_as(principal: Principal) -> Result<Self, ProtocolError> {
        let home = EngineHome::from_env()?;
        let exe = std::env::current_exe().map_err(|error| {
            ProtocolError::new(
                ErrorCode::ENGINE_UNAVAILABLE,
                ErrorScope::Engine,
                format!("Fetchpath could not find its own program file: {error}"),
            )
        })?;
        let client = launch::attach_or_launch(&home, &exe, Limits::default(), LAUNCH_WAIT)?
            .with_principal(principal);
        Ok(Self {
            client,
            id: ClientId::random(),
        })
    }

    /// Connects to a running engine only; never starts one.
    pub fn attach() -> Result<Self, ProtocolError> {
        Self::attach_as(Principal::User)
    }

    /// Connects to a running engine only, as `principal`.
    pub fn attach_as(principal: Principal) -> Result<Self, ProtocolError> {
        let client =
            launch::attach(&EngineHome::from_env()?, Limits::default())?.with_principal(principal);
        Ok(Self {
            client,
            id: ClientId::random(),
        })
    }

    pub fn send(&self, command: Command) -> Result<CommandResult, ProtocolError> {
        self.client.send(&self.id, command)
    }

    pub fn subscribe(&self, command: Command) -> Result<Subscription, ProtocolError> {
        self.client
            .subscribe(&CommandEnvelope::new(self.id.clone(), command))
    }

    /// Every job, in the engine's order (newest first), which is also the
    /// order queue indexes count in.
    pub fn jobs(&self, filter: JobFilter) -> Result<Vec<JobSnapshot>, ProtocolError> {
        match self.send(Command::ListJobs { filter })? {
            CommandResult::Jobs { jobs } => Ok(jobs),
            other => Err(unexpected(&other)),
        }
    }

    pub fn job(&self, job_id: &fetchpath_protocol::JobId) -> Result<JobSnapshot, ProtocolError> {
        match self.send(Command::GetJob {
            job_id: job_id.clone(),
        })? {
            CommandResult::Job { job } => Ok(job),
            other => Err(unexpected(&other)),
        }
    }

    /// Resolves every reference against one listing, so indexes mean what
    /// `fetchpath ls` showed at that moment for all of them.
    pub fn resolve(&self, references: &[String]) -> Result<Vec<JobSnapshot>, ProtocolError> {
        let jobs = self.jobs(JobFilter::All)?;
        references
            .iter()
            .map(|reference| {
                resolve(&jobs, reference)
                    .cloned()
                    .map_err(|message| input_error(&message))
            })
            .collect()
    }
}

pub fn unexpected(result: &CommandResult) -> ProtocolError {
    ProtocolError::new(
        ErrorCode::INTERNAL_UNKNOWN,
        ErrorScope::Command,
        format!("The engine answered with an unexpected result ({result:?})."),
    )
}

pub fn input_error(message: &str) -> ProtocolError {
    ProtocolError::new(
        ErrorCode::try_from("input.invalid_request".to_owned()).expect("valid code"),
        ErrorScope::Command,
        message,
    )
}

/// A queue index is a number of up to four digits, counted from 1 in the
/// order `fetchpath ls` prints. Anything else is the start of a job id, and
/// must match exactly one job.
pub fn resolve<'a>(jobs: &'a [JobSnapshot], reference: &str) -> Result<&'a JobSnapshot, String> {
    let reference = reference.trim();
    if reference.is_empty() {
        return Err("A job reference cannot be empty.".into());
    }
    if reference.len() <= 4 && reference.bytes().all(|byte| byte.is_ascii_digit()) {
        let index: usize = reference.parse().unwrap_or(0);
        return index
            .checked_sub(1)
            .and_then(|position| jobs.get(position))
            .ok_or_else(|| {
                format!(
                    "There is no download number {reference}; the list has {}.",
                    jobs.len()
                )
            });
    }
    let prefix = reference.to_ascii_lowercase();
    let mut matches = jobs
        .iter()
        .filter(|job| job.job_id.as_str().starts_with(&prefix));
    match (matches.next(), matches.next()) {
        (Some(job), None) => Ok(job),
        (None, _) => Err(format!("No download has an id starting with {reference}.")),
        (Some(_), Some(_)) => Err(format!(
            "More than one download has an id starting with {reference}; give more of it."
        )),
    }
}

/// The index `fetchpath ls` shows for a job, from 1.
pub fn index_of(jobs: &[JobSnapshot], job: &JobSnapshot) -> Option<usize> {
    jobs.iter()
        .position(|candidate| candidate.job_id == job.job_id)
        .map(|position| position + 1)
}

/// The exit code for an error, from its code alone, never its text.
pub fn exit_code(error: &ProtocolError) -> i32 {
    let code = error.code.as_str();
    match error.code.family() {
        ErrorFamily::Input => EXIT_USAGE,
        ErrorFamily::Storage if code.ends_with("destination_conflict") => EXIT_CONFLICT,
        ErrorFamily::Media if code.ends_with("destination_conflict") => EXIT_CONFLICT,
        ErrorFamily::Storage => EXIT_STORAGE,
        ErrorFamily::Integrity => EXIT_CHECKSUM,
        ErrorFamily::Source | ErrorFamily::Auth => EXIT_NETWORK,
        _ if matches!(
            code,
            "contract.unknown_job" | "contract.invalid_transition" | "contract.unsupported"
        ) =>
        {
            EXIT_USAGE
        }
        _ => EXIT_ENGINE,
    }
}

/// The exit code a finished job ends a waiting command with.
pub fn job_exit_code(job: &JobSnapshot) -> i32 {
    match job.state {
        JobState::Completed => 0,
        JobState::Cancelled => EXIT_CANCELLED,
        _ => job.error.as_ref().map_or(EXIT_ENGINE, exit_code),
    }
}

/// Prints an error for a person, or as `{"error": …}` on standard output,
/// and returns its exit code.
pub fn fail(error: &ProtocolError, json: bool) -> i32 {
    if json {
        print_json(&serde_json::json!({ "error": error }));
    } else {
        eprintln!("fetchpath: {}", error.message);
    }
    exit_code(error)
}

pub fn print_json(value: &impl Serialize) {
    println!(
        "{}",
        serde_json::to_string(value).expect("protocol types serialize")
    );
}

/// Set by Ctrl+C while a command waits.
pub static INTERRUPTED: AtomicBool = AtomicBool::new(false);

pub fn catch_interrupt() {
    // Failing to install it only means Ctrl+C ends the process at once.
    let _ = ctrlc::set_handler(|| INTERRUPTED.store(true, Ordering::SeqCst));
}

pub fn interrupted() -> bool {
    INTERRUPTED.load(Ordering::SeqCst)
}

/// A job is settled when nothing more will happen without a person: done,
/// cancelled, failed with no automatic retry due, or waiting for a new link.
pub fn settled(job: &JobSnapshot) -> bool {
    match job.state {
        JobState::Completed | JobState::Cancelled | JobState::WaitingForSource => true,
        JobState::Failed => job.retry_at.is_none(),
        _ => false,
    }
}

/// The short id shown in lists: enough to type back as a reference.
pub fn short_id(job: &JobSnapshot) -> &str {
    &job.job_id.as_str()[..8]
}

/// The file name, or the link when there is none yet.
pub fn name(job: &JobSnapshot) -> String {
    job.destination
        .as_deref()
        .and_then(|path| std::path::Path::new(path).file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| job.source_display.clone())
}

/// The state as a person reads it: a queued job with a start time is
/// scheduled, a failure with a retry due is retrying.
pub fn state_label(job: &JobSnapshot) -> &'static str {
    match job.state {
        // Its drive has no room above the disk reserve yet (FP-101).
        JobState::Queued
            if job.waiting_reason
                == Some(fetchpath_protocol::model::WaitingReason::StorageReserve) =>
        {
            "needs disk space"
        }
        JobState::Queued if job.not_before.is_some() => "scheduled",
        JobState::Failed if job.retry_at.is_some() => "retrying",
        // Copied from the cache, not transferred (FP-032).
        JobState::Completed if job.reused_from_cache => "from cache",
        JobState::Completed if job.from_paired_device.is_some() => "from paired",
        state => state_name(state),
    }
}

pub fn state_name(state: JobState) -> &'static str {
    match state {
        JobState::Queued => "queued",
        JobState::Probing => "probing",
        JobState::WaitingForSelection => "choose format",
        JobState::Ready => "ready",
        JobState::Running => "running",
        JobState::Pausing => "pausing",
        JobState::Paused => "paused",
        JobState::WaitingForSource => "needs link",
        JobState::Verifying => "verifying",
        JobState::Publishing => "saving",
        JobState::Completed => "completed",
        JobState::Cancelling => "cancelling",
        JobState::Cancelled => "cancelled",
        JobState::Failed => "failed",
        JobState::AwaitingApproval => "needs approval",
        JobState::Unknown => "unknown",
    }
}

/// Size and percent, only from a length the source stated.
pub fn amount(progress: &Progress) -> String {
    match progress.bytes_total {
        Some(total) if total > 0 => {
            let percent = (progress.bytes_received as f64 / total as f64 * 100.0).min(100.0);
            format!("{percent:.0}% of {}", bytes(total))
        }
        _ if progress.bytes_received > 0 => bytes(progress.bytes_received),
        _ => String::new(),
    }
}

/// The live progress line: percent, size, speed and time left, each only
/// when the engine knows it.
pub fn progress_line(progress: &Progress) -> String {
    let received = progress.bytes_received;
    let rate = progress
        .rate_bytes_per_second
        .map_or(String::new(), |rate| format!("  {}/s", bytes(rate)));
    let eta = progress
        .eta_seconds
        .map_or(String::new(), |eta| format!("  {} left", duration(eta)));
    match progress.bytes_total {
        Some(total) if total > 0 => {
            let percent = (received as f64 / total as f64 * 100.0).min(100.0);
            format!(
                "{percent:5.1}%  {} of {}{rate}{eta}",
                bytes(received),
                bytes(total)
            )
        }
        _ => format!("{} received{rate}", bytes(received)),
    }
}

pub use fetchpath_protocol::describe::bytes;

pub fn duration(seconds: u64) -> String {
    if seconds >= 3600 {
        format!(
            "{}:{:02}:{:02}",
            seconds / 3600,
            seconds / 60 % 60,
            seconds % 60
        )
    } else {
        format!("{}:{:02}", seconds / 60, seconds % 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fetchpath_protocol::Timestamp;

    fn job(id: &str) -> JobSnapshot {
        serde_json::from_value(serde_json::json!({
            "job_id": id,
            "kind": "file",
            "state": "queued",
            "job_revision": 1,
            "last_seq": 1,
            "source_display": "https://a.test/x",
            "progress": { "bytes_received": 0 },
            "created_at": Timestamp::now(),
        }))
        .unwrap()
    }

    #[test]
    fn a_job_is_found_by_queue_index_or_unique_id_prefix() {
        let jobs = [
            job("3f1c2a9b-0000-4000-8000-000000000001"),
            job("3f1d0000-0000-4000-8000-000000000002"),
            job("12345678-0000-4000-8000-000000000003"),
        ];
        let id = |reference: &str| resolve(&jobs, reference).map(|job| job.job_id.clone());
        assert_eq!(id("1").unwrap(), jobs[0].job_id);
        assert_eq!(id("3").unwrap(), jobs[2].job_id);
        assert_eq!(id("3F1C").unwrap(), jobs[0].job_id);
        // Up to four digits is an index; five reach the id.
        assert_eq!(id("12345").unwrap(), jobs[2].job_id);
        assert!(id("1234").is_err());
        assert!(id("0").is_err());
        assert!(id("4").is_err());
        assert!(id("3f1").unwrap_err().contains("More than one"));
        assert!(id("ffff9").unwrap_err().contains("No download"));
    }

    #[test]
    fn error_codes_decide_exit_codes() {
        let error = |code: &str| {
            ProtocolError::new(
                ErrorCode::try_from(code.to_owned()).unwrap(),
                ErrorScope::Job,
                "x",
            )
        };
        assert_eq!(exit_code(&error("input.invalid_url")), 2);
        assert_eq!(exit_code(&error("contract.unknown_job")), 2);
        assert_eq!(exit_code(&error("storage.destination_conflict")), 3);
        assert_eq!(exit_code(&error("source.transfer_failed")), 4);
        assert_eq!(exit_code(&error("integrity.checksum_mismatch")), 5);
        assert_eq!(exit_code(&error("storage.write_failed")), 6);
        assert_eq!(exit_code(&error("contract.engine_unavailable")), 1);
        assert_eq!(exit_code(&error("internal.unknown")), 1);
    }
}
