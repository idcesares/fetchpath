//! Drawing the inline view: the live panel with a progress bar per
//! download, a summary rule, then either the prompt and a hint line or a
//! confirmation card; plus the one-line receipts printed into scrollback.

use super::flows::Flows;
use super::flows::batch::Batch;
use super::line::{Out, Tone};
use super::live::Row;
use super::menu::{Menu, ROWS};
use super::prompt::Prompt;
use super::review::{self, Card, Field, Stage};
use super::setup::Setup;
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
    pub bar_full: &'static str,
    pub bar_empty: &'static str,
    pub rule: &'static str,
    /// The line between the dashboard's queue and details.
    pub divider: &'static str,
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
    bar_full: "█",
    bar_empty: "░",
    rule: "─",
    divider: "│",
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
    bar_full: "#",
    bar_empty: "-",
    rule: "-",
    divider: "|",
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

pub const DIM: Style = Style::new().fg(Color::DarkGray);
pub const ACCENT: Color = Color::Cyan;

pub fn tone_style(tone: Tone) -> Style {
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
        _ if super::flows::conflicted(job) => Out::new(
            Tone::Bad,
            format!(
                "{} {name}  a file with this name is already there; nothing was replaced  (/rename {} NAME)",
                glyphs.failed,
                client::short_id(job)
            ),
        ),
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

/// The group of a finished job listed beside the panel's (the dashboard
/// lists recent finished ones too).
pub const FINISHED: u8 = 6;

/// The bracketed cue and color for a panel row. The cue is plain ASCII in
/// every glyph set, so a state never rests on color alone: `[>]` running,
/// `[~]` queued, `[=]` paused, `[!]` failed, `[v]` done, `[?]` needs a
/// person (an approval, a choice, a link).
fn state_glyph(job: &JobSnapshot, group: u8) -> (&'static str, Color) {
    match (group, job.state) {
        (0, JobState::Verifying | JobState::Publishing) => ("[>]", Color::Magenta),
        (0, _) => ("[>]", ACCENT),
        (_, JobState::Failed) => ("[!]", Color::Red),
        (1, _) => ("[?]", Color::Magenta),
        (2, _) => ("[=]", Color::DarkGray),
        (FINISHED, JobState::Completed) => ("[v]", Color::Green),
        (FINISHED, JobState::Cancelled) => ("[-]", Color::DarkGray),
        (3 | 4, _) => ("[~]", Color::Yellow),
        _ => ("[?]", Color::DarkGray),
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
        JobState::Completed if !job.reused_from_cache && job.from_paired_device.is_none() => {
            "done".to_owned()
        }
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
        (FINISHED, JobState::Completed) => Color::Green,
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
pub fn pad(text: &str, width: usize, more: &str) -> String {
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
pub fn middle_fit(text: &str, width: usize, more: &str) -> String {
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
pub fn note(text: &str, style: Style, width: usize) -> Vec<Line<'static>> {
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
            let mut text = ready_text(card, look, glyphs, width, &site, &starting, warn, &save);
            // Which rule decides, and why (FP-064).
            if let Some(line) = crate::rules::card_line(look.link.rules.as_ref()) {
                text.body.extend(note(&line, DIM, width));
            }
            if let Some(clash) = card.clash() {
                text.body.extend(note(
                    &format!("{clash} is already there, so this one gets the next free name; nothing is replaced."),
                    warn,
                    width,
                ));
            }
            if let Some(hex) = &card.draft.sha256 {
                text.body.push(Line::from(vec![
                    Span::styled("SHA-256  ", DIM),
                    Span::raw(middle_fit(hex, width.saturating_sub(9), glyphs.more)),
                ]));
            }
            if card.needs_checksum() {
                text.body.extend(note(
                    "That rule needs a checksum for this file: press S and paste its SHA-256.",
                    warn,
                    width,
                ));
                text.keys = "S enter checksum · Esc cancel".into();
            } else if card.takes_checksum() && card.can_confirm() {
                text.keys = text
                    .keys
                    .replace(" · Esc cancel", " · S checksum · N name · Esc cancel");
            }
            if let Some(problem) = &card.problem {
                text.body
                    .extend(note(problem, Style::new().fg(Color::Red), width));
            }
            text
        }
    }
}

/// The card for a link the engine has looked at.
#[allow(clippy::too_many_arguments)]
fn ready_text(
    card: &Card,
    look: &review::Look,
    glyphs: &Glyphs,
    width: usize,
    site: &str,
    starting: &str,
    warn: Style,
    save: &dyn Fn(&Card) -> Line<'static>,
) -> CardText {
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
    } else if look.link.kind == LinkKind::MediaPage && look.tools_missing {
        let mut body = note(
            "Saving video needs two free programs that are not part of Fetchpath:",
            warn,
            width,
        );
        body.extend(tool_notes(width));
        body.extend(note(
                    "Press I to see what will be downloaded and set them up; this video will be looked at again afterwards.",
                    DIM,
                    width,
                ));
        CardText {
            title: format!("Video · {site}"),
            body,
            keys: "I set up video tools · Esc cancel".into(),
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
        if look.link.kind == LinkKind::WebPage && look.tools_missing {
            body.extend(note(
                        "This link is a web page, not a file. If it holds a video, Fetchpath needs its video tools to read it: press I to set them up.",
                        warn,
                        width,
                    ));
        } else if look.link.kind == LinkKind::WebPage {
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
        let tools = if look.tools_missing {
            " · I set up video tools"
        } else {
            ""
        };
        CardText {
            title: format!("{title} · {site}"),
            body,
            keys: format!("{enter}{starting}{tools} · Esc cancel"),
        }
    }
}

/// The helpers a setup would fetch, one wrapped line each.
fn tool_notes(width: usize) -> Vec<Line<'static>> {
    crate::tools::tool_lines(&fetchpath_media::setup::pinned_tools())
        .iter()
        .flat_map(|line| note(&format!("  {line}"), Style::new(), width))
        .collect()
}

/// The setup card: what will be downloaded before the person agrees, then
/// the download's progress.
pub fn setup_text(setup: &Setup, glyphs: &Glyphs, tick: u64, width: usize) -> CardText {
    let title = "Set up video tools".to_owned();
    if !setup.is_running() {
        let mut body = Vec::new();
        for line in crate::tools::disclosure(&setup.dir) {
            let style = if line.starts_with("  ") {
                Style::new()
            } else {
                DIM
            };
            body.extend(note(&line, style, width));
        }
        return CardText {
            title,
            body,
            keys: "Enter download and set up · Esc cancel".into(),
        };
    }
    let spinner = glyphs.spinner[(tick as usize) % glyphs.spinner.len()];
    let mut body = Vec::new();
    match setup.current() {
        Some((label, received, total)) => {
            body.push(Line::from(vec![
                Span::styled(spinner, Style::new().fg(ACCENT)),
                Span::raw(format!(" Downloading {label}")),
            ]));
            let amount = match total {
                Some(total) if total > 0 => format!(
                    " {:>3.0}%  {} of {}",
                    received as f64 / total as f64 * 100.0,
                    client::bytes(received),
                    client::bytes(total)
                ),
                _ => format!(" {}", client::bytes(received)),
            };
            let bar_width = width.saturating_sub(amount.width() + 1).clamp(4, 40);
            let filled = total
                .filter(|total| *total > 0)
                .map_or(0, |total| {
                    ((received as f64 / total as f64) * bar_width as f64).round() as usize
                })
                .min(bar_width);
            body.push(Line::from(vec![
                Span::styled(glyphs.bar_full.repeat(filled), Style::new().fg(ACCENT)),
                Span::styled(glyphs.bar_empty.repeat(bar_width - filled), DIM),
                Span::styled(amount, DIM),
            ]));
        }
        None => body.push(Line::from(vec![
            Span::styled(spinner, Style::new().fg(ACCENT)),
            Span::raw(" Checking what is already there".to_owned() + glyphs.more),
        ])),
    }
    body.extend(note(
        "Each download is checked against its recorded SHA-256 before it is installed.",
        DIM,
        width,
    ));
    CardText {
        title,
        body,
        keys: "Esc cancel".into(),
    }
}

/// What sits below the panel instead of the prompt.
#[derive(Clone, Copy)]
pub enum Overlay<'a> {
    Link(&'a Card),
    Setup(&'a Setup),
    /// A menu, with a value being typed into one of its rows.
    Menu(&'a Menu, Option<(&'a str, &'a Prompt)>),
    /// Several links previewed together.
    Batch(&'a Batch),
    /// A prompt from the queue: an existing file, an agent's request.
    Flow(&'a Flows),
}

/// Something a click can land on.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Target {
    /// A menu item.
    Item(usize),
    /// A format on a video card.
    Choice(usize),
    /// A command in the list under the prompt.
    Command(usize),
}

/// What a frame drew: where the cursor goes and which rows are clickable.
#[derive(Debug, Default)]
pub struct Drawn {
    pub cursor: Option<Position>,
    pub targets: Vec<(u16, Target)>,
}

impl Drawn {
    pub fn target_at(&self, row: u16) -> Option<Target> {
        self.targets
            .iter()
            .find(|(y, _)| *y == row)
            .map(|(_, target)| *target)
    }
}

/// A card's text plus which body lines are clickable, and where a typing
/// cursor sits (body line, column).
pub struct OverlayText {
    pub card: CardText,
    pub targets: Vec<(usize, Target)>,
    pub cursor: Option<(usize, usize)>,
}

/// A line being typed on a card, scrolled to keep the cursor in view, and
/// the cursor's column.
pub fn edit_line(label: &str, prompt: &Prompt, width: usize) -> (Line<'static>, usize) {
    let lead = format!("{label}: ");
    let room = width.saturating_sub(lead.width() + 1).max(1);
    let before = &prompt.text()[..prompt.cursor()];
    let mut start = 0;
    while before[start..].width() > room {
        start += before[start..].chars().next().map_or(1, char::len_utf8);
    }
    let column = lead.width() + before[start..].width();
    (
        Line::from(vec![
            Span::styled(lead, Style::new().fg(ACCENT)),
            Span::raw(fit(&prompt.text()[start..], room, "")),
        ]),
        column,
    )
}

/// The body lines of a video card that hold formats, with their choice.
fn choice_lines(card: &Card) -> Vec<(usize, Target)> {
    let count = card.variants().len();
    if count == 0 {
        return Vec::new();
    }
    let first = card
        .choice
        .saturating_sub(CARD_ROWS - 1)
        .min(count.saturating_sub(CARD_ROWS));
    // Line 0 is the title; formats follow.
    (first..count.min(first + CARD_ROWS))
        .enumerate()
        .map(|(line, choice)| (line + 1, Target::Choice(choice)))
        .collect()
}

/// A menu as card text: one line per visible item, the selected one marked
/// and highlighted, then a hint line (or the value being typed).
fn menu_text(
    menu: &Menu,
    editing: Option<(&str, &Prompt)>,
    glyphs: &Glyphs,
    width: usize,
) -> OverlayText {
    let label_width = menu
        .items
        .iter()
        .filter(|item| !item.inert)
        .map(|item| item.label.width())
        .max()
        .unwrap_or(0)
        .min(width * 3 / 5);
    let mut body = Vec::new();
    let mut targets = Vec::new();
    for (index, item) in menu.visible() {
        if item.inert {
            body.push(Line::styled(
                item.label.clone(),
                Style::new().fg(ACCENT).add_modifier(Modifier::BOLD),
            ));
            continue;
        }
        let selected = index == menu.selected;
        let mark = if selected { glyphs.prompt } else { " " };
        let label = pad(&item.label, label_width, glyphs.more);
        let room = width.saturating_sub(label_width + 5);
        let value = fit(&item.value, room, glyphs.more);
        let style = if selected {
            Style::new()
                .fg(ACCENT)
                .add_modifier(Modifier::BOLD | Modifier::REVERSED)
        } else {
            Style::new()
        };
        targets.push((body.len(), Target::Item(index)));
        body.push(Line::from(vec![
            Span::styled(format!("{mark} "), Style::new().fg(ACCENT)),
            Span::styled(format!("{label}  {value}"), style),
        ]));
    }
    let mut cursor = None;
    match editing {
        Some((label, prompt)) => {
            let (line, column) = edit_line(label, prompt, width);
            cursor = Some((body.len(), column));
            body.push(line);
        }
        None => {
            let position = if menu.items.len() > ROWS {
                format!("  ({} of {})", menu.selected + 1, menu.items.len())
            } else {
                String::new()
            };
            body.extend(note(&format!("{}{position}", menu.hint()), DIM, width));
        }
    }
    let keys = if editing.is_some() {
        "Enter save · Tab complete · Esc keep".to_owned()
    } else if glyphs.unicode {
        menu.keys.clone()
    } else {
        menu.keys
            .replace("↑↓", "Up/Down")
            .replace("←→", "Left/Right")
    };
    OverlayText {
        card: CardText {
            title: menu.title.clone(),
            body,
            keys,
        },
        targets,
        cursor,
    }
}

fn overlay_text(overlay: Overlay, glyphs: &Glyphs, tick: u64, width: usize) -> OverlayText {
    match overlay {
        Overlay::Link(card) => {
            let mut text = card_text(card, glyphs, tick, width);
            let mut cursor = None;
            if let Some((field, prompt)) = &card.editing {
                let label = match field {
                    Field::Checksum => "SHA-256",
                    Field::Name => "Name",
                };
                let (line, column) = edit_line(label, prompt, width);
                cursor = Some((text.body.len(), column));
                text.body.push(line);
                text.keys = "Enter save · Esc back".into();
            }
            OverlayText {
                card: text,
                targets: choice_lines(card),
                cursor,
            }
        }
        Overlay::Batch(batch) => batch.text(glyphs, tick, width),
        Overlay::Flow(flows) => flows.text(glyphs, width).unwrap_or(OverlayText {
            card: CardText {
                title: String::new(),
                body: Vec::new(),
                keys: String::new(),
            },
            targets: Vec::new(),
            cursor: None,
        }),
        Overlay::Setup(setup) => OverlayText {
            card: setup_text(setup, glyphs, tick, width),
            targets: Vec::new(),
            cursor: None,
        },
        Overlay::Menu(menu, editing) => menu_text(menu, editing, glyphs, width),
    }
}

/// Text room inside a card's frame: borders and a space of padding on each
/// side.
fn card_room(width: u16) -> usize {
    (width as usize).saturating_sub(4).max(10)
}

/// The rule's words: how many downloads are in each group, with the
/// combined speed.
pub fn summary(panel: &[Row]) -> String {
    let count = |wanted: u8| panel.iter().filter(|row| row.group == wanted).count();
    let mut parts = Vec::new();
    let active = count(0);
    if active > 0 {
        let rate: u64 = panel
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
pub fn job_line(row: &Row, width: usize, glyphs: &Glyphs, tick: u64) -> Line<'static> {
    let Row { index, group, job } = *row;
    let (glyph, color) = state_glyph(job, group);
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
        spans.extend(bar(job, group, bar_width, glyphs, tick));
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
    pub card: Option<Overlay<'a>>,
    /// Animation step for spinners and sliding bars.
    pub tick: u64,
    /// Commands matching what is typed after `/`, shown under the prompt:
    /// name and summary.
    pub palette: &'a [(String, String)],
    pub palette_selected: usize,
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
            Some(card) => {
                overlay_text(card, self.glyphs, 0, card_room(width))
                    .card
                    .body
                    .len()
                    + 2
            }
            // A compact terminal gives no hint line to an empty prompt.
            None => 1 + self.palette.len().max(usize::from(!self.hint.is_empty())),
        };
        (self.panel_rows() + 1 + below) as u16
    }

    /// Draws into `area` of `buffer` and returns where the cursor goes, if
    /// anywhere.
    pub fn render(&self, area: Rect, buffer: &mut Buffer) -> Drawn {
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
            row(
                job_line(job_row, width, self.glyphs, self.tick),
                &mut y,
                buffer,
            );
        }
        if hidden > 0 {
            let text = format!("    {} {hidden} more; /queue lists all", self.glyphs.more);
            row(
                Line::styled(fit(&text, width, self.glyphs.more), DIM),
                &mut y,
                buffer,
            );
        }
        let summary = format!(" {} ", summary(self.panel));
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
            let OverlayText {
                card: text,
                targets,
                cursor,
            } = overlay_text(card, self.glyphs, self.tick, card_room(area.width));
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
            return Drawn {
                cursor: cursor.map(|(line, column)| {
                    Position::new(
                        (inner.x + 1 + column as u16).min(inner.right().saturating_sub(1)),
                        inner.y + line as u16,
                    )
                }),
                targets: targets
                    .into_iter()
                    .map(|(line, target)| (inner.y + line as u16, target))
                    .collect(),
            };
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
        let mut targets = Vec::new();
        if self.palette.is_empty() && !self.hint.is_empty() {
            row(
                Line::styled(fit(self.hint, width, self.glyphs.more), DIM),
                &mut y,
                buffer,
            );
        }
        let name_width = self
            .palette
            .iter()
            .map(|(name, _)| name.width() + 1)
            .max()
            .unwrap_or(0);
        for (index, (name, summary)) in self.palette.iter().enumerate() {
            let selected = index == self.palette_selected;
            let style = if selected {
                Style::new()
                    .fg(ACCENT)
                    .add_modifier(Modifier::BOLD | Modifier::REVERSED)
            } else {
                Style::new().fg(ACCENT)
            };
            targets.push((y, Target::Command(index)));
            let name = pad(&format!("/{name}"), name_width, "");
            let summary = fit(
                summary,
                width.saturating_sub(name_width + 6),
                self.glyphs.more,
            );
            row(
                Line::from(vec![
                    Span::raw("  "),
                    Span::styled(name, style),
                    Span::raw("  "),
                    Span::styled(summary, if selected { Style::new() } else { DIM }),
                ]),
                &mut y,
                buffer,
            );
        }
        let cursor = Some(Position::new(
            cursor_x.min(area.right().saturating_sub(1)),
            prompt_y,
        ));
        Drawn { cursor, targets }
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
            palette: &[],
            palette_selected: 0,
        };
        let area = Rect::new(0, 0, width, frame.height(width));
        let mut buffer = Buffer::empty(area);
        let cursor = frame
            .render(area, &mut buffer)
            .cursor
            .expect("the prompt has a cursor");
        (text(&buffer), cursor)
    }

    /// Replays the recorded stream to its sixth progress sample, when one
    /// file is downloading and the other has just failed, and snapshots the
    /// rendered buffer.
    #[test]
    fn every_status_has_a_bracketed_cue_and_a_word_without_color() {
        for (state, group, cue, word) in [
            ("running", 0, "[>]", "starting"),
            ("queued", 3, "[~]", "queued"),
            ("paused", 2, "[=]", "paused"),
            ("failed", 1, "[!]", "failed"),
            ("completed", FINISHED, "[v]", "done"),
            ("awaiting_approval", 1, "[?]", "needs approval"),
        ] {
            let job: JobSnapshot = serde_json::from_value(serde_json::json!({
                "job_id": "00000000-0000-4000-8000-000000000000",
                "kind": "file", "state": state, "job_revision": 1, "last_seq": 1,
                "source_display": "https://a.test/f.zip",
                "progress": { "bytes_received": 0 },
                "created_at": "2026-09-26T10:00:00Z",
            }))
            .unwrap();
            let line = job_line(
                &Row {
                    index: 1,
                    group,
                    job: &job,
                },
                60,
                &ASCII,
                0,
            );
            let plain: String = line
                .spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect();
            assert!(plain.contains(cue), "{plain}");
            assert!(plain.contains(word), "{plain}");
        }
    }

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
                "  2 [>] sample.bin              ##########-----  66%  1.4 MiB/s  0:01",
                "  1 [!] missing.zip             ---------------       failed",
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
        assert_eq!(rows[0], "  5 [~] https://a.test/f...       queued");
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
                rules: None,
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
            tools_missing: false,
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
            card: Some(Overlay::Link(&card)),
            tick: 0,
            palette: &[],
            palette_selected: 0,
        };
        let area = Rect::new(0, 0, 60, frame.height(60));
        let mut buffer = Buffer::empty(area);
        assert!(frame.render(area, &mut buffer).cursor.is_none());
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
    #[test]
    fn the_setup_card_says_what_is_downloaded_before_asking() {
        let setup = crate::tui::setup::Setup::new(None).unwrap();
        let text = setup_text(&setup, &ASCII, 0, 76);
        let body: Vec<String> = text
            .body
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect()
            })
            .collect();
        let joined = body.join(
            "
",
        );
        for tool in fetchpath_media::setup::pinned_tools() {
            assert!(
                joined.contains(&tool.name) && joined.contains(&tool.license),
                "{joined}"
            );
        }
        assert!(joined.contains("not part of Fetchpath"), "{joined}");
        assert!(body.iter().all(|line| line.width() <= 76), "{body:#?}");
        assert_eq!(text.keys, "Enter download and set up · Esc cancel");
    }

    fn draw_with(frame: &Frame, width: u16) -> (Vec<String>, Drawn) {
        let area = Rect::new(0, 5, width, frame.height(width));
        let mut buffer = Buffer::empty(area);
        let drawn = frame.render(area, &mut buffer);
        (text(&buffer), drawn)
    }

    #[test]
    fn a_menu_marks_the_selection_and_every_row_can_be_clicked() {
        use crate::tui::menu::{Item, Menu};
        let mut menu = Menu::new(
            "Settings",
            vec![
                Item {
                    inert: true,
                    ..Item::new("Downloads", "", "")
                },
                Item::new("Downloads at the same time", "3", "Left/Right to change"),
                Item::new("Retry automatically", "on", "Enter to switch"),
            ],
            "↑↓ choose · Esc close",
        );
        menu.select(2, 1);
        let live = Live::default();
        let panel = live.panel();
        let frame = Frame {
            panel: &panel,
            prompt: "",
            cursor: 0,
            hint: "",
            glyphs: &ASCII,
            max_rows: 4,
            card: Some(Overlay::Menu(&menu, None)),
            tick: 0,
            palette: &[],
            palette_selected: 0,
        };
        let (rows, drawn) = draw_with(&frame, 50);
        assert_eq!(
            rows,
            [
                "-- Nothing downloading ---------------------------",
                "+ Settings --------------------------------------+",
                "| Downloads                                      |",
                "|   Downloads at the same time  3                |",
                "| > Retry automatically         on               |",
                "| Enter to switch                                |",
                "+ Up/Down choose · Esc close --------------------+",
            ]
        );
        // Rows are absolute: the viewport starts at row 5.
        assert_eq!(drawn.target_at(8), Some(Target::Item(1)));
        assert_eq!(drawn.target_at(9), Some(Target::Item(2)));
        assert_eq!(drawn.target_at(7), None, "the heading is not a target");
        assert!(drawn.cursor.is_none());
    }

    #[test]
    fn typing_a_slash_lists_matching_commands_to_choose_from() {
        let live = Live::default();
        let panel = live.panel();
        let commands = [
            ("resume", "Resume paused downloads"),
            ("retry", "Try failed downloads again"),
        ];
        let frame = Frame {
            panel: &panel,
            prompt: "/re",
            cursor: 3,
            hint: "unused while the list shows",
            glyphs: &ASCII,
            max_rows: 4,
            card: None,
            tick: 0,
            palette: &commands.map(|(name, summary)| (name.to_owned(), summary.to_owned())),
            palette_selected: 1,
        };
        let (rows, drawn) = draw_with(&frame, 50);
        assert_eq!(
            rows,
            [
                "-- Nothing downloading ---------------------------",
                "> /re",
                "  /resume  Resume paused downloads",
                "  /retry   Try failed downloads again",
            ]
        );
        assert_eq!(drawn.target_at(8), Some(Target::Command(1)));
        assert_eq!(drawn.cursor, Some(Position::new(5, 6)));
    }
}
