//! Protocol v1 on the session (FP-051): commands through a durable ledger,
//! optimistic revisions, replayable event streams and coalesced progress.
//!
//! The commit path is the one place persistence ordering matters:
//!
//! 1. Under the queue lock, look the command up in the ledger (contract §5:
//!    identity before time), then check its age, its target and its
//!    `expected_revision`, and start deferring saves.
//! 2. Run the change through the session's own methods. Their saves, and any
//!    other thread's, only mark the state dirty meanwhile.
//! 3. Under the queue lock again, derive the change's events, build the
//!    result, add the ledger entry, and write everything in one save. Only
//!    then is the result returned.
//!
//! A crash before step 3's write leaves no trace of the command, so a resend
//! applies it once; a crash after it returns the stored result on resend.

use crate::durable::{Correlating, LedgerEntry, MAX_COMMAND_AGE_MS, MAX_FUTURE_SKEW_MS};
use crate::{JobDraft, MediaDraft, Session, error_code, now_ms, wire};
use fetchpath_protocol::client::{EngineClient, EventStream, StreamItem, Subscription};
use fetchpath_protocol::command::{
    Command, CommandEnvelope, ConflictPolicy, DestinationDecision, JobFilter, JobInput, JobRequest,
    Schedule,
};
use fetchpath_protocol::error::{Action, ErrorCode, ErrorScope, ProtocolError};
use fetchpath_protocol::message::{
    CommandResult, ControlOutcome, JobEvent, ProgressKind, ProgressSample, StreamPosition,
};
use fetchpath_protocol::model::{
    self, EngineStatus, JobDetails, MediaInspection, MediaVariant, MediaVariantKind, QueueStats,
    Segment,
};
use fetchpath_protocol::{JobId, SCHEMA_VERSION, Timestamp};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Durable events a subscriber may fall behind by before it is closed and
/// must resubscribe from its last position.
pub(crate) const MAX_PENDING_EVENTS: usize = 1_024;
/// How often the in-process client reconciles and samples progress.
pub const TICK_INTERVAL: Duration = Duration::from_millis(250);

fn error(code: &'static str, scope: ErrorScope, message: impl Into<String>) -> ProtocolError {
    ProtocolError::new(
        ErrorCode::try_from(code.to_owned()).expect("a valid built-in code"),
        scope,
        message,
    )
}

fn unknown_job() -> ProtocolError {
    error(
        "contract.unknown_job",
        ErrorScope::Command,
        "This download is no longer available.",
    )
    .with_action(Action::RefreshClient)
}

fn transition(message: String) -> ProtocolError {
    error("contract.invalid_transition", ErrorScope::Command, message)
        .with_action(Action::RefreshClient)
}

fn input(message: String) -> ProtocolError {
    error("input.invalid_request", ErrorScope::Command, message).with_action(Action::CorrectInput)
}

fn unsupported(what: &str) -> ProtocolError {
    error(
        "contract.unsupported",
        ErrorScope::Command,
        format!("{what} is not supported by this engine yet."),
    )
    .with_action(Action::UpdateSoftware)
}

fn persistence(message: String) -> ProtocolError {
    let mut failure = error("internal.persistence_failed", ErrorScope::Engine, message);
    // The change is kept in memory with its ledger entry; resending the same
    // envelope returns its result once the queue can be saved again.
    failure.retryable = true;
    failure.action = Some(Action::Retry);
    failure
}

/// SHA-256 of what the command asks for, so a resend matches and a reused id
/// with a different request does not.
fn fingerprint(envelope: &CommandEnvelope) -> String {
    let canonical = serde_json::to_vec(&(&envelope.payload, envelope.expected_revision))
        .expect("a command always serializes");
    format!("{:x}", Sha256::digest(canonical))
}

fn target(command: &Command) -> Option<&JobId> {
    match command {
        Command::Start { job_id }
        | Command::Pause { job_id }
        | Command::Resume { job_id }
        | Command::Cancel { job_id, .. }
        | Command::Retry { job_id }
        | Command::UpdatePolicy { job_id, .. }
        | Command::ResolveDestination { job_id, .. }
        | Command::SelectMedia { job_id, .. }
        | Command::RefreshSource { job_id, .. }
        | Command::RefreshMediaChoices { job_id }
        | Command::RemoveJob { job_id } => Some(job_id),
        _ => None,
    }
}

fn millis(timestamp: Timestamp) -> Result<u64, ProtocolError> {
    u64::try_from(timestamp.unix_ms())
        .map_err(|_| input("A schedule before 1970 cannot be used.".into()))
}

/// What a change did, turned into a result once its events are derived.
enum Outcome {
    Job(String),
    Control(ControlOutcome, String),
    Removed(String),
    Settings,
    ShuttingDown,
}

impl Outcome {
    fn jobs(&self) -> HashSet<String> {
        match self {
            Self::Job(id) | Self::Control(_, id) | Self::Removed(id) => HashSet::from([id.clone()]),
            Self::Settings | Self::ShuttingDown => HashSet::new(),
        }
    }
}

pub struct Engine {
    session: Arc<Session>,
    /// Ledgered commands run one at a time.
    commands: Mutex<()>,
    started_at: Timestamp,
    sample_cursor: AtomicU64,
}

impl Engine {
    pub fn new(session: Arc<Session>) -> Arc<Self> {
        Arc::new(Self {
            session,
            commands: Mutex::new(()),
            started_at: Timestamp::now(),
            sample_cursor: AtomicU64::new(0),
        })
    }

    pub fn session(&self) -> &Arc<Session> {
        &self.session
    }

    /// Runs one command. Queries answer from the current state; changes go
    /// through the ledger and are acknowledged only after they commit.
    pub fn execute(&self, envelope: &CommandEnvelope) -> Result<CommandResult, ProtocolError> {
        if envelope.schema_version != SCHEMA_VERSION {
            return Err(ProtocolError::unsupported_version(envelope.schema_version));
        }
        if matches!(
            envelope.payload,
            Command::SubscribeJob { .. } | Command::SubscribeQueue { .. }
        ) {
            return Err(ProtocolError::malformed(
                "a subscription is opened with subscribe, not execute",
            ));
        }
        if !envelope.payload.is_mutating() {
            return self.query(&envelope.payload);
        }
        let _serial = self
            .commands
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let fingerprint = fingerprint(envelope);

        // Step 1: identity, time, target and revision, then defer saves.
        {
            let state = self.session.inner.lock().expect("desktop jobs poisoned");
            let mut durable = self.session.durable.lock().expect("engine state poisoned");
            if let Some(entry) = durable.engine.ledger.iter().find(|entry| {
                entry.client_id == envelope.client_id && entry.command_id == envelope.command_id
            }) {
                return if entry.fingerprint == fingerprint {
                    Ok(entry.result.clone())
                } else {
                    Err(error(
                        "contract.idempotency_conflict",
                        ErrorScope::Command,
                        "This command id was already used for a different request.",
                    )
                    .with_action(Action::RefreshClient))
                };
            }
            let age = now_ms() as i64 - envelope.issued_at.unix_ms();
            if age > MAX_COMMAND_AGE_MS {
                return Err(error(
                    "contract.command_expired",
                    ErrorScope::Command,
                    "This command is too old to be carried out. Send it again.",
                )
                .with_action(Action::RefreshClient));
            }
            if -age > MAX_FUTURE_SKEW_MS {
                return Err(error(
                    "contract.clock_skew",
                    ErrorScope::Command,
                    "This command is dated in the future. Check the computer's clock.",
                ));
            }
            if let Some(job_id) = target(&envelope.payload) {
                let record = state
                    .records
                    .iter()
                    .find(|record| record.id == job_id.as_str())
                    .ok_or_else(unknown_job)?;
                if let Some(expected) = envelope.expected_revision
                    && expected != record.durable.job_revision
                {
                    let mut conflict = error(
                        "contract.revision_conflict",
                        ErrorScope::Command,
                        "This download changed since it was last shown. Refresh and try again.",
                    )
                    .with_action(Action::RefreshClient);
                    conflict.current_revision = Some(record.durable.job_revision);
                    return Err(conflict);
                }
            }
            durable.defer = true;
        }

        // Step 2: the change.
        let applied = self.apply(&envelope.payload);

        // Step 3: one commit for the change, its events and its ledger entry.
        let mut state = self.session.inner.lock().expect("desktop jobs poisoned");
        let mut durable = self.session.durable.lock().expect("engine state poisoned");
        durable.defer = false;
        let outcome = match applied {
            Ok(outcome) => outcome,
            Err(failure) => {
                // A refused command changes nothing it owns, but saves held
                // back meanwhile still have to reach the disk.
                if std::mem::take(&mut durable.dirty) {
                    durable.derive_events(&mut state.records, now_ms());
                    let _ = self.session.write_locked(&state, &mut durable);
                }
                return Err(failure);
            }
        };
        durable.correlation = Some(Correlating {
            command_id: envelope.command_id.clone(),
            jobs: outcome.jobs(),
        });
        durable.derive_events(&mut state.records, now_ms());
        durable.correlation = None;
        let result = match &outcome {
            Outcome::Job(id) => CommandResult::Job {
                job: state
                    .records
                    .iter()
                    .find(|record| &record.id == id)
                    .map(wire::snapshot)
                    .ok_or_else(unknown_job)?,
            },
            Outcome::Control(control, id) => CommandResult::Control {
                outcome: *control,
                job: state
                    .records
                    .iter()
                    .find(|record| &record.id == id)
                    .map(wire::snapshot)
                    .ok_or_else(unknown_job)?,
            },
            Outcome::Removed(id) => CommandResult::Removed {
                job_id: JobId::try_from(id.as_str()).map_err(|_| unknown_job())?,
            },
            Outcome::Settings => CommandResult::Settings {
                view: wire::settings_view(
                    &self.session.settings(),
                    self.session.settings_repaired(),
                ),
            },
            Outcome::ShuttingDown => CommandResult::ShuttingDown,
        };
        durable.record(LedgerEntry {
            client_id: envelope.client_id.clone(),
            command_id: envelope.command_id.clone(),
            fingerprint,
            received_at_ms: now_ms(),
            generation: 0,
            result: result.clone(),
        });
        durable.prune_ledger(now_ms());
        durable.dirty = false;
        self.session
            .write_locked(&state, &mut durable)
            .map_err(persistence)?;
        Ok(result)
    }

    fn state_of(&self, job_id: &str) -> Option<String> {
        let state = self.session.inner.lock().expect("desktop jobs poisoned");
        state
            .records
            .iter()
            .find(|record| record.id == job_id)
            .map(|record| record.view.state.clone())
    }

    fn apply(&self, command: &Command) -> Result<Outcome, ProtocolError> {
        let session = &self.session;
        match command {
            Command::CreateJob { request } => match request {
                JobRequest::File {
                    input: source,
                    destination,
                    not_before,
                    expected_sha256,
                } => {
                    let JobInput::Url { url } = source else {
                        return Err(unsupported("Creating a job from a stored request"));
                    };
                    if destination.conflict != ConflictPolicy::Ask {
                        return Err(unsupported("Replacing an existing file"));
                    }
                    let created = session
                        .enqueue(vec![JobDraft {
                            url: url.expose().to_owned(),
                            destination: destination.path.clone(),
                            not_before_ms: not_before.map(millis).transpose()?,
                            checksum: expected_sha256.clone(),
                        }])
                        .map_err(input)?;
                    let job = created
                        .into_iter()
                        .next()
                        .ok_or_else(|| input("The download was not queued.".into()))?;
                    Ok(Outcome::Job(job.job_id))
                }
                JobRequest::Media {
                    input: source,
                    destination,
                    not_before,
                    variant_id,
                    quality_label,
                } => {
                    let JobInput::Url { url } = source else {
                        return Err(unsupported("Creating a job from a stored request"));
                    };
                    if destination.conflict != ConflictPolicy::Ask {
                        return Err(unsupported("Replacing an existing file"));
                    }
                    let job = session
                        .enqueue_media(MediaDraft {
                            url: url.expose().to_owned(),
                            variant_id: variant_id.clone(),
                            quality_label: quality_label.clone(),
                            destination: destination.path.clone(),
                            not_before_ms: not_before.map(millis).transpose()?,
                        })
                        .map_err(input)?;
                    Ok(Outcome::Job(job.job_id))
                }
            },
            Command::Start { job_id } => session
                .start_now(job_id.as_str())
                .map(|job| Outcome::Job(job.job_id))
                .map_err(transition),
            Command::Pause { job_id } => {
                let id = job_id.as_str();
                match self.state_of(id).as_deref() {
                    None => Err(unknown_job()),
                    Some("completed" | "cancelled") => Ok(Outcome::Control(
                        ControlOutcome::AlreadyTerminal,
                        id.to_owned(),
                    )),
                    Some("paused" | "failed") => {
                        Ok(Outcome::Control(ControlOutcome::NoOp, id.to_owned()))
                    }
                    Some("cancelling") => Ok(Outcome::Control(
                        ControlOutcome::CancelInProgress,
                        id.to_owned(),
                    )),
                    Some(_) => {
                        let view = session.pause(id).map_err(transition)?;
                        let control = if view.state == "completed" {
                            ControlOutcome::TooLate
                        } else {
                            ControlOutcome::Accepted
                        };
                        Ok(Outcome::Control(control, id.to_owned()))
                    }
                }
            }
            Command::Resume { job_id } => session
                .resume(job_id.as_str())
                .map(|job| Outcome::Job(job.job_id))
                .map_err(transition),
            Command::Cancel {
                job_id,
                retain_partial,
            } => {
                if *retain_partial {
                    return Err(unsupported("Keeping a partial file on cancel"));
                }
                self.cancel(job_id.as_str())
            }
            Command::Retry { job_id } => session
                .retry(job_id.as_str(), None, None, None)
                .map(|job| Outcome::Job(job.job_id))
                .map_err(transition),
            Command::UpdatePolicy { job_id, patch } => match &patch.schedule {
                None => Ok(Outcome::Job(job_id.as_str().to_owned())),
                Some(Schedule::Now) => session
                    .start_now(job_id.as_str())
                    .map(|job| Outcome::Job(job.job_id))
                    .map_err(transition),
                Some(Schedule::At { not_before }) => session
                    .reschedule(job_id.as_str(), millis(*not_before)?)
                    .map(|job| Outcome::Job(job.job_id))
                    .map_err(transition),
            },
            Command::ResolveDestination { job_id, decision } => match decision {
                DestinationDecision::ChooseNewPath { path } => session
                    .retry(job_id.as_str(), None, Some(path.clone()), None)
                    .map(|job| Outcome::Job(job.job_id))
                    .map_err(input),
                DestinationDecision::Cancel => self.cancel(job_id.as_str()),
                DestinationDecision::ReplaceExisting => {
                    Err(unsupported("Replacing an existing file"))
                }
            },
            Command::SelectMedia { .. } => {
                Err(unsupported("Choosing a media format after creation"))
            }
            Command::RefreshMediaChoices { .. } => Err(unsupported("Refreshing media choices")),
            Command::RefreshSource { job_id, source } => {
                let JobInput::Url { url } = source else {
                    return Err(unsupported("Refreshing from a stored request"));
                };
                session
                    .retry(job_id.as_str(), Some(url.expose().to_owned()), None, None)
                    .map(|job| Outcome::Job(job.job_id))
                    .map_err(input)
            }
            Command::RemoveJob { job_id } => session
                .remove(job_id.as_str())
                .map(|()| Outcome::Removed(job_id.as_str().to_owned()))
                .map_err(transition),
            Command::UpdateSettings { settings } => {
                let next = wire::session_settings(settings, &session.settings());
                session.update_settings(next).map_err(input)?;
                Ok(Outcome::Settings)
            }
            Command::EngineShutdown => {
                session.cancel_all_and_join();
                Ok(Outcome::ShuttingDown)
            }
            _ => Err(ProtocolError::malformed("not a change")),
        }
    }

    fn cancel(&self, job_id: &str) -> Result<Outcome, ProtocolError> {
        let response = self.session.cancel(job_id).map_err(transition)?;
        let control = match response.outcome {
            "accepted" => ControlOutcome::Accepted,
            "too_late" => ControlOutcome::TooLateToCancel,
            "already_terminal" => ControlOutcome::AlreadyTerminal,
            _ => ControlOutcome::Unknown,
        };
        Ok(Outcome::Control(control, job_id.to_owned()))
    }

    /// Reconciles, commits and returns every job's snapshot, newest first.
    fn snapshots(&self) -> Result<Vec<model::JobSnapshot>, ProtocolError> {
        self.session.list().map_err(persistence)?;
        let state = self.session.inner.lock().expect("desktop jobs poisoned");
        Ok(state.records.iter().rev().map(wire::snapshot).collect())
    }

    fn query(&self, command: &Command) -> Result<CommandResult, ProtocolError> {
        use fetchpath_protocol::model::JobState as S;
        match command {
            Command::ListJobs { filter } => {
                let jobs = self
                    .snapshots()?
                    .into_iter()
                    .filter(|job| match filter {
                        JobFilter::All => true,
                        JobFilter::Active => !job.state.is_terminal() && job.state != S::Failed,
                        JobFilter::Failed => job.state == S::Failed,
                        JobFilter::Finished => job.state.is_terminal(),
                    })
                    .collect();
                Ok(CommandResult::Jobs { jobs })
            }
            Command::GetJob { job_id } => self
                .snapshots()?
                .into_iter()
                .find(|job| &job.job_id == job_id)
                .map(|job| CommandResult::Job { job })
                .ok_or_else(unknown_job),
            Command::JobDetails { job_id } => {
                let details = self
                    .session
                    .details(job_id.as_str())
                    .map_err(|_| unknown_job())?;
                let state = self.session.inner.lock().expect("desktop jobs poisoned");
                let job = state
                    .records
                    .iter()
                    .find(|record| record.id == job_id.as_str())
                    .map(wire::snapshot)
                    .ok_or_else(unknown_job)?;
                Ok(CommandResult::Details {
                    details: JobDetails {
                        job,
                        segments: details
                            .segments
                            .into_iter()
                            .map(|segment| Segment {
                                start: segment.start,
                                end: segment.end,
                                received: segment.received,
                            })
                            .collect(),
                    },
                })
            }
            Command::InspectMedia { url } => {
                let inspection = self
                    .session
                    .inspect_media(url.expose())
                    .map_err(|message| {
                        let code = error_code(&message);
                        let code = if code == "internal.unknown" {
                            "input.invalid_request".to_owned()
                        } else {
                            code
                        };
                        ProtocolError::new(
                            ErrorCode::try_from(code).unwrap_or(ErrorCode::INTERNAL_UNKNOWN),
                            ErrorScope::Command,
                            message,
                        )
                    })?;
                Ok(CommandResult::MediaInspection {
                    inspection: MediaInspection {
                        title: inspection.title,
                        duration_seconds: inspection.duration_seconds,
                        variants: inspection
                            .variants
                            .into_iter()
                            .map(|variant| MediaVariant {
                                id: variant.id,
                                label: variant.label,
                                kind: match variant.kind {
                                    fetchpath_media::MediaKind::Video => MediaVariantKind::Video,
                                    fetchpath_media::MediaKind::Audio => MediaVariantKind::Audio,
                                },
                                extension: variant.extension,
                                height: variant.height,
                                fps: variant.fps,
                            })
                            .collect(),
                    },
                })
            }
            Command::QueueStats => {
                let stats = self.session.stats();
                Ok(CommandResult::QueueStats {
                    stats: QueueStats {
                        running: stats.running as u64,
                        queued: stats.queued as u64,
                        scheduled: stats.scheduled as u64,
                        paused: stats.paused as u64,
                        completed: stats.completed as u64,
                        failed: stats.failed as u64,
                        active_bytes: stats.active_bytes,
                        completed_bytes: stats.completed_bytes,
                        combined_bytes_per_second: stats.combined_bytes_per_second,
                        max_active_downloads: stats.max_active_downloads as u64,
                    },
                })
            }
            Command::History { query, limit } => {
                let needle = query.as_deref().map(str::to_lowercase);
                let limit = limit.unwrap_or(100).clamp(1, 1_000) as usize;
                let jobs = self
                    .snapshots()?
                    .into_iter()
                    .filter(|job| job.state.is_terminal() || job.state == S::Failed)
                    .filter(|job| {
                        needle.as_deref().is_none_or(|needle| {
                            job.source_display.to_lowercase().contains(needle)
                                || job
                                    .destination
                                    .as_deref()
                                    .is_some_and(|path| path.to_lowercase().contains(needle))
                        })
                    })
                    .take(limit)
                    .collect();
                Ok(CommandResult::Jobs { jobs })
            }
            Command::GetSettings => Ok(CommandResult::Settings {
                view: wire::settings_view(
                    &self.session.settings(),
                    self.session.settings_repaired(),
                ),
            }),
            Command::EngineStatus => {
                let jobs = self.snapshots()?;
                let durable = self.session.durable.lock().expect("engine state poisoned");
                Ok(CommandResult::EngineStatus {
                    status: EngineStatus {
                        engine_version: env!("CARGO_PKG_VERSION").to_owned(),
                        schema_version: SCHEMA_VERSION,
                        started_at: self.started_at,
                        connected_clients: durable
                            .subscribers
                            .iter()
                            .filter(|subscriber| !subscriber.is_closed())
                            .count() as u32,
                        active_jobs: jobs
                            .iter()
                            .filter(|job| !job.state.is_terminal() && job.state != S::Failed)
                            .count() as u32,
                        queue_cursor: durable.engine.cursor,
                    },
                })
            }
            _ => Err(ProtocolError::malformed("not a query")),
        }
    }

    /// Opens an event stream. The replay, or the snapshot boundary when the
    /// events were compacted, is computed and the subscriber registered under
    /// the queue lock, so no event can fall between them (contract §8).
    pub fn subscribe(&self, envelope: &CommandEnvelope) -> Result<Subscription, ProtocolError> {
        if envelope.schema_version != SCHEMA_VERSION {
            return Err(ProtocolError::unsupported_version(envelope.schema_version));
        }
        // Commit anything pending so positions describe the current state.
        self.session.list().map_err(persistence)?;
        let state = self.session.inner.lock().expect("desktop jobs poisoned");
        let mut durable = self.session.durable.lock().expect("engine state poisoned");
        let (subscriber, start, replay) = match &envelope.payload {
            Command::SubscribeJob { job_id, after_seq } => {
                let retained: Vec<&JobEvent> = durable
                    .events()
                    .filter(|event| &event.job_id == job_id && event.seq > *after_seq)
                    .collect();
                let contiguous = retained
                    .first()
                    .is_some_and(|event| event.seq == after_seq + 1);
                let record = state
                    .records
                    .iter()
                    .find(|record| record.id == job_id.as_str());
                let last = record
                    .map(|record| record.durable.last_seq)
                    .or_else(|| retained.last().map(|event| event.seq));
                let Some(last) = last else {
                    return Err(unknown_job());
                };
                let subscriber = Arc::new(Subscriber::new(Some(job_id.as_str().to_owned())));
                if *after_seq == last || (*after_seq < last && contiguous) {
                    let replay: Vec<JobEvent> = retained.into_iter().cloned().collect();
                    let start = CommandResult::Subscribed {
                        position: StreamPosition::Job {
                            job_id: job_id.clone(),
                            after_seq: *after_seq,
                        },
                    };
                    (subscriber, start, replay)
                } else {
                    let start = CommandResult::SnapshotBoundary {
                        jobs: record.map(wire::snapshot).into_iter().collect(),
                        position: StreamPosition::Job {
                            job_id: job_id.clone(),
                            after_seq: last,
                        },
                    };
                    (subscriber, start, Vec::new())
                }
            }
            Command::SubscribeQueue { after_cursor } => {
                let last = durable.engine.cursor;
                let retained: Vec<JobEvent> = durable
                    .events()
                    .filter(|event| event.cursor > *after_cursor)
                    .cloned()
                    .collect();
                let contiguous = retained
                    .first()
                    .is_some_and(|event| event.cursor == after_cursor + 1);
                let subscriber = Arc::new(Subscriber::new(None));
                if *after_cursor == last || (*after_cursor < last && contiguous) {
                    let start = CommandResult::Subscribed {
                        position: StreamPosition::Queue {
                            after_cursor: *after_cursor,
                        },
                    };
                    (subscriber, start, retained)
                } else {
                    let start = CommandResult::SnapshotBoundary {
                        jobs: state.records.iter().rev().map(wire::snapshot).collect(),
                        position: StreamPosition::Queue { after_cursor: last },
                    };
                    (subscriber, start, Vec::new())
                }
            }
            _ => {
                return Err(ProtocolError::malformed(
                    "subscribe takes SubscribeJob or SubscribeQueue",
                ));
            }
        };
        for event in &replay {
            subscriber.offer_event(event);
        }
        durable.subscribers.push(Arc::clone(&subscriber));
        drop(durable);
        drop(state);
        Ok(Subscription {
            start,
            events: Box::new(SubscriberStream { subscriber }),
        })
    }

    /// Reconciles the queue (starting, finishing and retrying jobs), commits
    /// the resulting events, and offers a progress sample for every running
    /// job to every subscriber. Never waits on a subscriber.
    pub fn tick(&self) {
        let _ = self.session.list();
        let state = self.session.inner.lock().expect("desktop jobs poisoned");
        let mut durable = self.session.durable.lock().expect("engine state poisoned");
        durable
            .subscribers
            .retain(|subscriber| !subscriber.is_closed());
        if durable.subscribers.is_empty() {
            return;
        }
        let occurred_at = Timestamp::now();
        for record in state
            .records
            .iter()
            .filter(|record| record.view.state == "running")
        {
            let Ok(job_id) = JobId::try_from(record.id.as_str()) else {
                continue;
            };
            let sample = ProgressSample {
                schema_version: SCHEMA_VERSION,
                job_id,
                sample_cursor: self.sample_cursor.fetch_add(1, Ordering::SeqCst) + 1,
                job_revision: record.durable.job_revision,
                occurred_at,
                kind: ProgressKind::ProgressSampled,
                public_payload: wire::progress(&record.view),
                correlation: Default::default(),
            };
            for subscriber in &durable.subscribers {
                subscriber.offer_progress(&sample);
            }
        }
    }
}

/// One subscriber's pending items. Durable events queue up to a bound;
/// progress keeps only the latest sample per job.
pub(crate) struct Subscriber {
    job: Option<String>,
    queue: Mutex<Pending>,
    ready: Condvar,
}

#[derive(Default)]
struct Pending {
    events: VecDeque<JobEvent>,
    progress: HashMap<String, ProgressSample>,
    progress_order: VecDeque<String>,
    closed: Option<ProtocolError>,
    finished: bool,
}

impl Subscriber {
    fn new(job: Option<String>) -> Self {
        Self {
            job,
            queue: Mutex::new(Pending::default()),
            ready: Condvar::new(),
        }
    }

    fn wants(&self, job_id: &JobId) -> bool {
        self.job.as_deref().is_none_or(|job| job == job_id.as_str())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Pending> {
        self.queue
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub(crate) fn is_closed(&self) -> bool {
        let pending = self.lock();
        pending.finished || pending.closed.is_some()
    }

    pub(crate) fn offer_event(&self, event: &JobEvent) {
        if !self.wants(&event.job_id) {
            return;
        }
        let mut pending = self.lock();
        if pending.finished || pending.closed.is_some() {
            return;
        }
        if pending.events.len() >= MAX_PENDING_EVENTS {
            // Too far behind to keep: close it so it resubscribes from its
            // last position, and free what it held.
            pending.events.clear();
            pending.progress.clear();
            pending.progress_order.clear();
            pending.closed = Some(
                error(
                    "resource.subscriber_lagging",
                    ErrorScope::Connection,
                    "This event stream fell too far behind. Subscribe again from the last event seen.",
                )
                .with_action(Action::RefreshClient),
            );
        } else {
            pending.events.push_back(event.clone());
        }
        drop(pending);
        self.ready.notify_all();
    }

    fn offer_progress(&self, sample: &ProgressSample) {
        if !self.wants(&sample.job_id) {
            return;
        }
        let mut pending = self.lock();
        if pending.finished || pending.closed.is_some() {
            return;
        }
        let key = sample.job_id.as_str().to_owned();
        if pending
            .progress
            .insert(key.clone(), sample.clone())
            .is_none()
        {
            pending.progress_order.push_back(key);
        }
        drop(pending);
        self.ready.notify_all();
    }
}

struct SubscriberStream {
    subscriber: Arc<Subscriber>,
}

impl EventStream for SubscriberStream {
    fn next_item(&mut self, timeout: Duration) -> Result<Option<StreamItem>, ProtocolError> {
        let deadline = Instant::now() + timeout;
        let mut pending = self.subscriber.lock();
        loop {
            // Durable events first, in order; then the latest sample per job.
            if let Some(event) = pending.events.pop_front() {
                return Ok(Some(StreamItem::Event(event)));
            }
            if let Some(key) = pending.progress_order.pop_front()
                && let Some(sample) = pending.progress.remove(&key)
            {
                return Ok(Some(StreamItem::Progress(sample)));
            }
            if let Some(failure) = pending.closed.clone() {
                return Err(failure);
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Ok(None);
            }
            pending = self
                .subscriber
                .ready
                .wait_timeout(pending, left)
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .0;
        }
    }
}

impl Drop for SubscriberStream {
    fn drop(&mut self) {
        self.subscriber.lock().finished = true;
    }
}

/// `EngineClient` for a session in the same process: for tests, and for
/// clients developed before the pipe (FP-052) carries them to the engine.
pub struct InProcessClient {
    engine: Arc<Engine>,
    stop: Arc<AtomicBool>,
    ticker: Option<JoinHandle<()>>,
}

impl InProcessClient {
    /// A client whose engine reconciles and samples progress every
    /// [`TICK_INTERVAL`].
    pub fn new(engine: Arc<Engine>) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let ticker = {
            let engine = Arc::clone(&engine);
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || {
                while !stop.load(Ordering::SeqCst) {
                    engine.tick();
                    std::thread::sleep(TICK_INTERVAL);
                }
            })
        };
        Self {
            engine,
            stop,
            ticker: Some(ticker),
        }
    }

    /// A client with no background ticking; the caller ticks.
    pub fn manual(engine: Arc<Engine>) -> Self {
        Self {
            engine,
            stop: Arc::new(AtomicBool::new(true)),
            ticker: None,
        }
    }

    pub fn engine(&self) -> &Arc<Engine> {
        &self.engine
    }
}

impl Drop for InProcessClient {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(ticker) = self.ticker.take() {
            let _ = ticker.join();
        }
    }
}

impl EngineClient for InProcessClient {
    fn execute(&self, envelope: &CommandEnvelope) -> Result<CommandResult, ProtocolError> {
        self.engine.execute(envelope)
    }

    fn subscribe(&self, envelope: &CommandEnvelope) -> Result<Subscription, ProtocolError> {
        self.engine.subscribe(envelope)
    }
}
