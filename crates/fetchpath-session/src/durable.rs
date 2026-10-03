//! The durable side of the engine (FP-051): the command ledger, the retained
//! event log and the per-job revision and sequence counters.
//!
//! The ledger and events are saved in `engine-v1.json`, beside the queue.
//! Every entry and event carries the generation of the commit that added it,
//! and the queue file records the generation it committed. A commit that adds
//! engine data writes the engine file first and the queue file second, so on
//! load anything newer than the queue's generation belongs to a change that
//! never committed and is discarded. A command's change, its ledger entry and
//! its events therefore commit together or not at all, while a save that
//! adds no events rewrites only the small queue file, as before.

use crate::engine::Subscriber;
use crate::{QueueRecord, wire};
use fetchpath_protocol::message::{Correlation, EventPayload, JobEvent};
use fetchpath_protocol::model::WaitingReason;
use fetchpath_protocol::{ClientId, CommandId, CommandResult, JobId, SCHEMA_VERSION};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

/// Durable events kept for replay. Older ones are compacted away and a
/// subscriber asking for them gets a snapshot boundary instead.
pub(crate) const MAX_EVENTS: usize = 512;
/// A command older than this is refused as expired.
pub(crate) const MAX_COMMAND_AGE_MS: i64 = 10 * 60 * 1_000;
/// A command stamped this far in the future is refused as clock skew.
pub(crate) const MAX_FUTURE_SKEW_MS: i64 = 2 * 60 * 1_000;
/// Ledger entries are kept at least this long after receipt, so no command
/// can still be accepted once its entry could have been purged (contract §5).
pub(crate) const LEDGER_RETENTION_MS: i64 = MAX_COMMAND_AGE_MS + MAX_FUTURE_SKEW_MS;

/// The engine file's contents.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DurableEngine {
    pub schema_version: u32,
    /// The last generation written to the engine file.
    #[serde(default)]
    pub generation: u64,
    /// The last queue-wide event cursor handed out.
    #[serde(default)]
    pub cursor: u64,
    #[serde(default)]
    pub events: VecDeque<StoredEvent>,
    #[serde(default)]
    pub ledger: Vec<LedgerEntry>,
}

impl DurableEngine {
    /// Keeps only what the queue file committed: entries and events of a
    /// later generation belong to a change whose queue write never happened.
    ///
    /// Numbers never go backwards, though. The queue may be older than the
    /// engine file, after a crash between the two writes or when a damaged
    /// queue file fell back to its backup, and subscribers may already have
    /// seen the newer cursors and sequences. Whenever the engine file saw
    /// more than the queue committed, numbering resumes one past the highest
    /// number seen, and the gap sends a resubscribing client to a snapshot
    /// boundary instead of letting a reused number hide a change.
    pub fn committed(mut self, generation: u64, cursor: u64) -> (Self, bool, Seen) {
        let mut seen = Seen {
            cursor: self.cursor.max(
                self.events
                    .iter()
                    .map(|stored| stored.event.cursor)
                    .max()
                    .unwrap_or(0),
            ),
            jobs: HashMap::new(),
        };
        for stored in &self.events {
            let entry = seen
                .jobs
                .entry(stored.event.job_id.as_str().to_owned())
                .or_insert((0, 0));
            entry.0 = entry.0.max(stored.event.seq);
            entry.1 = entry.1.max(stored.event.job_revision);
        }
        let before = (self.events.len(), self.ledger.len());
        self.events.retain(|stored| stored.generation <= generation);
        self.ledger.retain(|entry| entry.generation <= generation);
        let discarded = before != (self.events.len(), self.ledger.len());
        self.generation = generation;
        self.cursor = if seen.cursor > cursor {
            seen.cursor + 1
        } else {
            cursor
        };
        (self, discarded, seen)
    }
}

/// The highest numbers the engine file had handed out: the queue cursor, and
/// each job's sequence and revision.
pub(crate) struct Seen {
    pub cursor: u64,
    pub jobs: HashMap<String, (u64, u64)>,
}

impl Seen {
    /// Moves a restored record past any sequence or revision the engine file
    /// saw for it, leaving a gap so its old numbers are never reused.
    pub fn advance(&self, record: &mut QueueRecord) {
        if let Some(&(seq, revision)) = self.jobs.get(&record.id)
            && seq > record.durable.last_seq
        {
            record.durable.last_seq = seq + 1;
            record.durable.job_revision = record.durable.job_revision.max(revision) + 1;
        }
    }
}

/// A durable event and the commit generation that added it.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct StoredEvent {
    pub generation: u64,
    pub event: JobEvent,
}

/// One committed command and its immutable result.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LedgerEntry {
    pub client_id: ClientId,
    pub command_id: CommandId,
    /// SHA-256 of the payload and expected revision, so a reused id with a
    /// different request is told apart from a resend.
    pub fingerprint: String,
    pub received_at_ms: u64,
    /// The commit generation that added this entry.
    #[serde(default)]
    pub generation: u64,
    pub result: CommandResult,
}

/// What a job looked like in its last durable event.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Reported {
    pub state: String,
    #[serde(default)]
    pub not_before_ms: Option<u64>,
    /// Waiting for room above the disk reserve (FP-101).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub waiting_for_space: bool,
}

/// Saved with each record; omitted while unused, so a 0.1.0 record reads and
/// writes back unchanged.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RecordDurable {
    #[serde(default, skip_serializing_if = "is_zero")]
    pub job_revision: u64,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub last_seq: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reported: Option<Reported>,
}

fn is_zero(value: &u64) -> bool {
    *value == 0
}

pub(crate) struct RemovedJob {
    pub job_id: String,
    pub job_revision: u64,
    pub last_seq: u64,
}

/// The command whose change is being committed, and the jobs it touched.
pub(crate) struct Correlating {
    pub command_id: CommandId,
    pub jobs: HashSet<String>,
}

/// In-memory engine state, locked after the queue lock and never before it.
#[derive(Default)]
pub(crate) struct Durable {
    pub engine: DurableEngine,
    /// While a ledgered command runs, saves only mark the state dirty; the
    /// command's own commit then writes everything at once.
    pub defer: bool,
    pub dirty: bool,
    pub removed: Vec<RemovedJob>,
    pub correlation: Option<Correlating>,
    pub subscribers: Vec<Arc<Subscriber>>,
    /// Entries or events were added since the engine file was last written.
    pub engine_changed: bool,
    /// The generation the queue file last committed. Ledger entries and
    /// events above it are not durable yet.
    pub committed: u64,
    /// Committed in memory but not yet written; handed to subscribers only
    /// once a write succeeds.
    pub unbroadcast: Vec<JobEvent>,
}

impl Durable {
    fn next_cursor(&mut self) -> u64 {
        self.engine.cursor += 1;
        self.engine.cursor
    }

    fn correlate(&self, job_id: &str) -> Correlation {
        Correlation {
            command_id: self
                .correlation
                .as_ref()
                .filter(|correlating| correlating.jobs.contains(job_id))
                .map(|correlating| correlating.command_id.clone()),
            attempt_id: None,
        }
    }

    /// The generation the next write of the engine file will carry.
    pub fn pending_generation(&self) -> u64 {
        self.engine.generation + 1
    }

    fn push(&mut self, event: JobEvent) {
        self.engine.events.push_back(StoredEvent {
            generation: self.pending_generation(),
            event: event.clone(),
        });
        while self.engine.events.len() > MAX_EVENTS {
            self.engine.events.pop_front();
        }
        self.engine_changed = true;
        self.unbroadcast.push(event);
    }

    pub fn record(&mut self, mut entry: LedgerEntry) {
        entry.generation = self.pending_generation();
        self.engine.ledger.push(entry);
        self.engine_changed = true;
    }

    /// The retained events that are on disk, oldest first.
    pub fn committed_events(&self) -> impl Iterator<Item = &JobEvent> {
        let committed = self.committed;
        self.engine
            .events
            .iter()
            .filter(move |stored| stored.generation <= committed)
            .map(|stored| &stored.event)
    }

    /// Events derived but not yet on disk, oldest first.
    pub fn uncommitted_events(&self) -> impl Iterator<Item = &JobEvent> {
        let committed = self.committed;
        self.engine
            .events
            .iter()
            .filter(move |stored| stored.generation > committed)
            .map(|stored| &stored.event)
    }

    /// Compares every record with its last durable report and records one
    /// event per change. Every path that changes a job (commands, the poll,
    /// automatic retry, restart recovery) ends here.
    pub fn derive_events(&mut self, records: &mut [QueueRecord], now_ms: u64) {
        let occurred_at = wire::timestamp(now_ms);
        for removed in std::mem::take(&mut self.removed) {
            let Ok(job_id) = JobId::try_from(removed.job_id.as_str()) else {
                continue;
            };
            let event = JobEvent {
                schema_version: SCHEMA_VERSION,
                correlation: self.correlate(&removed.job_id),
                job_id,
                seq: removed.last_seq + 1,
                cursor: self.next_cursor(),
                job_revision: removed.job_revision + 1,
                occurred_at,
                payload: EventPayload::JobRemoved,
            };
            self.push(event);
        }
        for record in records.iter_mut() {
            let current = Reported {
                state: record.view.state.clone(),
                not_before_ms: record.not_before_ms,
                waiting_for_space: record.view.waiting_for_space,
            };
            let mut payloads = Vec::new();
            match record.durable.reported.as_ref() {
                None => payloads.push(None),
                Some(previous) if previous == &current => {}
                Some(previous) => {
                    let (before, after) = (
                        wire::job_state(&previous.state),
                        wire::job_state(&current.state),
                    );
                    if before != after {
                        payloads.push(Some(EventPayload::StateChanged {
                            previous: before,
                            state: after,
                            waiting_reason: wire::waiting_reason(&record.view),
                        }));
                        if let Some(error) = wire::protocol_error(&record.view)
                            .filter(|_| record.view.state == "failed")
                        {
                            payloads.push(Some(EventPayload::ErrorRecorded { error }));
                        }
                        if record.view.state == "completed" {
                            payloads.push(Some(EventPayload::PublicationCompleted {
                                destination: record.view.destination.clone(),
                                observed_sha256: record.view.observed_sha256.clone(),
                            }));
                        }
                    }
                    // A queued download starts waiting for disk space: the
                    // contract's `waiting` event, since its state is still
                    // queued.
                    if current.waiting_for_space && !previous.waiting_for_space {
                        payloads.push(Some(EventPayload::Waiting {
                            reason: WaitingReason::StorageReserve,
                        }));
                    }
                    if previous.not_before_ms != current.not_before_ms {
                        payloads.push(Some(EventPayload::PolicyChanged {
                            not_before: current.not_before_ms.map(wire::timestamp),
                        }));
                    }
                }
            }
            if payloads.is_empty() {
                // Only the session's own spelling changed (such as scheduled
                // to queued with no new time); nothing a client sees.
                record.durable.reported = Some(current);
                continue;
            }
            let Ok(job_id) = JobId::try_from(record.id.as_str()) else {
                record.durable.reported = Some(current);
                continue;
            };
            for payload in payloads {
                record.durable.last_seq += 1;
                record.durable.job_revision += 1;
                let payload = payload.unwrap_or_else(|| EventPayload::JobCreated {
                    job: wire::snapshot(record),
                });
                let event = JobEvent {
                    schema_version: SCHEMA_VERSION,
                    job_id: job_id.clone(),
                    seq: record.durable.last_seq,
                    cursor: self.next_cursor(),
                    job_revision: record.durable.job_revision,
                    occurred_at,
                    payload,
                    correlation: self.correlate(&record.id),
                };
                self.push(event);
            }
            record.durable.reported = Some(current);
        }
    }

    /// Drops ledger entries past their retention.
    pub fn prune_ledger(&mut self, now_ms: u64) {
        let now = now_ms as i64;
        self.engine
            .ledger
            .retain(|entry| now - (entry.received_at_ms as i64) <= LEDGER_RETENTION_MS);
    }

    /// Hands committed events to subscribers after a successful write.
    pub fn broadcast(&mut self) {
        let events = std::mem::take(&mut self.unbroadcast);
        if events.is_empty() {
            return;
        }
        self.subscribers
            .retain(|subscriber| !subscriber.is_closed());
        for subscriber in &self.subscribers {
            for event in &events {
                subscriber.offer_event(event);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fetchpath_protocol::Timestamp;

    fn event(cursor: u64, seq: u64) -> JobEvent {
        JobEvent {
            schema_version: SCHEMA_VERSION,
            job_id: JobId::try_from("018f9c2a-525c-7b9a-986c-b0707def18bb").unwrap(),
            seq,
            cursor,
            job_revision: seq,
            occurred_at: Timestamp::from_unix_ms(0),
            payload: EventPayload::JobRemoved,
            correlation: Correlation::default(),
        }
    }

    #[test]
    fn only_written_events_count_as_committed() {
        let mut durable = Durable::default();
        durable.engine.generation = 1;
        durable.committed = 1;
        durable.engine.events.push_back(StoredEvent {
            generation: 1,
            event: event(1, 1),
        });
        durable.push(event(2, 2));
        let committed: Vec<u64> = durable
            .committed_events()
            .map(|event| event.cursor)
            .collect();
        let pending: Vec<u64> = durable
            .uncommitted_events()
            .map(|event| event.cursor)
            .collect();
        assert_eq!(
            committed,
            vec![1],
            "an event not yet written must not be replayed"
        );
        assert_eq!(pending, vec![2]);
        durable.committed = 2;
        assert_eq!(durable.committed_events().count(), 2);
    }

    #[test]
    fn loading_an_older_queue_skips_past_every_number_already_used() {
        let mut engine = DurableEngine {
            generation: 3,
            cursor: 9,
            ..DurableEngine::default()
        };
        engine.events.push_back(StoredEvent {
            generation: 2,
            event: event(8, 4),
        });
        engine.events.push_back(StoredEvent {
            generation: 3,
            event: event(9, 5),
        });
        // The queue committed generation 2, cursor 8.
        let (kept, discarded, seen) = engine.committed(2, 8);
        assert!(discarded);
        assert_eq!(kept.events.len(), 1);
        assert_eq!(
            kept.cursor, 10,
            "one past the highest cursor used, leaving a gap"
        );
        assert_eq!(seen.jobs["018f9c2a-525c-7b9a-986c-b0707def18bb"], (5, 5));
        // A queue that committed everything resumes exactly where it was.
        let mut current = DurableEngine {
            generation: 3,
            cursor: 9,
            ..DurableEngine::default()
        };
        current.events.push_back(StoredEvent {
            generation: 3,
            event: event(9, 5),
        });
        let (kept, discarded, _) = current.committed(3, 9);
        assert!(!discarded);
        assert_eq!(kept.cursor, 9);
    }
}
