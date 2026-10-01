//! Following one job until nothing more happens without a person, for
//! `download`, `add --wait` and `watch JOB`.

use crate::client::{self, Engine};
use fetchpath_protocol::client::Subscription;
use fetchpath_protocol::command::Command;
use fetchpath_protocol::message::CommandResult;
use fetchpath_protocol::model::JobState;
use fetchpath_protocol::{EventStream, JobId, JobSnapshot, ProtocolError, StreamItem};
use std::io::{IsTerminal, Write};
use std::time::Duration;

/// What Ctrl+C does while waiting.
#[derive(Clone, Copy, Eq, PartialEq)]
pub enum OnInterrupt {
    /// Cancel the job and wait for the cancellation (`download`).
    Cancel,
    /// Stop waiting and leave the job to the engine.
    Leave,
}

pub struct Waited {
    pub job: JobSnapshot,
    /// Ctrl+C ended the wait before the job settled.
    pub left: bool,
}

/// The wait only needs these operations; keep reconnect and stream refresh
/// together so tests can prove quiet streams never cause a state query.
trait WaitEngine: Sized {
    fn send(&self, command: Command) -> Result<CommandResult, ProtocolError>;
    fn job(&self, id: &JobId) -> Result<JobSnapshot, ProtocolError>;
    fn subscribe(&self, command: Command) -> Result<Subscription, ProtocolError>;
    fn connect() -> Result<Self, ProtocolError>;
}

impl WaitEngine for Engine {
    fn send(&self, command: Command) -> Result<CommandResult, ProtocolError> {
        self.send(command)
    }
    fn job(&self, id: &JobId) -> Result<JobSnapshot, ProtocolError> {
        self.job(id)
    }
    fn subscribe(&self, command: Command) -> Result<Subscription, ProtocolError> {
        self.subscribe(command)
    }
    fn connect() -> Result<Self, ProtocolError> {
        Self::connect()
    }
}

/// Follows `job` until it is settled. With `live`, progress is drawn on
/// standard error while it is a terminal, and automatic retries are noted
/// there either way, so a script's log says why it is still waiting. If the engine goes
/// away, a new one is started and the wait continues where the job was
/// checkpointed.
pub fn follow(
    engine: &mut Engine,
    job: JobSnapshot,
    live: bool,
    on_interrupt: OnInterrupt,
) -> Result<Waited, ProtocolError> {
    follow_with(engine, job, live, on_interrupt)
}

fn follow_with<E: WaitEngine>(
    engine: &mut E,
    mut job: JobSnapshot,
    live: bool,
    on_interrupt: OnInterrupt,
) -> Result<Waited, ProtocolError> {
    let mut line = Line::new(live && std::io::stderr().is_terminal());
    let notes = live;
    let mut events = subscribe(engine, &mut job)?;
    let mut cancel_sent = false;
    let mut noted_retry = None;
    loop {
        if client::interrupted() {
            match on_interrupt {
                OnInterrupt::Leave => {
                    line.clear();
                    return Ok(Waited { job, left: true });
                }
                OnInterrupt::Cancel if !cancel_sent => {
                    engine.send(Command::Cancel {
                        job_id: job.job_id.clone(),
                        retain_partial: false,
                    })?;
                    cancel_sent = true;
                }
                OnInterrupt::Cancel => {}
            }
        }
        if client::settled(&job) {
            line.clear();
            return Ok(Waited { job, left: false });
        }
        let refresh = match events.next_item(Duration::from_millis(200)) {
            Ok(Some(StreamItem::Progress(sample))) => {
                if job.state == JobState::Running {
                    line.show(&client::progress_line(&sample.public_payload));
                }
                false
            }
            Ok(Some(StreamItem::Event(_))) => true,
            // Timeouts only let us observe Ctrl+C. Durable events drive
            // state refreshes; progress and quiet streams need no polling.
            Ok(None) => false,
            Err(_) => {
                // The engine stopped or restarted: reach the next one and
                // follow the job from what it has saved.
                *engine = E::connect()?;
                job = engine.job(&job.job_id)?;
                events = subscribe(engine, &mut job)?;
                true
            }
        };
        if refresh {
            job = match engine.job(&job.job_id) {
                Ok(current) => current,
                // Stopping or gone between two events: the same as a broken
                // stream, so the wait carries on with the next engine.
                Err(error) if engine_went_away(&error) => {
                    *engine = E::connect()?;
                    let mut current = engine.job(&job.job_id)?;
                    events = subscribe(engine, &mut current)?;
                    current
                }
                Err(error) => return Err(error),
            };
            match job.state {
                JobState::Running => line.show(&client::progress_line(&job.progress)),
                _ => line.show(&waiting_text(&job)),
            }
            if job.state == JobState::Failed && job.retry_at != noted_retry {
                noted_retry = job.retry_at;
                if let (Some(error), Some(at), true) = (&job.error, job.retry_at, notes) {
                    line.note(&format!(
                        "{} Trying again at {}.",
                        error.message,
                        crate::when::local(at)
                    ));
                }
            }
        }
    }
}

/// The engine stopped or the connection to it broke; a new one may be
/// reached (or refuses with a reason of its own, such as an update).
fn engine_went_away(error: &ProtocolError) -> bool {
    matches!(
        error.code.as_str(),
        "contract.engine_unavailable"
            | "contract.connection_lost"
            | "contract.connection_timed_out"
    )
}

fn subscribe(
    engine: &impl WaitEngine,
    job: &mut JobSnapshot,
) -> Result<Box<dyn EventStream>, ProtocolError> {
    let subscription = engine.subscribe(Command::SubscribeJob {
        job_id: job.job_id.clone(),
        after_seq: job.last_seq,
    })?;
    // A compacted stream starts after its snapshot. Apply that boundary
    // before waiting, including a completion that will not be replayed.
    if let CommandResult::SnapshotBoundary { jobs, .. } = subscription.start
        && let Some(current) = jobs
            .into_iter()
            .find(|current| current.job_id == job.job_id)
    {
        *job = current;
    }
    Ok(subscription.events)
}

fn waiting_text(job: &JobSnapshot) -> String {
    match (job.state, job.not_before, job.retry_at) {
        (JobState::Queued, Some(at), _) => format!("Scheduled for {}", crate::when::local(at)),
        (JobState::Queued, None, _) => "Waiting for a free download slot".into(),
        (JobState::Paused, ..) => "Paused. Resume it with `fetchpath resume`.".into(),
        (JobState::Failed, _, Some(at)) => format!("Retrying at {}", crate::when::local(at)),
        (state, ..) => {
            let label = client::state_name(state);
            let mut text = label.to_owned();
            if let Some(first) = text.get_mut(..1) {
                first.make_ascii_uppercase();
            }
            text
        }
    }
}

/// One line on standard error, redrawn in place.
struct Line {
    enabled: bool,
    width: usize,
}

impl Line {
    fn new(enabled: bool) -> Self {
        Self { enabled, width: 0 }
    }

    fn show(&mut self, text: &str) {
        if !self.enabled {
            return;
        }
        let length = text.chars().count();
        eprint!("\r{text}{}", " ".repeat(self.width.saturating_sub(length)));
        let _ = std::io::stderr().flush();
        self.width = length;
    }

    /// A message that stays, above the live line.
    fn note(&mut self, text: &str) {
        self.clear();
        eprintln!("{text}");
    }

    fn clear(&mut self) {
        if self.enabled && self.width > 0 {
            eprint!("\r{}\r", " ".repeat(self.width));
            let _ = std::io::stderr().flush();
            self.width = 0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fetchpath_protocol::message::{EventPayload, JobEvent, StreamPosition};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    fn running() -> JobSnapshot {
        let line = include_str!("../tests/fixtures/queue-stream.jsonl")
            .lines()
            .nth(1)
            .unwrap();
        let value: serde_json::Value = serde_json::from_str(line).unwrap();
        serde_json::from_value(value["public_payload"]["job"].clone()).unwrap()
    }

    struct Fake {
        job: JobSnapshot,
        boundary: bool,
        delivered: Arc<AtomicBool>,
        queries: Arc<AtomicUsize>,
    }

    impl WaitEngine for Fake {
        fn send(&self, _: Command) -> Result<CommandResult, ProtocolError> {
            panic!("unexpected command")
        }
        fn connect() -> Result<Self, ProtocolError> {
            panic!("unexpected reconnect")
        }
        fn job(&self, _: &JobId) -> Result<JobSnapshot, ProtocolError> {
            assert!(
                self.delivered.load(Ordering::SeqCst),
                "quiet streams must never poll the job"
            );
            self.queries.fetch_add(1, Ordering::SeqCst);
            Ok(self.job.clone())
        }
        fn subscribe(&self, _: Command) -> Result<Subscription, ProtocolError> {
            let position = StreamPosition::Job {
                job_id: self.job.job_id.clone(),
                after_seq: self.job.last_seq,
            };
            Ok(Subscription {
                start: if self.boundary {
                    CommandResult::SnapshotBoundary {
                        jobs: vec![self.job.clone()],
                        position,
                    }
                } else {
                    CommandResult::Subscribed { position }
                },
                events: Box::new(FakeStream {
                    job: self.job.clone(),
                    boundary: self.boundary,
                    quiet: true,
                    delivered: Arc::clone(&self.delivered),
                }),
            })
        }
    }

    struct FakeStream {
        job: JobSnapshot,
        boundary: bool,
        quiet: bool,
        delivered: Arc<AtomicBool>,
    }

    impl EventStream for FakeStream {
        fn next_item(&mut self, _: Duration) -> Result<Option<StreamItem>, ProtocolError> {
            assert!(
                !self.boundary,
                "a completed snapshot boundary needs no stream read"
            );
            if std::mem::take(&mut self.quiet) {
                // Exceed the old one-second poll deadline deterministically.
                std::thread::sleep(Duration::from_millis(1_050));
                return Ok(None);
            }
            assert!(!self.delivered.swap(true, Ordering::SeqCst));
            Ok(Some(StreamItem::Event(JobEvent {
                schema_version: fetchpath_protocol::SCHEMA_VERSION,
                job_id: self.job.job_id.clone(),
                seq: 2,
                cursor: 2,
                job_revision: 2,
                occurred_at: fetchpath_protocol::Timestamp::now(),
                payload: EventPayload::PublicationCompleted {
                    destination: self.job.destination.clone(),
                    observed_sha256: None,
                },
                correlation: Default::default(),
            })))
        }
    }

    #[test]
    fn completion_refreshes_from_an_event_without_idle_polling() {
        let initial = running();
        let mut completed = initial.clone();
        completed.state = JobState::Completed;
        let mut engine = Fake {
            job: completed.clone(),
            boundary: false,
            delivered: Arc::default(),
            queries: Arc::default(),
        };
        let waited = follow_with(&mut engine, initial, false, OnInterrupt::Leave).unwrap();
        assert_eq!(waited.job, completed);
        assert_eq!(engine.queries.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_completed_subscription_boundary_settles_without_a_poll_or_event() {
        let initial = running();
        let mut completed = initial.clone();
        completed.state = JobState::Completed;
        let mut engine = Fake {
            job: completed.clone(),
            boundary: true,
            delivered: Arc::default(),
            queries: Arc::default(),
        };
        let waited = follow_with(&mut engine, initial, false, OnInterrupt::Leave).unwrap();
        assert_eq!(waited.job, completed);
        assert_eq!(engine.queries.load(Ordering::SeqCst), 0);
    }
}
