//! A download that stopped because a file is already where it was going.
//! Fetchpath never replaces the file: the person keeps both (the download
//! is saved as the first free "name (N)"), types another name, or leaves
//! the download stopped.

use super::super::line::{Out, Tone};
use super::super::prompt::{Action, Prompt};
use super::super::view::{self, CardText, DIM, Glyphs, OverlayText};
use crate::client::{self, Engine};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use fetchpath_protocol::JobSnapshot;
use fetchpath_protocol::command::{Command, DestinationDecision};
use ratatui::style::{Color, Style};
use ratatui::text::Line;
use std::path::{Path, PathBuf};

pub struct Conflict {
    pub job: JobSnapshot,
    /// Where the download was going.
    path: PathBuf,
    editing: Option<Prompt>,
    problem: Option<String>,
    /// Plain mode has printed the question.
    pub asked: bool,
}

/// Saves a stopped download under `name` in the same folder, refusing a
/// name that is also taken. Returns what to say.
pub fn rename(engine: &Engine, job: &JobSnapshot, name: &str) -> Result<String, String> {
    let destination = job
        .destination
        .as_deref()
        .ok_or("This download has no destination to rename.")?;
    let name = super::file_name(name)?;
    let path = Path::new(destination).with_file_name(&name);
    if path.exists() {
        return Err(format!(
            "{name} is also in that folder; choose another name."
        ));
    }
    resolve(engine, job, &path)
}

fn resolve(engine: &Engine, job: &JobSnapshot, path: &Path) -> Result<String, String> {
    engine
        .send(Command::ResolveDestination {
            job_id: job.job_id.clone(),
            decision: DestinationDecision::ChooseNewPath {
                path: path.display().to_string(),
                expected_sha256: None,
            },
        })
        .map_err(|error| error.message)?;
    Ok(format!(
        "Saving {} {} as {}.",
        client::short_id(job),
        client::name(job),
        path.file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default()
    ))
}

impl Conflict {
    /// A prompt for a download stopped by an existing file; `None` for any
    /// other download.
    pub fn new(job: JobSnapshot) -> Option<Self> {
        if !super::conflicted(&job) {
            return None;
        }
        let path = PathBuf::from(job.destination.as_deref()?);
        Some(Self {
            job,
            path,
            editing: None,
            problem: None,
            asked: false,
        })
    }

    /// The free name keeping both would use.
    fn both(&self) -> PathBuf {
        super::beside(&self.path, &[])
    }

    fn name_of(path: &Path) -> String {
        path.file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default()
    }

    fn keep_both(&self, engine: &Engine) -> Out {
        match resolve(engine, &self.job, &self.both()) {
            Ok(said) => Out::new(Tone::Normal, said),
            Err(message) => Out::new(Tone::Bad, message),
        }
    }

    fn left(&self) -> Out {
        Out::new(
            Tone::Dim,
            format!(
                "{} stays stopped; /rename {} NAME saves it under another name.",
                client::name(&self.job),
                client::short_id(&self.job)
            ),
        )
    }

    /// Returns lines for scrollback and whether the prompt is finished.
    pub fn key(&mut self, engine: &Engine, key: KeyEvent) -> (Vec<Out>, bool) {
        if key.kind == KeyEventKind::Release {
            return (Vec::new(), false);
        }
        if let Some(prompt) = &mut self.editing {
            if key.code == KeyCode::Esc {
                self.editing = None;
                self.problem = None;
                return (Vec::new(), false);
            }
            if let Action::Submit(text) = prompt.handle(key) {
                match rename(engine, &self.job, &text) {
                    Ok(said) => return (vec![Out::new(Tone::Normal, said)], true),
                    Err(problem) => self.problem = Some(problem),
                }
            }
            return (Vec::new(), false);
        }
        match key.code {
            KeyCode::Enter => (vec![self.keep_both(engine)], true),
            KeyCode::Char('n' | 'N') => {
                let mut prompt = Prompt::default();
                prompt.set(Self::name_of(&self.both()));
                self.editing = Some(prompt);
                (Vec::new(), false)
            }
            KeyCode::Esc => (vec![self.left()], true),
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                (vec![self.left()], true)
            }
            _ => (Vec::new(), false),
        }
    }

    fn facts(&self) -> (String, String, String) {
        let folder = self
            .path
            .parent()
            .map(|folder| folder.display().to_string())
            .unwrap_or_default();
        (
            Self::name_of(&self.path),
            folder,
            Self::name_of(&self.both()),
        )
    }

    pub fn text(&self, glyphs: &Glyphs, width: usize) -> OverlayText {
        let (name, folder, both) = self.facts();
        let warn = Style::new().fg(Color::Yellow);
        let mut body = view::note(
            &format!("{name} is already in {folder}. Fetchpath does not replace it."),
            warn,
            width,
        );
        body.push(Line::from(vec![
            ratatui::text::Span::styled("Keep both  ", DIM),
            ratatui::text::Span::raw(view::fit(&both, width.saturating_sub(11), glyphs.more)),
        ]));
        let mut cursor = None;
        if let Some(prompt) = &self.editing {
            let (line, column) = view::edit_line("New name", prompt, width);
            cursor = Some((body.len(), column));
            body.push(line);
        }
        if let Some(problem) = &self.problem {
            body.extend(view::note(problem, Style::new().fg(Color::Red), width));
        }
        let keys = if self.editing.is_some() {
            "Enter save · Esc back".to_owned()
        } else {
            "Enter keep both · N another name · Esc leave it stopped".to_owned()
        };
        OverlayText {
            card: CardText {
                title: format!("Already exists · {}", client::name(&self.job)),
                body,
                keys,
            },
            targets: Vec::new(),
            cursor,
        }
    }

    pub fn plain_lines(&self) -> Vec<String> {
        let (name, folder, both) = self.facts();
        vec![
            format!("{name} is already in {folder}. Fetchpath does not replace it."),
            format!(
                "Press Enter to keep both, saving the download as {both}; type another name; or type no to leave it stopped."
            ),
        ]
    }

    pub fn plain_answer(&self, engine: &Engine, text: &str) -> (Vec<Out>, bool) {
        match text.trim() {
            "" | "y" | "yes" => (vec![self.keep_both(engine)], true),
            "n" | "no" | "N" | "No" | "NO" => (vec![self.left()], true),
            name => match rename(engine, &self.job, name) {
                Ok(said) => (vec![Out::new(Tone::Normal, said)], true),
                Err(problem) => (vec![Out::new(Tone::Bad, problem)], false),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stopped(destination: &Path) -> JobSnapshot {
        serde_json::from_value(serde_json::json!({
            "job_id": "3f1c2a9b-0000-4000-8000-000000000001",
            "kind": "file", "state": "failed", "job_revision": 1, "last_seq": 1,
            "source_display": "https://a.test/report.pdf",
            "destination": destination.display().to_string(),
            "progress": { "bytes_received": 0 },
            "created_at": "2026-09-26T10:00:00Z",
            "error": {
                "code": "storage.destination_conflict",
                "message_key": "storage.destination_conflict",
                "retryable": false,
                "scope": "job",
                "message": "A file already exists at this destination.",
            },
        }))
        .unwrap()
    }

    #[test]
    fn only_a_download_stopped_by_an_existing_file_asks() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("report.pdf");
        assert!(Conflict::new(stopped(&path)).is_some());
        let mut other = stopped(&path);
        other.error.as_mut().unwrap().code = fetchpath_protocol::ErrorCode::REVISION_CONFLICT;
        assert!(Conflict::new(other).is_none());
    }

    #[test]
    fn the_prompt_offers_the_next_free_name_and_says_nothing_is_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("report.pdf");
        std::fs::write(&path, b"x").unwrap();
        let conflict = Conflict::new(stopped(&path)).unwrap();
        let text = conflict.plain_lines().join("\n");
        assert!(text.contains("report.pdf is already in"), "{text}");
        assert!(text.contains("does not replace it"), "{text}");
        assert!(text.contains("report (1).pdf"), "{text}");
        let shown = conflict.text(&view::ASCII, 60);
        assert_eq!(
            shown.card.keys,
            "Enter keep both · N another name · Esc leave it stopped"
        );
    }
}
