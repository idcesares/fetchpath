//! Focused prompts beyond the link card (FP-061): a batch of links
//! previewed together, a download stopped by an existing file, and an
//! agent's request waiting for the person. The link card itself takes a
//! checksum and another name; the helpers for both are here. Each prompt
//! can be left with Esc and has a plain-mode form.

pub mod approval;
pub mod batch;
pub mod conflict;

use super::line::Out;
use super::live::Live;
use super::view::{Glyphs, OverlayText};
use crate::client::Engine;
use approval::Approval;
use conflict::Conflict;
use crossterm::event::KeyEvent;
use fetchpath_protocol::model::JobState;
use fetchpath_protocol::{JobId, JobSnapshot};
use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};

/// A SHA-256 as people paste it (either case, a `sha256:` prefix, spaces
/// around it), as the engine stores it: 64 lowercase hex digits.
pub fn checksum(text: &str) -> Result<String, String> {
    fetchpath_core::normalize_sha256(text).ok_or_else(|| {
        let digits = text.chars().filter(char::is_ascii_hexdigit).count();
        format!("That is not a SHA-256: it needs 64 hexadecimal digits ({digits} found).")
    })
}

/// A typed file name as it will be saved, or why it cannot be one.
pub fn file_name(text: &str) -> Result<String, String> {
    let text = text.trim();
    if text.contains(['/', '\\']) {
        return Err("Type a file name only; the folder stays as shown.".to_owned());
    }
    crate::download::safe_file_name(text).ok_or_else(|| "That cannot be a file name.".to_owned())
}

/// Whether something is already at `path`, or another download in the same
/// batch will be saved there.
pub fn occupied(path: &Path, taken: &[PathBuf]) -> bool {
    path.exists() || taken.iter().any(|other| same_path(other, path))
}

/// Windows paths compare without regard to case.
fn same_path(a: &Path, b: &Path) -> bool {
    a.as_os_str()
        .to_string_lossy()
        .eq_ignore_ascii_case(&b.as_os_str().to_string_lossy())
}

/// `path` when it is free, else the first free "name (N).ext" beside it.
/// Nothing is ever replaced.
pub fn beside(path: &Path, taken: &[PathBuf]) -> PathBuf {
    if !occupied(path, taken) {
        return path.to_path_buf();
    }
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let (stem, extension) = match name.rsplit_once('.') {
        Some((stem, extension)) if !stem.is_empty() => (stem.to_owned(), format!(".{extension}")),
        _ => (name.clone(), String::new()),
    };
    (1..)
        .map(|n| path.with_file_name(format!("{stem} ({n}){extension}")))
        .find(|candidate| !occupied(candidate, taken))
        .expect("some number is free")
}

/// A download that stopped because a file is already at its destination,
/// with no retry due.
pub fn conflicted(job: &JobSnapshot) -> bool {
    job.state == JobState::Failed
        && job.retry_at.is_none()
        && job
            .error
            .as_ref()
            .is_some_and(|error| error.code.as_str().ends_with("destination_conflict"))
}

/// The prompts that come from the queue rather than from what was typed:
/// downloads stopped by an existing file, and agents' requests.
#[derive(Default)]
pub struct Flows {
    conflicts: VecDeque<Conflict>,
    approval: Option<Approval>,
    /// Requests put off with Esc; `/approvals` asks about them again.
    later: HashSet<JobId>,
    /// Downloads already stopped by an existing file when last looked, so
    /// each asks once; those stopped before the terminal opened do not ask
    /// (`/rename` or `/queue` still reach them).
    stopped: Option<HashSet<JobId>>,
}

impl Flows {
    pub fn active(&self) -> bool {
        !self.conflicts.is_empty() || self.approval.is_some()
    }

    pub fn ask_rename(&mut self, job: &JobSnapshot) {
        if !self
            .conflicts
            .iter()
            .any(|held| held.job.job_id == job.job_id)
            && let Some(conflict) = Conflict::new(job.clone())
        {
            self.conflicts.push_back(conflict);
        }
    }

    /// Picks up downloads newly stopped by an existing file and requests
    /// waiting for the person, and drops a request answered elsewhere (in
    /// the desktop, or with `/approve`).
    pub fn watch(&mut self, live: &Live) {
        let now: HashSet<JobId> = live
            .jobs()
            .iter()
            .filter(|job| conflicted(job))
            .map(|job| job.job_id.clone())
            .collect();
        if let Some(before) = self.stopped.take() {
            for job in live.jobs().iter().rev() {
                if now.contains(&job.job_id) && !before.contains(&job.job_id) {
                    self.ask_rename(job);
                }
            }
        }
        self.stopped = Some(now);
        let waiting = |id: &JobId| {
            live.jobs()
                .iter()
                .find(|job| &job.job_id == id)
                .filter(|job| job.state == JobState::AwaitingApproval)
                .cloned()
        };
        match self.approval.as_ref().map(|held| waiting(&held.job.job_id)) {
            Some(Some(job)) => self.approval.as_mut().expect("held").job = job,
            Some(None) => self.approval = None,
            None => {}
        }
        if self.approval.is_none() {
            self.approval = live
                .jobs()
                .iter()
                .rev()
                .find(|job| {
                    job.state == JobState::AwaitingApproval && !self.later.contains(&job.job_id)
                })
                .map(|job| Approval::new(job.clone()));
        }
    }

    /// `/approvals`: ask again about requests put off.
    pub fn ask_again(&mut self) {
        self.later.clear();
    }

    /// Handles a key for the prompt showing; returns lines for scrollback.
    pub fn key(&mut self, engine: &Engine, key: KeyEvent) -> Vec<Out> {
        if let Some(conflict) = self.conflicts.front_mut() {
            let (lines, done) = conflict.key(engine, key);
            if done {
                self.conflicts.pop_front();
            }
            return lines;
        }
        if let Some(approval) = &self.approval {
            let (lines, done) = approval.key(engine, key);
            if done {
                if lines.is_empty() {
                    self.later.insert(approval.job.job_id.clone());
                }
                self.approval = None;
            }
            return lines;
        }
        Vec::new()
    }

    pub fn text(&self, glyphs: &Glyphs, width: usize) -> Option<OverlayText> {
        if let Some(conflict) = self.conflicts.front() {
            return Some(conflict.text(glyphs, width));
        }
        self.approval
            .as_ref()
            .map(|approval| approval.text(glyphs, width))
    }

    /// Plain mode: the lines asking the current question, if it has not
    /// been asked yet.
    pub fn plain_question(&mut self) -> Option<Vec<String>> {
        if let Some(conflict) = self.conflicts.front_mut() {
            return (!std::mem::replace(&mut conflict.asked, true)).then(|| conflict.plain_lines());
        }
        let approval = self.approval.as_mut()?;
        (!std::mem::replace(&mut approval.asked, true)).then(|| approval.plain_lines())
    }

    /// Plain mode: a typed answer to the current question, if one is
    /// showing. `None` when nothing is asked.
    pub fn plain_answer(&mut self, engine: &Engine, text: &str) -> Option<Vec<Out>> {
        if let Some(conflict) = self.conflicts.front_mut() {
            let (lines, done) = conflict.plain_answer(engine, text);
            if done {
                self.conflicts.pop_front();
            }
            return Some(lines);
        }
        let approval = self.approval.as_ref()?;
        let (lines, done) = approval.plain_answer(engine, text);
        if done {
            if lines.is_empty() {
                self.later.insert(approval.job.job_id.clone());
            }
            self.approval = None;
        }
        Some(lines)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pasted_checksum_is_normalized_or_refused_with_a_count() {
        let hex = "AB".repeat(32);
        assert_eq!(checksum(&format!("  sha256:{hex} ")), Ok("ab".repeat(32)));
        assert_eq!(
            checksum("abc123"),
            Err("That is not a SHA-256: it needs 64 hexadecimal digits (6 found).".to_owned())
        );
    }

    #[test]
    fn a_typed_name_stays_in_its_folder_and_is_made_safe() {
        assert_eq!(file_name(" report?.pdf "), Ok("report_.pdf".to_owned()));
        assert!(file_name("..\\up.txt").is_err());
        assert!(file_name("CON").is_err());
    }

    #[test]
    fn an_existing_or_taken_name_gets_the_next_free_number() {
        let dir = tempfile::tempdir().unwrap();
        let wanted = dir.path().join("report.pdf");
        assert_eq!(beside(&wanted, &[]), wanted);
        std::fs::write(&wanted, b"x").unwrap();
        std::fs::write(dir.path().join("report (1).pdf"), b"x").unwrap();
        let taken = [dir.path().join("REPORT (2).PDF")];
        assert_eq!(beside(&wanted, &taken), dir.path().join("report (3).pdf"));
        let bare = dir.path().join("README");
        std::fs::write(&bare, b"x").unwrap();
        assert_eq!(beside(&bare, &[]), dir.path().join("README (1)"));
    }
}
