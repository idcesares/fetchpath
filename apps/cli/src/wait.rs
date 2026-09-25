//! Following one job until nothing more happens without a person, for
//! `download`, `add --wait` and `watch JOB`.

use crate::client::{self, Engine};
use fetchpath_protocol::command::Command;
use fetchpath_protocol::model::JobState;
use fetchpath_protocol::{EventStream, JobSnapshot, ProtocolError, StreamItem};
use std::io::{IsTerminal, Write};
use std::time::{Duration, Instant};

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

/// Follows `job` until it is settled. With `live`, progress is drawn on
/// standard error while it is a terminal, and automatic retries are noted
/// there either way, so a script's log says why it is still waiting. If the engine goes
/// away, a new one is started and the wait continues where the job was
/// checkpointed.
pub fn follow(
    engine: &mut Engine,
    mut job: JobSnapshot,
    live: bool,
    on_interrupt: OnInterrupt,
) -> Result<Waited, ProtocolError> {
    let mut line = Line::new(live && std::io::stderr().is_terminal());
    let notes = live;
    let mut events = subscribe(engine, &job)?;
    let mut cancel_sent = false;
    let mut checked = Instant::now();
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
                checked.elapsed() >= Duration::from_secs(1)
            }
            Ok(Some(StreamItem::Event(_))) => true,
            Ok(None) => checked.elapsed() >= Duration::from_secs(1),
            Err(_) => {
                // The engine stopped or restarted: reach the next one and
                // follow the job from what it has saved.
                *engine = Engine::connect()?;
                job = engine.job(&job.job_id)?;
                events = subscribe(engine, &job)?;
                true
            }
        };
        if refresh {
            job = engine.job(&job.job_id)?;
            checked = Instant::now();
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

fn subscribe(engine: &Engine, job: &JobSnapshot) -> Result<Box<dyn EventStream>, ProtocolError> {
    Ok(engine
        .subscribe(Command::SubscribeJob {
            job_id: job.job_id.clone(),
            after_seq: job.last_seq,
        })?
        .events)
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
