//! An agent's request waiting for the person (contract D1): who asks, what
//! it would save, how big, where, and why its access does not cover it.
//! A approves, D denies, Esc decides later.

use super::super::line::{Out, Tone};
use super::super::view::{self, CardText, DIM, Glyphs, OverlayText};
use crate::client::{self, Engine};
use crate::queue::{self, Control};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind};
use fetchpath_protocol::JobSnapshot;
use fetchpath_protocol::principal::{ApprovalReason, Principal};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use std::path::Path;

pub struct Approval {
    pub job: JobSnapshot,
    /// Plain mode has printed the question.
    pub asked: bool,
}

/// Who asks: an agent's own name, else the principal as written.
pub fn who(job: &JobSnapshot) -> String {
    match &job.principal {
        Principal::Agent(name) => format!("the agent {name}"),
        other => other.to_string(),
    }
}

fn reason(reason: &ApprovalReason) -> &'static str {
    match reason {
        ApprovalReason::OutsideGrantedFolders => {
            "It would save outside the folders you let it use."
        }
        ApprovalReason::SizeLimit => "It passed the size you let it download and stopped.",
        ApprovalReason::RateLimit => "It asked for more downloads this hour than you allow.",
        ApprovalReason::PeerDiscovery => "It would contact peers and discovery services.",
        ApprovalReason::PeerUpload => "It would upload pieces to peers.",
        ApprovalReason::Unknown => "It asked for something its access does not cover.",
    }
}

/// The facts shown: label and value.
fn facts(job: &JobSnapshot) -> Vec<(&'static str, String)> {
    let size = match job.progress.bytes_total {
        Some(total) => client::bytes(total),
        None if job.progress.bytes_received > 0 => format!(
            "stopped at {}, total unknown",
            client::bytes(job.progress.bytes_received)
        ),
        None => "unknown until it starts".to_owned(),
    };
    let (name, folder) = match job.destination.as_deref().map(Path::new) {
        Some(path) => (
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default(),
            path.parent()
                .map(|folder| folder.display().to_string())
                .unwrap_or_default(),
        ),
        None => (client::name(job), "not chosen".to_owned()),
    };
    vec![
        ("File", name),
        ("From", job.source_display.clone()),
        ("Size", size),
        ("Folder", folder),
    ]
}

fn reasons(job: &JobSnapshot) -> Vec<&'static str> {
    match &job.approval {
        Some(request) if !request.reasons.is_empty() => {
            request.reasons.iter().map(reason).collect()
        }
        _ => vec![reason(&ApprovalReason::Unknown)],
    }
}

/// The request as lines, for `/approvals` and plain mode.
pub fn lines(job: &JobSnapshot, index: Option<usize>) -> Vec<String> {
    let number = index
        .map(|index| format!("{} ", index + 1))
        .unwrap_or_default();
    let mut out = vec![format!(
        "{number}{} ({}) asks to download:",
        who(job),
        client::short_id(job)
    )];
    for (label, value) in facts(job) {
        out.push(format!("  {label:<7}{value}"));
    }
    for why in reasons(job) {
        out.push(format!("  {why}"));
    }
    out
}

impl Approval {
    pub fn new(job: JobSnapshot) -> Self {
        Self { job, asked: false }
    }

    fn answer(&self, engine: &Engine, action: Control) -> Out {
        match engine.send(queue::control_command(action, &self.job)) {
            Ok(result) => Out::new(Tone::Normal, queue::describe(action, &self.job, &result)),
            Err(error) => Out::new(
                Tone::Bad,
                format!(
                    "{} {}: {}",
                    client::short_id(&self.job),
                    client::name(&self.job),
                    error.message
                ),
            ),
        }
    }

    /// Returns lines for scrollback and whether the card is finished; no
    /// lines and finished means "later".
    pub fn key(&self, engine: &Engine, key: KeyEvent) -> (Vec<Out>, bool) {
        if key.kind == KeyEventKind::Release {
            return (Vec::new(), false);
        }
        match key.code {
            KeyCode::Char('a' | 'A') => (vec![self.answer(engine, Control::Approve)], true),
            KeyCode::Char('d' | 'D') => (vec![self.answer(engine, Control::Deny)], true),
            KeyCode::Esc => (Vec::new(), true),
            _ => (Vec::new(), false),
        }
    }

    pub fn text(&self, glyphs: &Glyphs, width: usize) -> OverlayText {
        let mut body = vec![Line::styled(
            view::fit(
                &format!("{} asks to download:", who(&self.job)),
                width,
                glyphs.more,
            ),
            Style::new().add_modifier(Modifier::BOLD),
        )];
        for (label, value) in facts(&self.job) {
            body.push(Line::from(vec![
                Span::styled(format!("{label:<8}"), DIM),
                Span::raw(view::middle_fit(
                    &value,
                    width.saturating_sub(8),
                    glyphs.more,
                )),
            ]));
        }
        for why in reasons(&self.job) {
            body.extend(view::note(why, Style::new().fg(Color::Yellow), width));
        }
        OverlayText {
            card: CardText {
                title: format!("Agent request · {}", client::short_id(&self.job)),
                body,
                keys: "A approve · D deny · Esc decide later".to_owned(),
            },
            targets: Vec::new(),
            cursor: None,
        }
    }

    pub fn plain_lines(&self) -> Vec<String> {
        let mut out = lines(&self.job, None);
        out.push(
            "Type approve or deny, or press Enter to decide later (/approvals asks again)."
                .to_owned(),
        );
        out
    }

    pub fn plain_answer(&self, engine: &Engine, text: &str) -> (Vec<Out>, bool) {
        match text.trim().to_ascii_lowercase().as_str() {
            "a" | "approve" | "yes" | "y" => (vec![self.answer(engine, Control::Approve)], true),
            "d" | "deny" | "no" | "n" => (vec![self.answer(engine, Control::Deny)], true),
            "" | "later" => (Vec::new(), true),
            _ => (
                vec![Out::new(
                    Tone::Normal,
                    "Type approve or deny, or press Enter to decide later.",
                )],
                false,
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> JobSnapshot {
        serde_json::from_value(serde_json::json!({
            "job_id": "3f1c2a9b-0000-4000-8000-000000000001",
            "kind": "file", "state": "awaiting_approval", "job_revision": 1, "last_seq": 1,
            "source_display": "https://a.test/model.bin",
            "destination": "C:\\Users\\person\\Documents\\model.bin",
            "progress": { "bytes_received": 0, "bytes_total": 3221225472u64 },
            "created_at": "2026-09-26T10:00:00Z",
            "principal": "agent:helper",
            "approval": { "reasons": ["outside_granted_folders", "size_limit"] },
        }))
        .unwrap()
    }

    #[test]
    fn a_request_names_the_agent_file_size_folder_and_why() {
        assert_eq!(
            lines(&request(), Some(0)),
            [
                "1 the agent helper (3f1c2a9b) asks to download:",
                "  File   model.bin",
                "  From   https://a.test/model.bin",
                "  Size   3.0 GiB",
                "  Folder C:\\Users\\person\\Documents",
                "  It would save outside the folders you let it use.",
                "  It passed the size you let it download and stopped.",
            ]
        );
        let card = Approval::new(request()).text(&view::ASCII, 70);
        assert_eq!(card.card.keys, "A approve · D deny · Esc decide later");
        assert_eq!(card.card.body.len(), 7);
    }
}
