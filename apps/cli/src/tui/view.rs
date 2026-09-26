//! Drawing the inline view: the live panel with a progress bar per
//! download, a summary rule, then either the prompt and a hint line or a
//! confirmation card; plus the one-line receipts printed into scrollback.

use super::line::{Out, Tone};
use super::live::Row;
use super::review::{self, Card, Stage};
use crate::client;
use fetchpath_protocol::JobSnapshot;
use fetchpath_protocol::model::{JobState, LinkKind};
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::symbols::border;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Widget};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Symbols for states, bars and receipts. The classic console's default
/// fonts lack most symbols, so ASCII is used there and in plain mode.
#[derive(Clone, Copy, Debug)]
pub struct Glyphs {
    pub saved: &'static str,
    pub failed: &'static str,
    pub cancelled: &'static str,
    /// Frames of the spinner shown beside a moving download.
    pub spinner: &'static [&'static str],
    pub paused: &'static str,
    pub waiting: &'static str,
    pub attention: &'static str,
    pub bar_full: &'static str,
    pub bar_empty: &'static str,
    pub rule: &'static str,
    pub prompt: &'static str,
    pub more: &'static str,
    pub chosen: &'static str,
    pub unchosen: &'static str,
    pub unicode: bool,
}

pub const UNICODE: Glyphs = Glyphs {
    saved: "✓",
    failed: "✗",
    cancelled: "–",
    spinner: &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"],
    paused: "‖",
    waiting: "·",
    attention: "!",
    bar_full: "█",
    bar_empty: "░",
    rule: "─",
    prompt: "›",
    more: "…",
    chosen: "●",
    unchosen: "○",
    unicode: true,
};

pub const ASCII: Glyphs = Glyphs {
    saved: "+",
    failed: "x",
    cancelled: "-",
    spinner: &["|", "/", "-", "\\"],
    paused: "=",
    waiting: ".",
    attention: "!",
    bar_full: "#",
    bar_empty: "-",
    rule: "-",
    prompt: ">",
    more: "...",
    chosen: "(*)",
    unchosen: "( )",
    unicode: false,
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
const ACCENT: Color = Color::Cyan;

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

/// The glyph and color for a panel row. A moving download spins.
fn state_glyph(job: &JobSnapshot, group: u8, glyphs: &Glyphs, tick: u64) -> (&'static str, Color) {
    match group {
        0 => {
            let frame = glyphs.spinner[(tick as usize) % glyphs.spinner.len()];
            let color = match job.state {
                JobState::Verifying | JobState::Publishing => Color::Magenta,
                _ => ACCENT,
            };
            (frame, color)
        }
        1 => (glyphs.attention, Color::Yellow),
        2 => (glyphs.paused, Color::Yellow),
        _ => (glyphs.waiting, Color::DarkGray),
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

/// The short text after a bar: speed and time left while running, the
/// state otherwise.
fn tail(job: &JobSnapshot) -> String {
    let progress = &job.progress;
    match job.state {
        JobState::Running => {
            let rate = progress
                .rate_bytes_per_second
                .map(|rate| format!("{}/s", client::bytes(rate)));
            let eta = progress.eta_seconds.map(client::duration);
            match (progress.bytes_total, rate, eta) {
                (Some(_), Some(rate), Some(eta)) => format!("{rate}  {eta}"),
                (Some(_), Some(rate), None) => rate,
                (None, Some(rate), _) => {
                    format!("{}  {rate}", client::bytes(progress.bytes_received))
                }
                _ => "starting".to_owned(),
            }
        }
        JobState::Queued if job.not_before.is_some() => format!(
            "starts {}",
            crate::when::local(job.not_before.expect("checked"))
        ),
        JobState::Failed if job.retry_at.is_some() => "retrying".to_owned(),
        _ => client::state_label(job).to_owned(),
    }
}

/// The fraction done, when the size is known.
fn fraction(job: &JobSnapshot) -> Option<f64> {
    match job.progress.bytes_total {
        Some(total) if total > 0 => {
            Some((job.progress.bytes_received as f64 / total as f64).clamp(0.0, 1.0))
        }
        _ => None,
    }
}

/// A bar `width` cells wide. Without a known size a moving download shows a
/// short block sliding along the track.
fn bar(
    job: &JobSnapshot,
    group: u8,
    width: usize,
    glyphs: &Glyphs,
    tick: u64,
) -> Vec<Span<'static>> {
    let color = match (group, job.state) {
        (0, JobState::Verifying | JobState::Publishing) => Color::Magenta,
        (0, _) => ACCENT,
        (1, _) => Color::Yellow,
        (2, _) => Color::Yellow,
        _ => Color::DarkGray,
    };
    let (start, filled) = match fraction(job) {
        Some(done) => (0, (done * width as f64).round() as usize),
        None if group == 0 && width > 4 => {
            let span = (width / 5).max(2);
            let travel = width - span + 1;
            ((tick as usize / 2) % travel, span)
        }
        None => (0, 0),
    };
    let end = (start + filled).min(width);
    vec![
        Span::styled(glyphs.bar_empty.repeat(start), DIM),
        Span::styled(glyphs.bar_full.repeat(end - start), Style::new().fg(color)),
        Span::styled(glyphs.bar_empty.repeat(width - end), DIM),
    ]
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

/// `text` cut or padded to exactly `width` columns.
fn pad(text: &str, width: usize, more: &str) -> String {
    let cut = fit(text, width, more);
    let used = cut.width();
    format!("{cut}{}", " ".repeat(width.saturating_sub(used)))
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

/// Most format rows a card lists before scrolling.
const CARD_ROWS: usize = 6;

/// A card's title, body and key line, shared by the inline card and plain
/// mode.
pub struct CardText {
    pub title: String,
    pub body: Vec<Line<'static>>,
    pub keys: String,
}

/// Keeps the start and the end of `text` within `width` columns, which
/// suits paths: the drive and the file name stay visible.
fn middle_fit(text: &str, width: usize, more: &str) -> String {
    if text.width() <= width {
        return text.to_owned();
    }
    let keep = width.saturating_sub(more.width());
    let head = keep / 3;
    let mut tail_room = keep - head;
    let start: String = fit(text, head, "");
    let mut end: Vec<char> = Vec::new();
    for c in text.chars().rev() {
        let w = c.width().unwrap_or(0);
        if w > tail_room {
            break;
        }
        tail_room -= w;
        end.push(c);
    }
    end.reverse();
    format!("{start}{more}{}", end.into_iter().collect::<String>())
}

/// A message wrapped to the card's width, one styled line per row.
fn note(text: &str, style: Style, width: usize) -> Vec<Line<'static>> {
    wrap(text, width)
        .into_iter()
        .map(|row| Line::styled(row, style))
        .collect()
}

/// Describes a card: what the link is and what Enter will do. `width` is
/// the room for text inside the frame.
pub fn card_text(card: &Card, glyphs: &Glyphs, tick: u64, width: usize) -> CardText {
    let site = review::host(&card.draft.link);
    let warn = Style::new().fg(Color::Yellow);
    let save = |card: &Card| -> Line<'static> {
        let place = card
            .destination()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|message| message);
        let label = "Save as  ";
        Line::from(vec![
            Span::styled(label, DIM),
            Span::raw(middle_fit(
                &place,
                width.saturating_sub(label.len()),
                glyphs.more,
            )),
        ])
    };
    let starting = card
        .draft
        .at
        .map(|at| format!(" · starts {}", crate::when::local(at)))
        .unwrap_or_default();
    match &card.stage {
        Stage::Looking => CardText {
            title: format!("Link · {site}"),
            body: vec![Line::from(vec![
                Span::styled(
                    glyphs.spinner[(tick as usize) % glyphs.spinner.len()],
                    Style::new().fg(ACCENT),
                ),
                Span::raw(" Looking at the link…".replace('…', glyphs.more)),
            ])],
            keys: "Esc cancel".into(),
        },
        Stage::Failed(message) => CardText {
            title: format!("Link · {site}"),
            body: {
                let mut body = note(
                    &format!("Could not look at this link: {message}"),
                    warn,
                    width,
                );
                body.push(save(card));
                body
            },
            keys: format!("Enter download anyway{starting} · Esc cancel"),
        },
        Stage::Ready(look) => {
            if let Some(media) = &look.media {
                let variants = card.variants();
                let length = media
                    .duration_seconds
                    .map(|seconds| format!("  ·  {}", client::duration(seconds as u64)))
                    .unwrap_or_default();
                let title: String = media.title.chars().filter(|c| !c.is_control()).collect();
                let title = fit(&title, width.saturating_sub(length.width()), glyphs.more);
                let mut body = vec![Line::from(vec![
                    Span::styled(title, Style::new().add_modifier(Modifier::BOLD)),
                    Span::styled(length, DIM),
                ])];
                let first = card
                    .choice
                    .saturating_sub(CARD_ROWS - 1)
                    .min(variants.len().saturating_sub(CARD_ROWS));
                for (row, variant) in variants.iter().enumerate().skip(first).take(CARD_ROWS) {
                    let chosen = row == card.choice;
                    let mark = if chosen {
                        glyphs.chosen
                    } else {
                        glyphs.unchosen
                    };
                    let style = if chosen {
                        Style::new().fg(ACCENT).add_modifier(Modifier::BOLD)
                    } else {
                        Style::new()
                    };
                    let best = if row == card.recommended() {
                        "  recommended"
                    } else {
                        ""
                    };
                    body.push(Line::from(vec![
                        Span::styled(format!("  {mark} "), style),
                        Span::styled(format!("{:>2}  ", row + 1), DIM),
                        Span::styled(review::variant_text(variant), style),
                        Span::styled(best, DIM),
                    ]));
                }
                if variants.len() > CARD_ROWS {
                    body.push(Line::styled(
                        format!("       {} of {} formats", card.choice + 1, variants.len()),
                        DIM,
                    ));
                }
                body.push(save(card));
                let kind = if look.link.kind == LinkKind::MediaPage {
                    "Video"
                } else {
                    "Video on a page"
                };
                CardText {
                    title: format!("{kind} · {site}"),
                    body,
                    keys: format!("↑↓ choose · Enter download{starting} · Esc cancel")
                        .replace("↑↓", if glyphs.unicode { "↑↓" } else { "Up/Down" }),
                }
            } else if look.link.kind == LinkKind::MediaPage {
                CardText {
                    title: format!("Video · {site}"),
                    body: note(
                        &format!(
                            "This is a video page, but its formats could not be read: {}",
                            look.media_error.as_deref().unwrap_or("no formats")
                        ),
                        warn,
                        width,
                    ),
                    keys: "Esc close".into(),
                }
            } else {
                let name = card.file_name();
                let kind = review::kind_label(look.link.content_type.as_deref(), &name);
                let size = look
                    .link
                    .size_bytes
                    .map_or("size unknown".to_owned(), client::bytes);
                let resume = if look.link.resumable {
                    "can resume"
                } else {
                    "cannot resume"
                };
                let mut body = vec![
                    Line::styled(
                        middle_fit(&name, width, glyphs.more),
                        Style::new().add_modifier(Modifier::BOLD),
                    ),
                    Line::styled(format!("{kind} · {size} · {resume}"), DIM),
                ];
                if look.link.kind == LinkKind::WebPage {
                    body.extend(note(
                        "This link is a web page, not a file. It holds no video Fetchpath can read.",
                        warn,
                        width,
                    ));
                }
                body.push(save(card));
                let (title, enter) = if look.link.kind == LinkKind::WebPage {
                    ("Web page", "Enter save the page")
                } else {
                    ("File", "Enter download")
                };
                CardText {
                    title: format!("{title} · {site}"),
                    body,
                    keys: format!("{enter}{starting} · Esc cancel"),
                }
            }
        }
    }
}

/// Text room inside a card's frame: borders and a space of padding on each
/// side.
fn card_room(width: u16) -> usize {
    (width as usize).saturating_sub(4).max(10)
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
    /// A link being confirmed; it takes the prompt's place.
    pub card: Option<&'a Card>,
    /// Animation step for spinners and sliding bars.
    pub tick: u64,
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

    /// The viewport height at a width: panel, rule, then prompt and hint or
    /// the card.
    pub fn height(&self, width: u16) -> u16 {
        let below = match self.card {
            Some(card) => card_text(card, self.glyphs, 0, card_room(width)).body.len() + 2,
            None => 2,
        };
        (self.panel_rows() + 1 + below) as u16
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

    /// One panel row: number, glyph, name, bar, percent and tail. Narrow
    /// windows drop the bar first.
    fn job_line(&self, row: &Row, width: usize) -> Line<'static> {
        let Row { index, group, job } = *row;
        let glyphs = self.glyphs;
        let (glyph, color) = state_glyph(job, group, glyphs, self.tick);
        let number = format!("{index:>3} ");
        let percent = fraction(job).map_or(String::new(), |done| format!("{:.0}%", done * 100.0));
        let tail = tail(job);
        let lead = number.width() + glyph.width() + 1;
        // Wide windows align every row's tail; narrow ones spend no spare room.
        let tail_width = if width >= 60 {
            18.max(tail.width()).min(width / 3)
        } else {
            tail.width().min(width / 3)
        };
        let mut spans = vec![
            Span::styled(number, DIM),
            Span::styled(glyph, Style::new().fg(color)),
            Span::raw(" "),
        ];
        let room = width.saturating_sub(lead + 5 + tail_width + 2);
        let name_style = if group == 0 {
            Style::new().add_modifier(Modifier::BOLD)
        } else {
            Style::new()
        };
        if room >= 24 {
            let bar_width = (room * 2 / 5).clamp(8, 32);
            let name_width = room - bar_width - 1;
            spans.push(Span::styled(
                pad(&client::name(job), name_width, glyphs.more),
                name_style,
            ));
            spans.push(Span::raw(" "));
            spans.extend(bar(job, group, bar_width, glyphs, self.tick));
        } else {
            let name_width = width.saturating_sub(lead + 5 + tail_width + 2).max(6);
            spans.push(Span::styled(
                pad(&client::name(job), name_width, glyphs.more),
                name_style,
            ));
        }
        spans.push(Span::styled(
            format!(" {percent:>4}"),
            Style::new().fg(color),
        ));
        spans.push(Span::raw("  "));
        spans.push(Span::styled(fit(&tail, tail_width, glyphs.more), DIM));
        Line::from(spans)
    }

    /// Draws into `area` of `buffer` and returns where the cursor goes, if
    /// anywhere.
    pub fn render(&self, area: Rect, buffer: &mut Buffer) -> Option<Position> {
        let width = area.width as usize;
        let mut y = area.y;
        let bottom = area.bottom();
        let row = |line: Line, y: &mut u16, buffer: &mut Buffer| {
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
        for job_row in &self.panel[..shown] {
            row(self.job_line(job_row, width), &mut y, buffer);
        }
        if hidden > 0 {
            let text = format!("    {} {hidden} more; /queue lists all", self.glyphs.more);
            row(
                Line::styled(fit(&text, width, self.glyphs.more), DIM),
                &mut y,
                buffer,
            );
        }
        let summary = format!(" {} ", self.summary());
        let lead = self.glyphs.rule.repeat(2);
        let rest = width.saturating_sub(lead.width() + summary.width());
        row(
            Line::from(vec![
                Span::styled(lead, DIM),
                Span::styled(summary, Style::new().fg(ACCENT)),
                Span::styled(self.glyphs.rule.repeat(rest), DIM),
            ]),
            &mut y,
            buffer,
        );

        if let Some(card) = self.card {
            let text = card_text(card, self.glyphs, self.tick, card_room(area.width));
            let height = (text.body.len() + 2) as u16;
            let block_area = Rect::new(area.x, y, area.width, height.min(bottom.saturating_sub(y)));
            let block = Block::bordered()
                .border_style(Style::new().fg(ACCENT))
                .title(Span::styled(
                    format!(" {} ", text.title),
                    Style::new().fg(ACCENT).add_modifier(Modifier::BOLD),
                ))
                .title_bottom(Span::styled(format!(" {} ", text.keys), DIM));
            let block = if self.glyphs.unicode {
                block.border_type(BorderType::Rounded)
            } else {
                block.border_set(border::Set {
                    top_left: "+",
                    top_right: "+",
                    bottom_left: "+",
                    bottom_right: "+",
                    vertical_left: "|",
                    vertical_right: "|",
                    horizontal_top: "-",
                    horizontal_bottom: "-",
                })
            };
            let inner = block.inner(block_area);
            block.render(block_area, buffer);
            for (offset, line) in text.body.iter().enumerate() {
                let line_y = inner.y + offset as u16;
                if line_y < inner.bottom() {
                    buffer.set_line(inner.x + 1, line_y, line, inner.width.saturating_sub(1));
                }
            }
            return None;
        }

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
                Span::styled(lead, Style::new().fg(ACCENT).add_modifier(Modifier::BOLD)),
                Span::raw(visible),
            ]),
            &mut y,
            buffer,
        );
        row(
            Line::styled(fit(self.hint, width, self.glyphs.more), DIM),
            &mut y,
            buffer,
        );
        Some(Position::new(
            cursor_x.min(area.right().saturating_sub(1)),
            prompt_y,
        ))
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
            card: None,
            tick: 0,
        };
        let area = Rect::new(0, 0, width, frame.height(width));
        let mut buffer = Buffer::empty(area);
        let cursor = frame
            .render(area, &mut buffer)
            .expect("the prompt has a cursor");
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
                "  2 | sample.bin               ##########------  66%  1.4 MiB/s  0:01",
                "  1 ! missing.zip              ----------------       failed",
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
        assert_eq!(rows[0], "  5 . https://a.test/fil...       queued");
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
        // A long path keeps its drive and its file name.
        let path = r"C:\Users\person\AppData\Local\Somewhere\Deep\Downloads\ubuntu.iso";
        let fitted = middle_fit(path, 30, "...");
        assert_eq!(fitted.width(), 30);
        assert!(
            fitted.starts_with(r"C:\Users\") && fitted.ends_with(r"\ubuntu.iso"),
            "{fitted}"
        );
    }

    #[test]
    fn a_media_card_shows_the_formats_and_where_the_file_goes() {
        use crate::tui::review::{Card, Draft, Look};
        use fetchpath_protocol::model::{
            LinkInspection, MediaInspection, MediaVariant, MediaVariantKind,
        };
        let link = "https://www.youtube.com/watch?v=x";
        let mut card = Card::new(Draft {
            link: link.into(),
            url: fetchpath_protocol::SensitiveUrl::try_from(link.to_owned()).unwrap(),
            to: Some(r"C:\Users\person\Downloads\".into()),
            at: None,
            sha256: None,
            quality: None,
        });
        let variant = |id: &str, label: &str, height| MediaVariant {
            id: id.into(),
            label: label.into(),
            kind: MediaVariantKind::Video,
            extension: "mp4".into(),
            height,
            fps: None,
        };
        card.set(Ok(Look {
            link: LinkInspection {
                kind: LinkKind::MediaPage,
                file_name: None,
                content_type: None,
                size_bytes: None,
                resumable: false,
            },
            media: Some(MediaInspection {
                title: "Big Buck Bunny".into(),
                duration_seconds: Some(596.0),
                variants: vec![
                    variant("22", "720p", Some(720)),
                    variant("137", "1080p", Some(1080)),
                ],
            }),
            media_error: None,
            folder: None,
        }));
        let live = Live::default();
        let panel = live.panel();
        let frame = Frame {
            panel: &panel,
            prompt: "",
            cursor: 0,
            hint: "",
            glyphs: &ASCII,
            max_rows: 4,
            card: Some(&card),
            tick: 0,
        };
        let area = Rect::new(0, 0, 60, frame.height(60));
        let mut buffer = Buffer::empty(area);
        assert!(frame.render(area, &mut buffer).is_none());
        assert_eq!(
            text(&buffer),
            [
                "-- Nothing downloading -------------------------------------",
                "+ Video · youtube.com -------------------------------------+",
                "| Big Buck Bunny  ·  9:56                                  |",
                "|   (*)  1  1080p  mp4  recommended                        |",
                "|   ( )  2  720p  mp4                                      |",
                r"| Save as  C:\Users\person\Downloads\Big Buck Bunny.mp4    |",
                "+ Up/Down choose · Enter download · Esc cancel ------------+",
            ]
        );
    }
}
