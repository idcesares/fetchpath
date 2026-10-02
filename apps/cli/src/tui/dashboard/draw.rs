//! Drawing the dashboard: a header with the queue's summary, the queue, the
//! chosen download's details pane, a status line and the key map. Wide
//! windows put the details beside the queue, narrower ones below it.

use super::Key;
use crate::client;
use crate::tui::line::Out;
use crate::tui::live::Row;
use crate::tui::view::{self, ACCENT, DIM, Glyphs, fit, pad};
use fetchpath_protocol::JobSnapshot;
use fetchpath_protocol::model::{JobKind, JobState, Segment};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

/// Columns from which the details sit beside the queue.
const SIDE_BY_SIDE: u16 = 100;
/// The smallest window the dashboard draws in.
pub const MIN_WIDTH: u16 = 30;
pub const MIN_HEIGHT: u16 = 8;

pub struct Board<'a> {
    pub rows: &'a [Row<'a>],
    pub selected: usize,
    /// The first list entry shown when it last drew.
    pub first: usize,
    /// The chosen download's ranges in flight, when asked.
    pub segments: Option<&'a [Segment]>,
    /// Speed a second for the last minute, oldest first.
    pub speeds: &'a [Option<u64>],
    pub glyphs: &'a Glyphs,
    pub tick: u64,
    pub status: Option<&'a Out>,
    pub keys: &'a [Key],
}

/// What a frame drew: the first entry shown and which screen row holds
/// which entry, for scrolling and clicks.
#[derive(Debug, Default)]
pub struct Drawn {
    pub first: usize,
    pub rows: Vec<(u16, usize)>,
}

fn put(buffer: &mut Buffer, area: Rect, y: u16, line: &Line) {
    if y >= area.top() && y < area.bottom() {
        buffer.set_line(area.x, y, line, area.width);
    }
}

fn heading(text: &str) -> Span<'static> {
    Span::styled(
        text.to_owned(),
        Style::new().fg(ACCENT).add_modifier(Modifier::BOLD),
    )
}

impl Board<'_> {
    pub fn render(&self, area: Rect, buffer: &mut Buffer) -> Drawn {
        let glyphs = self.glyphs;
        if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
            put(
                buffer,
                area,
                area.y,
                &Line::raw(fit(
                    "Make the window larger to see the dashboard.",
                    area.width as usize,
                    glyphs.more,
                )),
            );
            put(
                buffer,
                area,
                area.y + 1,
                &Line::styled(fit("Esc goes back", area.width as usize, glyphs.more), DIM),
            );
            return Drawn {
                first: self.first,
                rows: Vec::new(),
            };
        }
        let width = area.width as usize;

        // Header: the name and the queue's summary.
        let summary = format!(" {} ", view::summary(self.rows));
        let title = " Fetchpath ";
        let lead = glyphs.rule.repeat(2);
        let rest = width.saturating_sub(lead.width() + title.width() + summary.width());
        put(
            buffer,
            area,
            area.y,
            &Line::from(vec![
                Span::styled(lead, DIM),
                heading(title),
                Span::styled(summary, Style::new().fg(ACCENT)),
                Span::styled(glyphs.rule.repeat(rest), DIM),
            ]),
        );

        // Footer: what the last action said, then the keys.
        let bottom = area.bottom();
        if let Some(status) = self.status {
            put(
                buffer,
                area,
                bottom - 2,
                &Line::styled(
                    fit(&status.text, width, glyphs.more),
                    view::tone_style(status.tone),
                ),
            );
        }
        put(buffer, area, bottom - 1, &self.key_map(width));

        let body = Rect::new(area.x, area.y + 1, area.width, area.height - 3);
        let (list, details) = if area.width >= SIDE_BY_SIDE {
            let left = area.width * 11 / 20;
            for y in body.top()..body.bottom() {
                buffer.set_string(body.x + left, y, glyphs.divider, DIM);
            }
            (
                Rect::new(body.x, body.y, left, body.height),
                Rect::new(
                    body.x + left + 2,
                    body.y,
                    body.width - left - 2,
                    body.height,
                ),
            )
        } else {
            // The queue gets up to two fifths, at least three rows.
            let wanted = self.rows.len().max(1) as u16;
            let height = wanted
                .clamp(3, (body.height * 2 / 5).max(3))
                .min(body.height);
            if body.height > height {
                put(
                    buffer,
                    body,
                    body.y + height,
                    &Line::styled(glyphs.rule.repeat(width), DIM),
                );
            }
            (
                Rect::new(body.x, body.y, body.width, height),
                Rect::new(
                    body.x,
                    body.y + height + 1,
                    body.width,
                    body.height.saturating_sub(height + 1),
                ),
            )
        };
        let drawn = self.list(list, buffer);
        if let Some(row) = self.rows.get(self.selected) {
            self.details(row.job, details, buffer);
        }
        drawn
    }

    fn key_map(&self, width: usize) -> Line<'static> {
        let mut parts: Vec<(String, &str)> = vec![("↑↓".to_owned(), "choose")];
        if !self.glyphs.unicode {
            parts[0].0 = "Up/Down".to_owned();
        }
        let mut seen = Vec::new();
        for key in self.keys {
            if !seen.contains(&key.key) {
                seen.push(key.key);
                parts.push((key.key.to_string(), key.label));
            }
        }
        parts.push(("Esc".to_owned(), "back"));
        let mut spans = Vec::new();
        let mut used = 0;
        for (index, (key, label)) in parts.iter().enumerate() {
            let separator = if index == 0 { "" } else { "  " };
            let piece = separator.width() + key.width() + 1 + label.width();
            // Keep "Esc back" whatever the width.
            let last = index == parts.len() - 1;
            if !last && used + piece + 10 > width {
                continue;
            }
            spans.push(Span::raw(separator));
            spans.push(Span::styled(
                key.clone(),
                Style::new().fg(ACCENT).add_modifier(Modifier::BOLD),
            ));
            spans.push(Span::styled(format!(" {label}"), DIM));
            used += piece;
        }
        Line::from(spans)
    }

    fn list(&self, area: Rect, buffer: &mut Buffer) -> Drawn {
        let height = area.height as usize;
        let mut drawn = Drawn::default();
        if self.rows.is_empty() {
            put(
                buffer,
                area,
                area.y,
                &Line::styled(
                    fit(
                        "  Nothing here yet. Esc goes back; paste a link at the prompt.",
                        area.width as usize,
                        self.glyphs.more,
                    ),
                    DIM,
                ),
            );
            return drawn;
        }
        // Scroll only as far as it takes to keep the choice in view.
        let mut first = self.first.min(self.rows.len().saturating_sub(1));
        if self.selected < first {
            first = self.selected;
        } else if height > 0 && self.selected >= first + height {
            first = self.selected + 1 - height;
        }
        drawn.first = first;
        for (offset, row) in self.rows.iter().skip(first).take(height).enumerate() {
            let y = area.y + offset as u16;
            let entry = first + offset;
            let line = view::job_line(row, area.width as usize, self.glyphs, self.tick);
            put(buffer, area, y, &line);
            if entry == self.selected {
                buffer.set_style(
                    Rect::new(area.x, y, area.width, 1),
                    Style::new().add_modifier(Modifier::REVERSED),
                );
            }
            drawn.rows.push((y, entry));
        }
        drawn
    }

    fn details(&self, job: &JobSnapshot, area: Rect, buffer: &mut Buffer) {
        if area.height == 0 || area.width < 10 {
            return;
        }
        let width = area.width as usize;
        let more = self.glyphs.more;
        let mut lines: Vec<Line<'static>> = Vec::new();
        lines.push(Line::styled(
            fit(&client::name(job), width, more),
            Style::new().add_modifier(Modifier::BOLD),
        ));
        let field = |label: &str, value: &str| -> Line<'static> {
            Line::from(vec![
                Span::styled(format!("{label:<9}"), DIM),
                Span::raw(fit(value, width.saturating_sub(9), more)),
            ])
        };
        let mut state = client::state_label(job).to_owned();
        if job.attempt > 0 {
            state.push_str(&format!(", attempt {}", job.attempt + 1));
        }
        if let Some(at) = job.retry_at {
            state.push_str(&format!(", retrying {}", crate::when::local(at)));
        }
        if let Some(at) = job.not_before
            && job.state == JobState::Queued
        {
            state.push_str(&format!(", starts {}", crate::when::local(at)));
        }
        if let Some(quality) = &job.quality_label {
            state.push_str(&format!(" · {quality}"));
        }
        lines.push(field("State", &state));
        let progress = if job.state == JobState::Running {
            client::progress_line(&job.progress).trim_start().to_owned()
        } else {
            client::amount(&job.progress)
        };
        if !progress.is_empty() {
            lines.push(field("Progress", &progress));
        }
        lines.push(field("From", &job.source_display));
        lines.push(field(
            "To",
            job.destination.as_deref().unwrap_or("not chosen yet"),
        ));
        if let Some(error) = &job.error {
            lines.push(Line::from(vec![
                Span::styled(format!("{:<9}", "Problem"), DIM),
                Span::styled(
                    fit(&error.message, width.saturating_sub(9), more),
                    Style::new().fg(Color::Red),
                ),
            ]));
        }

        // Share what is left between the graph and the ranges.
        let left = (area.height as usize).saturating_sub(lines.len());
        let graph_rows = if left >= 12 {
            5
        } else if left >= 8 {
            3
        } else if left >= 5 {
            2
        } else {
            0
        };
        if graph_rows > 0 {
            lines.push(Line::raw(""));
            lines.extend(self.graph(job, graph_rows, width));
        }
        let left = (area.height as usize).saturating_sub(lines.len());
        if left >= 3 {
            lines.push(Line::raw(""));
            lines.extend(self.ranges(job, left - 1, width));
        }
        for (offset, line) in lines.iter().take(area.height as usize).enumerate() {
            put(buffer, area, area.y + offset as u16, line);
        }
    }

    /// The speed graph: a heading with the latest and highest speed, then
    /// `rows` rows with a column a second, newest on the right.
    fn graph(&self, job: &JobSnapshot, rows: usize, width: usize) -> Vec<Line<'static>> {
        let measured: Vec<u64> = self.speeds.iter().flatten().copied().collect();
        let Some(&peak) = measured.iter().max() else {
            return vec![
                Line::from(vec![heading("Speed")]),
                Line::styled("  No speed measured yet.", DIM),
            ];
        };
        // Only a running download has a speed now.
        let now = match self.speeds.last().copied().flatten() {
            Some(now) if job.state == JobState::Running => {
                format!("now {}/s · ", client::bytes(now))
            }
            _ => String::new(),
        };
        let mut out = vec![Line::from(vec![
            heading("Speed"),
            Span::styled(
                fit(
                    &format!("  {now}peak {}/s · last minute", client::bytes(peak)),
                    width.saturating_sub(5),
                    self.glyphs.more,
                ),
                DIM,
            ),
        ])];
        let columns = self.speeds.len().min(width);
        let shown = &self.speeds[self.speeds.len() - columns..];
        let levels: &[&str] = if self.glyphs.unicode {
            &[" ", "▁", "▂", "▃", "▄", "▅", "▆", "▇", "█"]
        } else {
            &[" ", ".", ".", ":", ":", "|", "|", "#", "#"]
        };
        let steps = rows * 8;
        for row in (0..rows).rev() {
            let mut text = String::with_capacity(columns);
            for value in shown {
                let level = match value {
                    Some(value) if peak > 0 => {
                        let scaled = (*value as f64 / peak as f64 * steps as f64).round() as usize;
                        // A moving download never draws as nothing.
                        if *value > 0 { scaled.max(1) } else { 0 }
                    }
                    _ => 0,
                };
                let fill = level.saturating_sub(row * 8).min(8);
                text.push_str(levels[fill]);
            }
            out.push(Line::styled(text, Style::new().fg(ACCENT)));
        }
        out
    }

    /// Ranges in flight, one per connection, each with how much of it has
    /// arrived.
    fn ranges(&self, job: &JobSnapshot, rows: usize, width: usize) -> Vec<Line<'static>> {
        let more = self.glyphs.more;
        let Some(segments) = self.segments.filter(|segments| !segments.is_empty()) else {
            let why = if job.kind != JobKind::File {
                "The media helper fetches video and audio; it reports no ranges."
            } else if job.state == JobState::Running {
                "No ranges in flight at this moment."
            } else {
                "Ranges show while the download runs."
            };
            return vec![
                Line::from(vec![heading("Connections")]),
                Line::styled(fit(&format!("  {why}"), width, more), DIM),
            ];
        };
        let in_flight: u64 = segments.iter().map(|segment| segment.received).sum();
        let mut out = vec![Line::from(vec![
            heading("Connections"),
            Span::styled(
                fit(
                    &format!(
                        "  {} · {} in flight",
                        segments.len(),
                        client::bytes(in_flight)
                    ),
                    width.saturating_sub(11),
                    more,
                ),
                DIM,
            ),
        ])];
        let room = rows.saturating_sub(1);
        let hidden = segments.len().saturating_sub(room);
        let shown = if hidden > 0 {
            room.saturating_sub(1)
        } else {
            room
        };
        for (number, segment) in segments.iter().enumerate().take(shown) {
            let length = segment.end.saturating_sub(segment.start) + 1;
            let done = (segment.received as f64 / length as f64).clamp(0.0, 1.0);
            let label = format!(
                "{:>2} {}–{}",
                number + 1,
                client::bytes(segment.start),
                client::bytes(segment.end + 1)
            );
            let label = if self.glyphs.unicode {
                label
            } else {
                label.replace('–', "-")
            };
            let percent = format!(" {:>3.0}%", done * 100.0);
            let label_width = 26.min(width / 2);
            let bar_width = width.saturating_sub(label_width + percent.width() + 1);
            let filled = (done * bar_width as f64).round() as usize;
            out.push(Line::from(vec![
                Span::raw(pad(&label, label_width, more)),
                Span::raw(" "),
                Span::styled(self.glyphs.bar_full.repeat(filled), Style::new().fg(ACCENT)),
                Span::styled(self.glyphs.bar_empty.repeat(bar_width - filled), DIM),
                Span::styled(percent, Style::new().fg(ACCENT)),
            ]));
        }
        if hidden > 0 {
            out.push(Line::styled(
                format!("   {more} {} more", segments.len() - shown),
                DIM,
            ));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::config::keys::Keys;
    use crate::tui::dashboard::{entries, keys};
    use crate::tui::live::{Live, tests::recorded};
    use crate::tui::view::{ASCII, UNICODE};
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

    /// The recorded stream at its sixth progress sample: one file moving,
    /// the other just failed.
    fn midway() -> Live {
        let mut live = Live::default();
        let mut samples = 0;
        for message in recorded() {
            match message {
                ServerMessage::Event(event) => {
                    live.apply(&event);
                }
                ServerMessage::Progress(sample) => {
                    live.progress(&sample);
                    samples += 1;
                }
                ServerMessage::Reply(_) => {}
            }
            if samples == 6 {
                break;
            }
        }
        live
    }

    const SEGMENTS: [Segment; 3] = [
        Segment {
            start: 0,
            end: 999_999,
            received: 750_000,
        },
        Segment {
            start: 1_000_000,
            end: 1_999_999,
            received: 250_000,
        },
        Segment {
            start: 2_000_000,
            end: 2_999_999,
            received: 0,
        },
    ];

    fn speeds() -> Vec<Option<u64>> {
        let mut speeds = vec![None; 50];
        speeds.extend(
            [
                400_000, 800_000, 1_200_000, 1_600_000, 2_000_000, 1_800_000, 1_500_000, 0,
                1_000_000, 1_500_000,
            ]
            .map(Some),
        );
        speeds
    }

    fn draw(live: &Live, glyphs: &Glyphs, width: u16, height: u16) -> (Vec<String>, Drawn) {
        let rows = entries(live);
        let offered = keys(rows[0].job, &Keys::default());
        let speeds = speeds();
        let board = Board {
            rows: &rows,
            selected: 0,
            first: 0,
            segments: Some(&SEGMENTS),
            speeds: &speeds,
            glyphs,
            tick: 0,
            status: None,
            keys: &offered,
        };
        let area = Rect::new(0, 0, width, height);
        let mut buffer = Buffer::empty(area);
        let drawn = board.render(area, &mut buffer);
        (text(&buffer), drawn)
    }

    #[test]
    fn a_wide_window_puts_the_details_beside_the_queue() {
        let (rows, drawn) = draw(&midway(), &ASCII, 120, 24);
        assert_eq!(
            rows,
            [
                "-- Fetchpath  1 downloading at 1.4 MiB/s · 1 to check ------------------------------------------------------------------",
                "  2 [>] sample.bin          #########----  66%  1.4 MiB/s  0:01   | sample.bin",
                "  1 [!] missing.zip         -------------       failed            | State    running",
                "                                                                  | Progress 65.5%  1.9 MiB of 2.9 MiB  1.4 MiB/s  0:...",
                "                                                                  | From     http://downloads.example/sample.bin",
                "                                                                  | To       C:\\Users\\person\\Downloads\\sample.bin",
                "                                                                  |",
                "                                                                  | Speed  now 1.4 MiB/s · peak 1.9 MiB/s · last minute",
                "                                                                  |                                               #:",
                "                                                                  |                                              ###|  |",
                "                                                                  |                                             ##### :#",
                "                                                                  |                                            ###### ##",
                "                                                                  |                                           ####### ##",
                "                                                                  |",
                "                                                                  | Connections  3 · 976.6 KiB in flight",
                "                                                                  |  1 0 B-976.6 KiB           ###############-----  75%",
                "                                                                  |  2 976.6 KiB-1.9 MiB       #####---------------  25%",
                "                                                                  |  3 1.9 MiB-2.9 MiB         --------------------   0%",
                "                                                                  |",
                "                                                                  |",
                "                                                                  |",
                "                                                                  |",
                "",
                "Up/Down choose  p pause  c cancel  o folder  Esc back",
            ]
        );
        assert_eq!(drawn.rows, [(1, 0), (2, 1)]);
    }

    #[test]
    fn a_narrow_window_puts_the_details_below_and_keeps_the_keys() {
        let (rows, _) = draw(&midway(), &ASCII, 60, 20);
        for row in &rows {
            assert!(row.chars().count() <= 60, "{row}");
        }
        assert!(rows[1].contains("sample.bin"), "{rows:?}");
        assert!(rows[2].contains("missing.zip"), "{rows:?}");
        assert!(rows.iter().any(|row| row.starts_with("State")), "{rows:?}");
        assert!(
            rows.iter().any(|row| row.starts_with("Connections")),
            "{rows:?}"
        );
        assert_eq!(
            rows[19],
            "Up/Down choose  p pause  c cancel  o folder  Esc back"
        );
    }

    #[test]
    fn every_size_draws_without_spilling_and_a_tiny_one_says_so() {
        let live = midway();
        for (width, height) in [
            (1, 1),
            (29, 7),
            (30, 8),
            (45, 12),
            (80, 24),
            (99, 30),
            (100, 30),
            (200, 60),
        ] {
            for glyphs in [&ASCII, &UNICODE] {
                let (rows, drawn) = draw(&live, glyphs, width, height);
                assert_eq!(rows.len(), height as usize);
                assert!(drawn.rows.iter().all(|(y, _)| *y < height));
                if width < MIN_WIDTH || height < MIN_HEIGHT {
                    assert!(drawn.rows.is_empty());
                }
            }
        }
        let (rows, _) = draw(&live, &ASCII, 29, 7);
        assert_eq!(rows[0], "Make the window larger to ...");
    }

    #[test]
    fn the_list_scrolls_to_keep_the_choice_in_view() {
        let live = midway();
        let rows = entries(&live);
        let board = Board {
            rows: &rows,
            selected: 1,
            first: 0,
            segments: None,
            speeds: &[],
            glyphs: &ASCII,
            tick: 0,
            status: None,
            keys: &[],
        };
        // Sixty columns, nine rows: the queue gets three rows, so both fit.
        let area = Rect::new(0, 0, 60, 9);
        let mut buffer = Buffer::empty(area);
        assert_eq!(board.render(area, &mut buffer).first, 0);
        // A list scrolled past the choice comes back to it.
        let board = Board { first: 5, ..board };
        let mut buffer = Buffer::empty(area);
        let drawn = board.render(area, &mut buffer);
        assert_eq!(drawn.first, 1);
        assert_eq!(drawn.rows[0], (1, 1));
    }

    #[test]
    fn a_waiting_download_says_when_ranges_show() {
        let live = midway();
        let rows = entries(&live);
        let board = Board {
            rows: &rows,
            selected: 1,
            first: 0,
            segments: None,
            speeds: &[],
            glyphs: &ASCII,
            tick: 0,
            status: None,
            keys: &keys(rows[1].job, &Keys::default()),
        };
        let area = Rect::new(0, 0, 120, 24);
        let mut buffer = Buffer::empty(area);
        board.render(area, &mut buffer);
        let text = text(&buffer).join("\n");
        assert!(text.contains("No speed measured yet."), "{text}");
        assert!(
            text.contains("Ranges show while the download runs."),
            "{text}"
        );
    }
}
