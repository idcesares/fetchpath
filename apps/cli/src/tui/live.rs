//! The queue as the terminal shows it, kept current from the engine's
//! queue stream: which jobs belong in the live panel, and which have just
//! finished and print a receipt into scrollback.
//!
//! Events carry a per-job `seq`; a snapshot carries the `last_seq` it
//! reflects. Anything at or below it is already applied, so a snapshot
//! fetched at any moment can be merged with the stream without a gap or a
//! double step (job contract §8).

use crate::client;
use fetchpath_protocol::message::{EventPayload, JobEvent, ProgressSample};
use fetchpath_protocol::model::JobState;
use fetchpath_protocol::{JobId, JobSnapshot};
use std::collections::HashSet;

/// Something worth a line in scrollback.
#[derive(Clone, Debug, PartialEq)]
pub enum Notice {
    /// A job has finished: saved, cancelled, or failed with no retry due.
    Receipt(Box<JobSnapshot>),
    /// A durable event on a job that has not finished, as `watch` words it.
    /// A loud one (a warning or a recorded problem) is worth printing even
    /// where the panel shows the job.
    Event {
        name: String,
        text: String,
        loud: bool,
    },
}

/// A job in the live panel.
#[derive(Clone, Copy, Debug)]
pub struct Row<'a> {
    /// Its number in `/queue`, from 1.
    pub index: usize,
    /// See [`group`].
    pub group: u8,
    pub job: &'a JobSnapshot,
}

#[derive(Default)]
pub struct Live {
    /// Every job in the engine's order, newest first, which is the order
    /// queue indexes count in.
    jobs: Vec<JobSnapshot>,
    /// Jobs an event said failed, until a snapshot says whether a retry is
    /// due: the event alone cannot tell a final failure from a retrying one.
    settling: HashSet<JobId>,
}

/// Where a job sits in the panel, most urgent first; `None` when it has
/// finished and left the panel.
pub fn group(job: &JobSnapshot) -> Option<u8> {
    Some(match job.state {
        JobState::Completed | JobState::Cancelled => return None,
        JobState::Failed if job.retry_at.is_none() => return None,
        JobState::Probing
        | JobState::Ready
        | JobState::Running
        | JobState::Verifying
        | JobState::Publishing
        | JobState::Pausing
        | JobState::Cancelling => 0,
        JobState::AwaitingApproval
        | JobState::WaitingForSelection
        | JobState::WaitingForSource
        | JobState::Failed => 1,
        JobState::Paused => 2,
        JobState::Queued if job.not_before.is_none() => 3,
        JobState::Queued => 4,
        JobState::Unknown => 5,
    })
}

impl Live {
    pub fn new(jobs: Vec<JobSnapshot>) -> Self {
        Self {
            jobs,
            settling: HashSet::new(),
        }
    }

    /// The panel group, holding a settling failure among those needing a
    /// person until its snapshot arrives.
    pub fn group(&self, job: &JobSnapshot) -> Option<u8> {
        group(job).or_else(|| self.settling.contains(&job.job_id).then_some(1))
    }

    fn finished(&self, job: &JobSnapshot) -> bool {
        self.group(job).is_none()
    }

    pub fn jobs(&self) -> &[JobSnapshot] {
        &self.jobs
    }

    fn position(&self, job_id: &JobId) -> Option<usize> {
        self.jobs.iter().position(|job| &job.job_id == job_id)
    }

    /// The jobs the panel lists with their queue index: active first, then
    /// those needing a person, paused, queued and scheduled, each group in
    /// queue order (the next to run first).
    pub fn panel(&self) -> Vec<Row<'_>> {
        let mut shown: Vec<(u8, usize, &JobSnapshot)> = self
            .jobs
            .iter()
            .enumerate()
            .filter_map(|(position, job)| self.group(job).map(|group| (group, position, job)))
            .collect();
        shown.sort_by_key(|(group, position, _)| (*group, std::cmp::Reverse(*position)));
        shown
            .into_iter()
            .map(|(group, position, job)| Row {
                index: position + 1,
                group,
                job,
            })
            .collect()
    }

    /// Replaces a job with a fresher snapshot, or adds it at the front. A
    /// snapshot older than what is held is ignored.
    pub fn refresh(&mut self, job: JobSnapshot) -> Vec<Notice> {
        let position = self.position(&job.job_id);
        let was_finished = match position {
            Some(position) if job.last_seq < self.jobs[position].last_seq => return Vec::new(),
            Some(position) => self.finished(&self.jobs[position]),
            None => false,
        };
        self.settling.remove(&job.job_id);
        let position = match position {
            Some(position) => {
                self.jobs[position] = job;
                position
            }
            None => {
                self.jobs.insert(0, job);
                0
            }
        };
        self.receipt(was_finished, position)
    }

    pub fn remove(&mut self, job_id: &JobId) {
        self.jobs.retain(|job| &job.job_id != job_id);
        self.settling.remove(job_id);
    }

    fn receipt(&self, was_finished: bool, position: usize) -> Vec<Notice> {
        let job = &self.jobs[position];
        if !was_finished && self.finished(job) {
            vec![Notice::Receipt(Box::new(job.clone()))]
        } else {
            Vec::new()
        }
    }

    /// Applies one durable event. Returns whether it was new (a caller then
    /// fetches the job for fields no event carries, such as a retry time)
    /// and what it is worth saying.
    ///
    /// A failure prints its receipt only from a snapshot, which says whether
    /// a retry is due; until then the job waits in the panel.
    pub fn apply(&mut self, event: &JobEvent) -> (bool, Vec<Notice>) {
        if let EventPayload::JobCreated { job } = &event.payload {
            let mut job = job.clone();
            job.last_seq = job.last_seq.max(event.seq);
            let new = self.position(&job.job_id).is_none();
            let notices = self.refresh(job);
            return (new, notices);
        }
        let Some(position) = self.position(&event.job_id) else {
            return (false, Vec::new());
        };
        if event.seq <= self.jobs[position].last_seq {
            return (false, Vec::new());
        }
        if matches!(event.payload, EventPayload::JobRemoved) {
            let job_id = event.job_id.clone();
            self.remove(&job_id);
            return (true, Vec::new());
        }
        let was_finished = self.finished(&self.jobs[position]);
        let job = &mut self.jobs[position];
        job.last_seq = event.seq;
        job.job_revision = job.job_revision.max(event.job_revision);
        match &event.payload {
            EventPayload::StateChanged { state, .. } => {
                job.state = *state;
                job.retry_at = None;
                if *state == JobState::Failed {
                    self.settling.insert(job.job_id.clone());
                }
            }
            EventPayload::PolicyChanged { not_before } => job.not_before = *not_before,
            EventPayload::SourceChanged { source_display } => {
                job.source_display = source_display.clone();
            }
            EventPayload::ErrorRecorded { error } => job.error = Some(error.clone()),
            EventPayload::PublicationCompleted {
                destination,
                observed_sha256,
            } => {
                if destination.is_some() {
                    job.destination = destination.clone();
                }
                if observed_sha256.is_some() {
                    job.observed_sha256 = observed_sha256.clone();
                }
            }
            _ => {}
        }
        let mut notices = self.receipt(was_finished, position);
        let job = &self.jobs[position];
        if notices.is_empty()
            && !self.finished(job)
            && !self.settling.contains(&job.job_id)
            && let Some(text) = crate::queue::event_text(&event.payload)
        {
            notices.push(Notice::Event {
                name: client::name(job),
                text,
                loud: matches!(
                    event.payload,
                    EventPayload::Warning { .. } | EventPayload::ErrorRecorded { .. }
                ),
            });
        }
        (true, notices)
    }

    /// Applies a progress sample to a job still moving. Samples are
    /// ephemeral and may arrive out of step with events; the next snapshot
    /// corrects anything they got wrong.
    pub fn progress(&mut self, sample: &ProgressSample) {
        if let Some(position) = self.position(&sample.job_id)
            && !self.finished(&self.jobs[position])
        {
            self.jobs[position].progress = sample.public_payload.clone();
        }
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use fetchpath_protocol::message::ServerMessage;

    /// A queue stream recorded from a real engine with `fetchpath watch
    /// --json`: two files, one saved and one failing for good.
    pub const RECORDED: &str = include_str!("../../tests/fixtures/queue-stream.jsonl");

    pub fn recorded() -> Vec<ServerMessage> {
        RECORDED
            .lines()
            .filter(|line| !line.trim().is_empty())
            .filter_map(|line| serde_json::from_str::<ServerMessage>(line).ok())
            .collect()
    }

    #[test]
    fn a_recorded_stream_prints_one_receipt_per_finished_job() {
        let mut live = Live::default();
        let mut receipts = Vec::new();
        for message in recorded() {
            match message {
                ServerMessage::Event(event) => {
                    let (_, notices) = live.apply(&event);
                    receipts.extend(notices.into_iter().filter_map(|notice| match notice {
                        Notice::Receipt(job) => Some(job),
                        Notice::Event { .. } => None,
                    }));
                }
                ServerMessage::Progress(sample) => live.progress(&sample),
                ServerMessage::Reply(_) => {}
            }
        }
        // The saved file finishes from events alone; a failure waits for a
        // snapshot to say no retry is due.
        assert_eq!(receipts.len(), 1);
        assert_eq!(receipts[0].state, JobState::Completed);
        assert!(
            live.panel()
                .iter()
                .all(|row| row.job.state == JobState::Failed)
        );

        let mut failed = live.panel()[0].job.clone();
        failed.retry_at = None;
        let notices = live.refresh(failed);
        assert!(matches!(&notices[..], [Notice::Receipt(job)] if job.state == JobState::Failed));
        assert!(live.panel().is_empty());
        // A second look at the same finished job says nothing more.
        let again = live.jobs()[0].clone();
        assert!(live.refresh(again).is_empty());
    }

    #[test]
    fn events_already_in_a_snapshot_are_not_applied_twice() {
        let messages = recorded();
        let mut live = Live::default();
        let mut applied = Vec::new();
        for message in &messages {
            if let ServerMessage::Event(event) = message {
                let (new, _) = live.apply(event);
                applied.push(new);
            }
        }
        // Replaying the whole stream over its own result changes nothing.
        let before: Vec<JobSnapshot> = live.jobs().to_vec();
        for message in &messages {
            if let ServerMessage::Event(event) = message {
                let (new, notices) = live.apply(event);
                assert!(!new || matches!(event.payload, EventPayload::JobCreated { .. }));
                assert!(notices.is_empty());
            }
        }
        assert_eq!(live.jobs(), &before[..]);
        assert!(applied.iter().all(|new| *new));
    }

    #[test]
    fn the_panel_puts_active_jobs_first_and_scheduled_last() {
        let job = |id: &str, state: &str, not_before: bool| {
            let mut value = serde_json::json!({
                "job_id": id, "kind": "file", "state": state, "job_revision": 1,
                "last_seq": 1, "source_display": format!("https://a.test/{id}"),
                "progress": { "bytes_received": 0 },
                "created_at": "2026-09-26T10:00:00Z",
            });
            if not_before {
                value["not_before"] = "2026-09-26T18:00:00Z".into();
            }
            serde_json::from_value::<JobSnapshot>(value).unwrap()
        };
        // Newest first, as the engine lists them.
        let live = Live::new(vec![
            job("00000006-0000-4000-8000-000000000000", "queued", true),
            job("00000005-0000-4000-8000-000000000000", "completed", false),
            job("00000004-0000-4000-8000-000000000000", "queued", false),
            job("00000003-0000-4000-8000-000000000000", "paused", false),
            job(
                "00000002-0000-4000-8000-000000000000",
                "awaiting_approval",
                false,
            ),
            job("00000001-0000-4000-8000-000000000000", "running", false),
        ]);
        let order: Vec<usize> = live.panel().iter().map(|row| row.index).collect();
        assert_eq!(order, [6, 5, 4, 3, 1]);
    }
}
