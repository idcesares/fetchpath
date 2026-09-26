//! The one-line editor under the live panel.
//!
//! FP-059 chose a small in-house editor over a line-editor crate: such
//! crates own the cursor and the screen while they read a line, which
//! cannot coexist with a panel redrawn above the prompt many times a
//! second. This one holds text and a cursor, and the inline view draws it.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

/// What a key did.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Action {
    None,
    Edited,
    Submit(String),
    Complete,
    /// Ctrl+C or Ctrl+D on an empty line.
    Leave,
}

#[derive(Default)]
pub struct Prompt {
    text: String,
    /// A byte offset on a character boundary.
    cursor: usize,
    history: Vec<String>,
    /// Which history entry Up and Down have reached, and the line being
    /// written before recall began.
    recall: Option<(usize, String)>,
}

impl Prompt {
    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn before_cursor(&self) -> &str {
        &self.text[..self.cursor]
    }

    pub fn set(&mut self, text: String) {
        self.cursor = text.len();
        self.text = text;
        self.recall = None;
    }

    /// Replaces `start..cursor` with `insert`, as completion does.
    pub fn replace_before_cursor(&mut self, start: usize, insert: &str) {
        self.text.replace_range(start..self.cursor, insert);
        self.cursor = start + insert.len();
        self.recall = None;
    }

    pub fn insert(&mut self, text: &str) {
        let clean: String = text
            .chars()
            .map(|c| {
                if c == '\t' || c == '\n' || c == '\r' {
                    ' '
                } else {
                    c
                }
            })
            .filter(|c| !c.is_control())
            .collect();
        self.text.insert_str(self.cursor, &clean);
        self.cursor += clean.len();
        self.recall = None;
    }

    fn previous_boundary(&self, from: usize) -> usize {
        self.text[..from]
            .char_indices()
            .next_back()
            .map_or(0, |(index, _)| index)
    }

    fn next_boundary(&self, from: usize) -> usize {
        self.text[from..]
            .chars()
            .next()
            .map_or(from, |c| from + c.len_utf8())
    }

    /// The start of the word before `from`, skipping spaces first.
    fn word_back(&self, from: usize) -> usize {
        let before = &self.text[..from];
        let trimmed = before.trim_end();
        trimmed
            .char_indices()
            .rev()
            .find(|(_, c)| c.is_whitespace())
            .map_or(0, |(index, c)| index + c.len_utf8())
    }

    fn word_forward(&self, from: usize) -> usize {
        let after = &self.text[from..];
        let skipped = after.len() - after.trim_start().len();
        after[skipped..]
            .char_indices()
            .find(|(_, c)| c.is_whitespace())
            .map_or(self.text.len(), |(index, _)| from + skipped + index)
    }

    fn recall(&mut self, older: bool) {
        if self.history.is_empty() {
            return;
        }
        let next = match (&self.recall, older) {
            (None, true) => Some(self.history.len() - 1),
            (None, false) => return,
            (Some((0, _)), true) => Some(0),
            (Some((at, _)), true) => Some(at - 1),
            (Some((at, _)), false) if at + 1 < self.history.len() => Some(at + 1),
            (Some(_), false) => None,
        };
        let draft = match self.recall.take() {
            Some((_, draft)) => draft,
            None => self.text.clone(),
        };
        match next {
            Some(at) => {
                self.text = self.history[at].clone();
                self.recall = Some((at, draft));
            }
            None => self.text = draft,
        }
        self.cursor = self.text.len();
    }

    pub fn handle(&mut self, key: KeyEvent) -> Action {
        // Windows reports releases too; only presses and repeats type.
        if key.kind == KeyEventKind::Release {
            return Action::None;
        }
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        match key.code {
            KeyCode::Char('c' | 'd') if control => {
                if self.text.is_empty() {
                    return Action::Leave;
                }
                if key.code == KeyCode::Char('d') {
                    return self.delete_forward();
                }
                self.set(String::new());
            }
            KeyCode::Char('a') if control => self.cursor = 0,
            KeyCode::Char('e') if control => self.cursor = self.text.len(),
            KeyCode::Char('b') if control => self.cursor = self.previous_boundary(self.cursor),
            KeyCode::Char('f') if control => self.cursor = self.next_boundary(self.cursor),
            KeyCode::Char('b') if alt => self.cursor = self.word_back(self.cursor),
            KeyCode::Char('f') if alt => self.cursor = self.word_forward(self.cursor),
            KeyCode::Char('u') if control => {
                self.text.replace_range(..self.cursor, "");
                self.cursor = 0;
            }
            KeyCode::Char('k') if control => self.text.truncate(self.cursor),
            KeyCode::Char('w') if control => self.delete_word_back(),
            KeyCode::Backspace if control || alt => self.delete_word_back(),
            KeyCode::Char('p') if control => self.recall(true),
            KeyCode::Char('n') if control => self.recall(false),
            KeyCode::Char(c) if !control || alt => {
                // AltGr arrives as Ctrl+Alt with the typed character.
                let mut buffer = [0; 4];
                self.insert(c.encode_utf8(&mut buffer));
            }
            KeyCode::Backspace => {
                if self.cursor > 0 {
                    let start = self.previous_boundary(self.cursor);
                    self.text.replace_range(start..self.cursor, "");
                    self.cursor = start;
                }
            }
            KeyCode::Delete => return self.delete_forward(),
            KeyCode::Left if control => self.cursor = self.word_back(self.cursor),
            KeyCode::Right if control => self.cursor = self.word_forward(self.cursor),
            KeyCode::Left => self.cursor = self.previous_boundary(self.cursor),
            KeyCode::Right => self.cursor = self.next_boundary(self.cursor),
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = self.text.len(),
            KeyCode::Up => self.recall(true),
            KeyCode::Down => self.recall(false),
            KeyCode::Esc => self.set(String::new()),
            KeyCode::Tab => return Action::Complete,
            KeyCode::Enter => {
                let line = std::mem::take(&mut self.text);
                self.cursor = 0;
                self.recall = None;
                if !line.trim().is_empty() && self.history.last() != Some(&line) {
                    self.history.push(line.clone());
                }
                return Action::Submit(line);
            }
            _ => return Action::None,
        }
        Action::Edited
    }

    fn delete_forward(&mut self) -> Action {
        let end = self.next_boundary(self.cursor);
        self.text.replace_range(self.cursor..end, "");
        Action::Edited
    }

    fn delete_word_back(&mut self) {
        let start = self.word_back(self.cursor);
        self.text.replace_range(start..self.cursor, "");
        self.cursor = start;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(prompt: &mut Prompt, code: KeyCode) -> Action {
        prompt.handle(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn control(prompt: &mut Prompt, c: char) -> Action {
        prompt.handle(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL))
    }

    fn typed(prompt: &mut Prompt, text: &str) {
        for c in text.chars() {
            press(prompt, KeyCode::Char(c));
        }
    }

    #[test]
    fn editing_moves_by_characters_and_words() {
        let mut prompt = Prompt::default();
        typed(&mut prompt, "/pause 12 ünï");
        press(&mut prompt, KeyCode::Left);
        press(&mut prompt, KeyCode::Backspace);
        assert_eq!(prompt.text(), "/pause 12 üï");
        control(&mut prompt, 'w');
        assert_eq!(prompt.text(), "/pause 12 ï");
        prompt.handle(KeyEvent::new(KeyCode::Left, KeyModifiers::CONTROL));
        assert_eq!(prompt.before_cursor(), "/pause ");
        control(&mut prompt, 'k');
        assert_eq!(prompt.text(), "/pause ");
        press(&mut prompt, KeyCode::Home);
        press(&mut prompt, KeyCode::Delete);
        assert_eq!(prompt.text(), "pause ");
        control(&mut prompt, 'e');
        control(&mut prompt, 'u');
        assert_eq!(prompt.text(), "");
    }

    #[test]
    fn enter_submits_and_up_recalls_without_losing_the_draft() {
        let mut prompt = Prompt::default();
        typed(&mut prompt, "/queue");
        assert_eq!(
            press(&mut prompt, KeyCode::Enter),
            Action::Submit("/queue".into())
        );
        typed(&mut prompt, "/help");
        press(&mut prompt, KeyCode::Enter);
        typed(&mut prompt, "draft");
        press(&mut prompt, KeyCode::Up);
        assert_eq!(prompt.text(), "/help");
        press(&mut prompt, KeyCode::Up);
        press(&mut prompt, KeyCode::Up);
        assert_eq!(prompt.text(), "/queue");
        press(&mut prompt, KeyCode::Down);
        press(&mut prompt, KeyCode::Down);
        assert_eq!(prompt.text(), "draft");
    }

    #[test]
    fn pasted_text_stays_on_one_line_and_ctrl_c_clears_then_leaves() {
        let mut prompt = Prompt::default();
        prompt.insert("https://a.test/x\r\nhttps://a.test/y\u{7}");
        assert_eq!(prompt.text(), "https://a.test/x  https://a.test/y");
        assert_eq!(control(&mut prompt, 'c'), Action::Edited);
        assert_eq!(prompt.text(), "");
        assert_eq!(control(&mut prompt, 'c'), Action::Leave);
        let release = KeyEvent {
            kind: KeyEventKind::Release,
            ..KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE)
        };
        assert_eq!(prompt.handle(release), Action::None);
        assert_eq!(prompt.text(), "");
    }
}
