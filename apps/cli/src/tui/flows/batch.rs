//! Several links at once: each is looked at, then one card lists them all
//! with a mark on each to download. Files and videos (at the quality their
//! own card would choose) start marked; web pages and links that could not
//! be read start unmarked. Two links with the same name, or a name already
//! in the folder, are saved as "name (N)" so nothing is replaced.

use super::super::review::{self, Card, Draft, Look, Stage};
use super::super::view::{self, ACCENT, CardText, DIM, Glyphs, OverlayText, Target};
use crate::client::{self, Engine};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use fetchpath_protocol::ProtocolError;
use fetchpath_protocol::model::LinkKind;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use unicode_width::UnicodeWidthStr;

/// Rows listed at once; the list follows the selection.
const ROWS: usize = 8;

pub struct Batch {
    pub cards: Vec<Card>,
    pub marked: Vec<bool>,
    pub selected: usize,
    answers: Option<Receiver<(usize, Result<Look, ProtocolError>)>>,
    pending: usize,
}

/// What a key did to the batch.
#[derive(Debug, Eq, PartialEq)]
pub enum Answer {
    None,
    Confirm,
    Cancel,
}

impl Batch {
    fn with(drafts: Vec<Draft>) -> Self {
        let count = drafts.len();
        Self {
            cards: drafts.into_iter().map(Card::new).collect(),
            marked: vec![false; count],
            selected: 0,
            answers: None,
            pending: count,
        }
    }

    /// Looks at every link on one thread, one after another, so the view
    /// keeps moving.
    pub fn start(drafts: Vec<Draft>) -> Self {
        let mut batch = Self::with(drafts);
        let asked: Vec<Draft> = batch.cards.iter().map(|card| card.draft.clone()).collect();
        let (send, receive) = mpsc::channel();
        std::thread::spawn(move || {
            let engine = Engine::connect();
            for (index, draft) in asked.iter().enumerate() {
                let result = match &engine {
                    Ok(engine) => review::look(engine, draft),
                    Err(error) => Err(error.clone()),
                };
                if send.send((index, result)).is_err() {
                    return;
                }
            }
        });
        batch.answers = Some(receive);
        batch
    }

    /// Plain mode: looks at every link now.
    pub fn look_now(engine: &Engine, drafts: Vec<Draft>) -> Self {
        let mut batch = Self::with(drafts);
        for index in 0..batch.cards.len() {
            let result = review::look(engine, &batch.cards[index].draft);
            batch.settle(index, result);
        }
        batch
    }

    fn settle(&mut self, index: usize, result: Result<Look, ProtocolError>) {
        let card = &mut self.cards[index];
        card.set(result);
        self.marked[index] = starts_marked(card);
        self.pending = self.pending.saturating_sub(1);
        self.share_names();
    }

    /// Takes the engine's answers that have come; true if anything changed.
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        loop {
            let Some(answers) = &self.answers else {
                return changed;
            };
            match answers.try_recv() {
                Ok((index, result)) => {
                    self.settle(index, result);
                    changed = true;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.answers = None;
                    for index in 0..self.cards.len() {
                        if matches!(self.cards[index].stage, Stage::Looking) {
                            let lost = client::input_error("The link could not be looked at.");
                            self.settle(index, Err(lost));
                        }
                    }
                    return true;
                }
            }
        }
        changed
    }

    /// Each marked link avoids the paths the marked links before it will
    /// use, so two links with the same name do not clash.
    fn share_names(&mut self) {
        let mut taken: Vec<PathBuf> = Vec::new();
        for (card, marked) in self.cards.iter_mut().zip(&self.marked) {
            card.taken = taken.clone();
            if *marked && let Ok(path) = card.destination() {
                taken.push(path);
            }
        }
    }

    pub fn looking(&self) -> bool {
        self.pending > 0
    }

    pub fn chosen(&self) -> impl Iterator<Item = &Card> {
        self.cards
            .iter()
            .zip(&self.marked)
            .filter(|(_, marked)| **marked)
            .map(|(card, _)| card)
    }

    pub fn toggle(&mut self, index: usize) {
        if let Some(card) = self.cards.get(index)
            && can_mark(card)
        {
            self.marked[index] = !self.marked[index];
            self.share_names();
        }
    }

    fn toggle_all(&mut self) {
        let markable: Vec<usize> = (0..self.cards.len())
            .filter(|&index| can_mark(&self.cards[index]))
            .collect();
        let all = markable.iter().all(|&index| self.marked[index]);
        for index in markable {
            self.marked[index] = !all;
        }
        self.share_names();
    }

    pub fn key(&mut self, key: KeyEvent) -> Answer {
        if key.kind == KeyEventKind::Release {
            return Answer::None;
        }
        let last = self.cards.len().saturating_sub(1);
        match key.code {
            KeyCode::Esc => return Answer::Cancel,
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                return Answer::Cancel;
            }
            KeyCode::Enter if !self.looking() && self.chosen().next().is_some() => {
                return Answer::Confirm;
            }
            KeyCode::Up | KeyCode::Char('k') => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => self.selected = (self.selected + 1).min(last),
            KeyCode::Home => self.selected = 0,
            KeyCode::End => self.selected = last,
            KeyCode::Char(' ') => self.toggle(self.selected),
            KeyCode::Char('a' | 'A') => self.toggle_all(),
            _ => {}
        }
        Answer::None
    }

    /// One row: the mark, the name it will be saved as, and what it is.
    fn row(&self, index: usize, glyphs: &Glyphs) -> (String, String) {
        let card = &self.cards[index];
        let mark = if !can_mark(card) {
            "   "
        } else if self.marked[index] {
            if glyphs.unicode { "[✓]" } else { "[x]" }
        } else {
            "[ ]"
        };
        let name = card
            .destination()
            .ok()
            .and_then(|path| {
                path.file_name()
                    .map(|name| name.to_string_lossy().into_owned())
            })
            .unwrap_or_else(|| card.file_name());
        let what = match &card.stage {
            Stage::Looking => format!("looking{}", glyphs.more),
            Stage::Failed(message) => format!("could not look: {message}"),
            Stage::Ready(look) => describe(card, look),
        };
        let clash = card
            .clash()
            .filter(|_| self.marked[index])
            .map(|other| format!(" · {other} is taken, kept both"))
            .unwrap_or_default();
        (format!("{mark} {name}"), format!("{what}{clash}"))
    }

    fn keys(&self, glyphs: &Glyphs) -> String {
        let count = self.chosen().count();
        let enter = if self.looking() {
            format!("looking at {} more", self.pending)
        } else if count == 0 {
            "nothing marked".to_owned()
        } else {
            format!("Enter download {count}")
        };
        let moves = if glyphs.unicode { "↑↓" } else { "Up/Down" };
        format!("{moves} choose · Space mark · A all · {enter} · Esc cancel")
    }

    /// Where the marked downloads go: one folder, or "several folders".
    fn folders(&self) -> String {
        let mut folders: Vec<String> = self
            .chosen()
            .filter_map(|card| card.destination().ok())
            .filter_map(|path| path.parent().map(|folder| folder.display().to_string()))
            .collect();
        folders.dedup();
        match folders.as_slice() {
            [] => String::new(),
            [one] => format!("Save in  {one}"),
            _ => "Saved in several folders, as each rule says".to_owned(),
        }
    }

    pub fn text(&self, glyphs: &Glyphs, tick: u64, width: usize) -> OverlayText {
        let first = self
            .selected
            .saturating_sub(ROWS - 1)
            .min(self.cards.len().saturating_sub(ROWS));
        let mut body = Vec::new();
        let mut targets = Vec::new();
        let name_width = (width * 2 / 5).max(12);
        for index in (first..self.cards.len()).take(ROWS) {
            let (name, what) = self.row(index, glyphs);
            let chosen = index == self.selected;
            let style = if chosen {
                Style::new()
                    .fg(ACCENT)
                    .add_modifier(Modifier::BOLD | Modifier::REVERSED)
            } else {
                Style::new()
            };
            let spin = if matches!(self.cards[index].stage, Stage::Looking) {
                glyphs.spinner[(tick as usize) % glyphs.spinner.len()]
            } else {
                ""
            };
            targets.push((body.len(), Target::Choice(index)));
            body.push(Line::from(vec![
                Span::styled(view::pad(&name, name_width, glyphs.more), style),
                Span::raw("  "),
                Span::styled(spin, Style::new().fg(ACCENT)),
                Span::styled(
                    view::fit(
                        &what,
                        width.saturating_sub(name_width + 2 + spin.width()),
                        glyphs.more,
                    ),
                    DIM,
                ),
            ]));
        }
        if self.cards.len() > ROWS {
            body.push(Line::styled(
                format!("    {} of {} links", self.selected + 1, self.cards.len()),
                DIM,
            ));
        }
        let folders = self.folders();
        if !folders.is_empty() {
            body.push(Line::styled(view::fit(&folders, width, glyphs.more), DIM));
        }
        OverlayText {
            card: CardText {
                title: format!("{} links", self.cards.len()),
                body,
                keys: self.keys(glyphs),
            },
            targets,
            cursor: None,
        }
    }

    /// Plain mode: every row, numbered, and what to type.
    pub fn plain_lines(&self) -> Vec<String> {
        let mut out = vec![format!("{} links:", self.cards.len())];
        for index in 0..self.cards.len() {
            let (name, what) = self.row(index, &view::ASCII);
            out.push(format!("  {:>2} {name}  {what}", index + 1));
        }
        let folders = self.folders();
        if !folders.is_empty() {
            out.push(format!("  {folders}"));
        }
        out.push(format!(
            "Press Enter to download the {} marked, type numbers to mark or unmark (for example 2 3), or no to cancel.",
            self.chosen().count()
        ));
        out
    }

    /// Plain mode: a typed answer. Numbers toggle rows and print the list
    /// again.
    pub fn plain_answer(&mut self, text: &str) -> PlainReply {
        let text = text.trim().to_ascii_lowercase();
        match text.as_str() {
            "" | "y" | "yes" if self.chosen().next().is_some() => PlainReply::Confirm,
            "" | "y" | "yes" => PlainReply::Unclear,
            "n" | "no" | "cancel" => PlainReply::Cancel,
            _ => {
                let numbers: Option<Vec<usize>> = text
                    .split([' ', ','])
                    .filter(|word| !word.is_empty())
                    .map(|word| {
                        word.parse::<usize>()
                            .ok()
                            .filter(|n| (1..=self.cards.len()).contains(n))
                    })
                    .collect();
                match numbers {
                    Some(numbers) if !numbers.is_empty() => {
                        for number in numbers {
                            self.toggle(number - 1);
                        }
                        PlainReply::Changed
                    }
                    _ => PlainReply::Unclear,
                }
            }
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
pub enum PlainReply {
    Confirm,
    Cancel,
    Changed,
    Unclear,
}

/// What a looked-at link is, in a few words.
fn describe(card: &Card, look: &Look) -> String {
    if let Some(variant) = card.variants().get(card.choice) {
        return format!("video · {}", variant.label);
    }
    match look.link.kind {
        LinkKind::MediaPage => "video page; formats could not be read".to_owned(),
        LinkKind::WebPage => "web page, not a file".to_owned(),
        _ => {
            let size = look
                .link
                .size_bytes
                .map_or("size unknown".to_owned(), client::bytes);
            let kind = review::kind_label(look.link.content_type.as_deref(), &card.file_name());
            format!("{kind} · {size}")
        }
    }
}

/// Whether a link can be downloaded from the batch: what its own card would
/// allow.
fn can_mark(card: &Card) -> bool {
    !matches!(card.stage, Stage::Looking) && card.can_confirm()
}

fn starts_marked(card: &Card) -> bool {
    match &card.stage {
        Stage::Ready(look) => can_mark(card) && look.link.kind != LinkKind::WebPage,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fetchpath_protocol::SensitiveUrl;
    use fetchpath_protocol::model::LinkInspection;

    fn draft(link: &str) -> Draft {
        Draft {
            link: link.to_owned(),
            url: SensitiveUrl::try_from(link.to_owned()).unwrap(),
            to: None,
            at: None,
            sha256: None,
            quality: None,
        }
    }

    fn look(kind: &str, name: &str, folder: &std::path::Path) -> Look {
        let link: LinkInspection = serde_json::from_value(serde_json::json!({
            "kind": kind,
            "file_name": name,
            "size_bytes": 2048,
            "resumable": true,
        }))
        .unwrap();
        Look {
            link,
            media: None,
            media_error: None,
            tools_missing: false,
            folder: Some(folder.to_path_buf()),
        }
    }

    fn batch(folder: &std::path::Path) -> Batch {
        let mut batch = Batch::with(vec![
            draft("https://a.test/a/report.pdf"),
            draft("https://b.test/b/report.pdf"),
            draft("https://c.test/"),
            draft("https://d.test/broken"),
        ]);
        batch.settle(0, Ok(look("file", "report.pdf", folder)));
        batch.settle(1, Ok(look("file", "report.pdf", folder)));
        batch.settle(2, Ok(look("web_page", "index.html", folder)));
        batch.settle(3, Err(client::input_error("refused")));
        batch
    }

    #[test]
    fn files_start_marked_pages_do_not_and_same_names_keep_both() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("report.pdf"), b"x").unwrap();
        let batch = batch(dir.path());
        assert_eq!(batch.marked, [true, true, false, false]);
        assert!(!batch.looking());
        let saved: Vec<PathBuf> = batch
            .chosen()
            .map(|card| card.destination().unwrap())
            .collect();
        assert_eq!(
            saved,
            [
                dir.path().join("report (1).pdf"),
                dir.path().join("report (2).pdf")
            ]
        );
        let lines = batch.plain_lines();
        assert!(lines[1].contains("[x] report (1).pdf"), "{lines:?}");
        assert!(
            lines[1].contains("report.pdf is taken, kept both"),
            "{lines:?}"
        );
        assert!(
            lines[3].contains("[ ] index.html  web page, not a file"),
            "{lines:?}"
        );
        assert!(lines[4].contains("could not look: refused"), "{lines:?}");
        assert!(
            lines
                .last()
                .unwrap()
                .starts_with("Press Enter to download the 2 marked")
        );
    }

    #[test]
    fn keys_and_typed_numbers_mark_rows_and_esc_cancels() {
        let dir = tempfile::tempdir().unwrap();
        let mut batch = batch(dir.path());
        let press = |code| KeyEvent::from(code);
        assert_eq!(batch.key(press(KeyCode::Char(' '))), Answer::None);
        assert_eq!(batch.marked, [false, true, false, false]);
        // Unmarking the first frees its name for the second.
        assert_eq!(
            batch.chosen().next().unwrap().destination().unwrap(),
            dir.path().join("report.pdf")
        );
        // A marks everything its own card could start, a page included.
        batch.key(press(KeyCode::Char('a')));
        assert_eq!(batch.marked, [true, true, true, true]);
        assert_eq!(batch.plain_answer("3 4"), PlainReply::Changed);
        assert_eq!(batch.marked, [true, true, false, false]);
        assert_eq!(batch.plain_answer("9"), PlainReply::Unclear);
        assert_eq!(batch.plain_answer(""), PlainReply::Confirm);
        assert_eq!(batch.key(press(KeyCode::Enter)), Answer::Confirm);
        assert_eq!(batch.key(press(KeyCode::Esc)), Answer::Cancel);
        assert_eq!(batch.plain_answer("no"), PlainReply::Cancel);
    }
}
