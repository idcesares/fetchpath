//! Drawing the inline view: the live panel, a summary rule, the prompt and
//! a hint line, plus the one-line receipts printed into scrollback.

use super::line::{Out, Tone};
use super::live::Row;
use crate::client;
use fetchpath_protocol::JobSnapshot;
use fetchpath_protocol::model::JobState;
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Symbols for states and receipts. The classic console's default fonts
/// lack most symbols, so ASCII is used there and in plain mode.
#[derive(Clone, Copy, Debug)]
pub struct Glyphs {
    pub saved: &'static str,
    pub failed: &'static str,
    pub cancelled: &'static str,
    pub active: &'static str,
    pub paused: &'static str,
    pub waiting: &'static str,
    pub attention: &'static str,
    pub rule: &'static str,
    pub prompt: &'static str,
    pub more: &'static str,
}

pub const UNICODE: Glyphs = Glyphs {
    saved: "✓",
    failed: "✗",
    cancelled: "–",
    active: "↓",
    paused: "‖",
    waiting: "·",
    attention: "!",
    rule: "─",
    prompt: "›",
    more: "…",
};

pub const ASCII: Glyphs = Glyphs {
    saved: "+",
    failed: "x",
    cancelled: "-",
    active: ">",
    paused: "=",
    waiting: ".",
    attention: "!",
    rule: "-",
    prompt: ">",
    more: "...",
};

/// Unicode where the terminal is known to draw it: Windows Terminal, and
/// terminals that name themselves (VS Code, WezTerm and others).
pub fn glyphs_for_terminal() -> Glyphs {
    let named = |name: &str| std::env::var_os(name).is_some_and(|value| !value.is_empty());
    if named("WT_SESSION") || named("TERM_PROGRAM") {
        UNICODE
    } else {
        ASCII
    }
}

const DIM: Style = Style::new().fg(Color::DarkGray);

fn tone_style(tone: Tone) -> Style {
    match tone {
        Tone::Normal => Style::new(),
        Tone::Dim => DIM,
        Tone::Good => Style::new().fg(Color::Green),
        Tone::Bad => Style::new().fg(Color::Red),
    }
}

/// A receipt: saved, failed or cancelled, the name, and where it went or
/// why it stopped.
pub fn receipt(job: &JobSnapshot, glyphs: &Glyphs) -> Out {
    let name = client::name(job);
    match job.state {
        JobState::Completed => {
            let size = job
                .progress
                .bytes_total
                .or((job.progress.bytes_received > 0).then_some(job.progress.bytes_received))
                .map(|bytes| format!("  {}", client::bytes(bytes)))
                .unwrap_or_default();
            let place = job
                .destination
                .as_deref()
                .map(|path| format!("  {path}"))
                .unwrap_or_default();
            Out::new(Tone::Good, format!("{} {name}{size}{place}", glyphs.saved))
        }
        JobState::Cancelled => {
            Out::new(Tone::Dim, format!("{} {name}  cancelled", glyphs.cancelled))
        }
        _ => {
            let why = job.error.as_ref().map_or_else(
                || client::state_label(job).to_owned(),
                |error| error.message.clone(),
            );
            Out::new(
                Tone::Bad,
                format!(
                    "{} {name}  {why}  (/retry {})",
                    glyphs.failed,
                    client::short_id(job)
                ),
            )
        }
    }
}

fn state_glyph(group: u8, glyphs: &Glyphs) -> (&'static str, Style) {
    match group {
        0 => (glyphs.active, Style::new().fg(Color::Cyan)),
        1 => (glyphs.attention, Style::new().fg(Color::Yellow)),
        2 => (glyphs.paused, DIM),
        _ => (glyphs.waiting, DIM),
    }
}

/// What the right side of a panel row says: live progress while moving,
/// otherwise the state and anything known about amount or timing.
pub fn detail(job: &JobSnapshot) -> String {
    match job.state {
        JobState::Running => client::progress_line(&job.progress).trim_start().to_owned(),
        JobState::Queued if job.not_before.is_some() => format!(
            "starts {}",
            crate::when::local(job.not_before.expect("checked"))
        ),
        JobState::Failed if job.retry_at.is_some() => format!(
            "retrying {}",
            crate::when::local(job.retry_at.expect("checked"))
        ),
        _ => {
            let amount = client::amount(&job.progress);
            if amount.is_empty() {
                client::state_label(job).to_owned()
            } else {
                format!("{}  {amount}", client::state_label(job))
            }
        }
    }
}

/// Cuts `text` to `width` columns, ending with an ellipsis when cut.
pub fn fit(text: &str, width: usize, more: &str) -> String {
    if text.width() <= width {
        return text.to_owned();
    }
    let room = width.saturating_sub(more.width());
    let mut used = 0;
    let mut cut = String::new();
    for c in text.chars() {
        let w = c.width().unwrap_or(0);
        if used + w > room {
            break;
        }
        used += w;
        cut.push(c);
    }
    if width >= more.width() {
        cut.push_str(more);
    }
    cut
}

/// Splits text into rows of at most `width` columns, for scrollback.
pub fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut rows = vec![String::new()];
    let mut used = 0;
    for c in text.chars() {
        let w = c.width().unwrap_or(0);
        if used + w > width {
            rows.push(String::new());
            used = 0;
        }
        rows.last_mut().expect("one row").push(c);
        used += w;
    }
    rows
}

/// Scrollback lines as they will be drawn: wrapped to the width, styled.
pub fn scrollback(lines: &[Out], width: u16) -> Vec<Line<'static>> {
    lines
        .iter()
        .flat_map(|out| {
            let style = tone_style(out.tone);
            wrap(&out.text, width as usize)
                .into_iter()
                .map(move |row| Line::styled(row, style))
        })
        .collect()
}

pub struct Frame<'a> {
    /// The panel's jobs, each with its queue index.
    pub panel: &'a [Row<'a>],
    pub prompt: &'a str,
    /// Byte offset of the cursor in `prompt`.
    pub cursor: usize,
    pub hint: &'a str,
    pub glyphs: &'a Glyphs,
    /// Most panel rows to draw before summarizing the rest.
    pub max_rows: usize,
}

impl Frame<'_> {
    /// Rows the panel takes, including the "and N more" row.
    pub fn panel_rows(&self) -> usize {
        if self.panel.len() > self.max_rows {
            self.max_rows.max(1)
        } else {
            self.panel.len()
        }
    }

    /// The viewport height: panel, rule, prompt and hint.
    pub fn height(&self) -> u16 {
        (self.panel_rows() + 3) as u16
    }

    fn summary(&self) -> String {
        let count = |wanted: u8| self.panel.iter().filter(|row| row.group == wanted).count();
        let mut parts = Vec::new();
        let active = count(0);
        if active > 0 {
            let rate: u64 = self
                .panel
                .iter()
                .filter(|row| row.job.state == JobState::Running)
                .filter_map(|row| row.job.progress.rate_bytes_per_second)
                .sum();
            let rate = if rate > 0 {
                format!(" at {}/s", client::bytes(rate))
            } else {
                String::new()
            };
            parts.push(format!("{active} downloading{rate}"));
        }
        for (group, word) in [
            (1, "to check"),
            (2, "paused"),
            (3, "queued"),
            (4, "scheduled"),
        ] {
            let n = count(group);
            if n > 0 {
                parts.push(format!("{n} {word}"));
            }
        }
        if parts.is_empty() {
            "Nothing downloading".to_owned()
        } else {
            parts.join(" · ")
        }
    }

    /// Draws into `area` of `buffer` and returns where the cursor goes.
    pub fn render(&self, area: Rect, buffer: &mut Buffer) -> Position {
        let width = area.width as usize;
        let mut y = area.y;
        let bottom = area.bottom();
        let mut row = |line: Line, y: &mut u16| {
            if *y < bottom {
                buffer.set_line(area.x, *y, &line, area.width);
                *y += 1;
            }
        };
        let rows = self.panel_rows();
        let (shown, hidden) = if self.panel.len() > rows {
            (rows - 1, self.panel.len() - (rows - 1))
        } else {
            (self.panel.len(), 0)
        };
        for Row { index, group, job } in &self.panel[..shown] {
            let (glyph, glyph_style) = state_glyph(*group, self.glyphs);
            let number = format!("{index:>3} ");
            let detail = detail(job);
            let fixed = number.width() + glyph.width() + 1;
            let room = width.saturating_sub(fixed + detail.width() + 2);
            let name = fit(&client::name(job), room.max(8), self.glyphs.more);
            let gap = width.saturating_sub(fixed + name.width() + detail.width());
            let line = Line::from(vec![
                Span::styled(number, DIM),
                Span::styled(glyph, glyph_style),
                Span::raw(" "),
                Span::raw(name),
                Span::raw(" ".repeat(gap.max(2))),
                Span::styled(detail, DIM),
            ]);
            row(line, &mut y);
        }
        if hidden > 0 {
            let text = format!("    {} {hidden} more; /queue lists all", self.glyphs.more);
            row(
                Line::styled(fit(&text, width, self.glyphs.more), DIM),
                &mut y,
            );
        }
        let summary = format!(" {} ", self.summary());
        let lead = self.glyphs.rule.repeat(2);
        let tail = width.saturating_sub(lead.width() + summary.width());
        row(
            Line::styled(
                fit(
                    &format!("{lead}{summary}{}", self.glyphs.rule.repeat(tail)),
                    width,
                    "",
                ),
                DIM,
            ),
            &mut y,
        );

        // The prompt scrolls sideways to keep the cursor in view.
        let lead = format!("{} ", self.glyphs.prompt);
        let room = width.saturating_sub(lead.width() + 1).max(1);
        let before = &self.prompt[..self.cursor];
        let mut start = 0;
        while before[start..].width() > room {
            start += before[start..].chars().next().map_or(1, char::len_utf8);
        }
        let visible = fit(&self.prompt[start..], room, "");
        let cursor_x = area.x + (lead.width() + before[start..].width()) as u16;
        let prompt_y = y;
        row(
            Line::from(vec![
                Span::styled(lead, Style::new().add_modifier(Modifier::BOLD)),
                Span::raw(visible),
            ]),
            &mut y,
        );
        row(
            Line::styled(fit(self.hint, width, self.glyphs.more), DIM),
            &mut y,
        );
        Position::new(cursor_x.min(area.right().saturating_sub(1)), prompt_y)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::live::{Live, tests::recorded};
    use fetchpath_protocol::message::ServerMessage;

    fn text(buffer: &Buffer) -> Vec<String> {
        let area = buffer.area;
        (area.top()..area.bottom())
            .map(|y| {
                (area.left()..area.right())
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
            .collect()
    }

    fn draw(live: &Live, prompt: &str, width: u16, max_rows: usize) -> (Vec<String>, Position) {
        let panel = live.panel();
        let frame = Frame {
            panel: &panel,
            prompt,
            cursor: prompt.len(),
            hint: "Paste a link, or type / for commands",
            glyphs: &ASCII,
            max_rows,
        };
        let area = Rect::new(0, 0, width, frame.height());
        let mut buffer = Buffer::empty(area);
        let cursor = frame.render(area, &mut buffer);
        (text(&buffer), cursor)
    }

    /// Replays the recorded stream to its sixth progress sample, when one
    /// file is downloading and the other has just failed, and snapshots the
    /// rendered buffer.
    #[test]
    fn the_panel_renders_a_recorded_stream() {
        let mut live = Live::default();
        let messages = recorded();
        let mut samples = 0;
        for message in &messages {
            match message {
                ServerMessage::Event(event) => {
                    live.apply(event);
                }
                ServerMessage::Progress(sample) => {
                    live.progress(sample);
                    samples += 1;
                }
                ServerMessage::Reply(_) => {}
            }
            if samples == 6 {
                break;
            }
        }
        let (rows, cursor) = draw(&live, "/pause 2", 72, 6);
        assert_eq!(
            rows,
            [
                "  2 > sample.bin         65.5%  1.9 MiB of 2.9 MiB  1.4 MiB/s  0:01 left",
                "  1 ! missing.zip                                                 failed",
                "-- 1 downloading at 1.4 MiB/s · 1 to check -----------------------------",
                "> /pause 2",
                "Paste a link, or type / for commands",
            ],
        );
        assert_eq!(cursor, Position::new(10, 3));
    }

    #[test]
    fn a_long_queue_is_summarized_and_the_prompt_scrolls_to_the_cursor() {
        let jobs: Vec<JobSnapshot> = (0..5)
            .map(|n| {
                serde_json::from_value(serde_json::json!({
                    "job_id": format!("0000000{n}-0000-4000-8000-000000000000"),
                    "kind": "file", "state": "queued", "job_revision": 1, "last_seq": 1,
                    "source_display": format!("https://a.test/file-{n}.zip"),
                    "progress": { "bytes_received": 0 },
                    "created_at": "2026-09-26T10:00:00Z",
                }))
                .unwrap()
            })
            .collect();
        let live = Live::new(jobs);
        let long = format!("https://a.test/{}", "x".repeat(60));
        let (rows, cursor) = draw(&live, &long, 40, 3);
        assert_eq!(rows.len(), 6);
        // Queued jobs list the next to run (the oldest) first.
        assert_eq!(rows[0], "  5 . https://a.test/file-4.zip   queued");
        assert_eq!(rows[2], "    ... 3 more; /queue lists all");
        assert_eq!(rows[3], "-- 5 queued ----------------------------");
        assert!(rows[4].ends_with(&"x".repeat(20)), "{}", rows[4]);
        assert_eq!(cursor.y, 4);
        assert!(cursor.x < 40);
    }

    #[test]
    fn receipts_say_where_a_file_went_or_why_it_stopped() {
        let mut live = Live::default();
        for message in recorded() {
            match message {
                ServerMessage::Event(event) => {
                    live.apply(&event);
                }
                ServerMessage::Progress(sample) => live.progress(&sample),
                ServerMessage::Reply(_) => {}
            }
        }
        let saved = live
            .jobs()
            .iter()
            .find(|job| job.state == JobState::Completed)
            .unwrap();
        assert_eq!(
            receipt(saved, &ASCII).text,
            r"+ sample.bin  2.9 MiB  C:\Users\person\Downloads\sample.bin"
        );
        let mut failed = live.jobs()[0].clone();
        failed.retry_at = None;
        let out = receipt(&failed, &UNICODE);
        assert_eq!(out.tone, Tone::Bad);
        assert_eq!(
            out.text,
            "✗ missing.zip  source.transfer_failed: HTTP status 404  (/retry 4175930f)"
        );
    }

    #[test]
    fn wrapping_and_fitting_count_columns_not_bytes() {
        assert_eq!(wrap("abcdef", 4), ["abcd", "ef"]);
        assert_eq!(wrap("日本語", 4), ["日本", "語"]);
        assert_eq!(fit("abcdefgh", 5, "..."), "ab...");
        assert_eq!(fit("abc", 5, "..."), "abc");
    }
}
