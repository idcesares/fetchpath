//! The dashboard (FP-060): a full-screen view toggled from the inline view
//! with F2 (or the key `/keys` gives it) or `/dashboard`. It lists the
//! queue, shows the chosen download's details (speed over the last minute, and its ranges in flight, one per
//! connection, from the engine's `JobDetails`) and acts on it with single
//! keys named on screen. It uses the alternate screen, so leaving it puts
//! the inline view and scrollback back as they were; what happened while it
//! was open prints into scrollback then.

mod draw;

use super::Session;
use super::config::keys::{Act, Bind, Keys};
use super::jobs_menu::{self, Deed, JobsMenu};
use super::line::{Out, Tone};
use super::live::{Live, Row};
use super::view::FINISHED;
use crate::queue::Control;
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers,
    MouseButton, MouseEventKind,
};
use crossterm::{cursor, terminal};
use draw::Board;
use fetchpath_protocol::command::Command;
use fetchpath_protocol::message::CommandResult;
use fetchpath_protocol::model::{JobKind, JobState, Segment};
use fetchpath_protocol::{JobId, JobSnapshot, ProtocolError};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

/// How far back the speed graph looks, one column a second, as the
/// desktop's details window does.
pub const SPEED_WINDOW: Duration = Duration::from_secs(60);

/// A sample older than this no longer speaks for the second being drawn.
const SAMPLE_LIFE: Duration = Duration::from_secs(3);

/// Recent finished downloads listed after the ones in progress.
const FINISHED_ROWS: usize = 30;

/// How often the chosen download's ranges are asked for while it moves.
const DETAILS_EVERY: Duration = Duration::from_millis(500);

/// Engine-measured speeds per download, recorded from every progress sample
/// whether or not the dashboard is open, so its graph has history at once.
#[derive(Default)]
pub struct Speeds {
    jobs: HashMap<JobId, VecDeque<(Instant, u64)>>,
}

impl Speeds {
    pub fn record(&mut self, job_id: &JobId, at: Instant, rate: Option<u64>) {
        let Some(rate) = rate else {
            return;
        };
        let samples = self.jobs.entry(job_id.clone()).or_default();
        samples.push_back((at, rate));
        while samples
            .front()
            .is_some_and(|(then, _)| at.duration_since(*then) > SPEED_WINDOW + SAMPLE_LIFE)
        {
            samples.pop_front();
        }
        // Forget downloads that have not moved for a while.
        if self.jobs.len() > 64 {
            self.jobs.retain(|_, samples| {
                samples
                    .back()
                    .is_some_and(|(then, _)| at.duration_since(*then) <= SPEED_WINDOW)
            });
        }
    }

    /// One value a second for the last minute, oldest first: the latest
    /// sample at or before each second's end, if one is recent enough;
    /// `None` before the first sample.
    pub fn series(&self, job_id: &JobId, now: Instant) -> Vec<Option<u64>> {
        let Some(samples) = self.jobs.get(job_id) else {
            return Vec::new();
        };
        let seconds = SPEED_WINDOW.as_secs() as u32;
        let mut series = Vec::with_capacity(seconds as usize);
        let mut started = false;
        for back in (0..seconds).rev() {
            let Some(end) = now.checked_sub(Duration::from_secs(back.into())) else {
                series.push(None);
                continue;
            };
            let latest = samples.iter().rev().find(|(then, _)| *then <= end);
            let value = match latest {
                Some((then, rate)) => {
                    started = true;
                    Some(if end.duration_since(*then) <= SAMPLE_LIFE {
                        *rate
                    } else {
                        0
                    })
                }
                None => None,
            };
            series.push(if started { value.or(Some(0)) } else { None });
        }
        series
    }
}

/// Queue filter cycled with `Tab` / `Shift+Tab` in the dashboard.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Filter {
    #[default]
    All,
    Active,
    Attention,
    Paused,
    Finished,
}

impl Filter {
    pub fn next(self) -> Self {
        match self {
            Self::All => Self::Active,
            Self::Active => Self::Attention,
            Self::Attention => Self::Paused,
            Self::Paused => Self::Finished,
            Self::Finished => Self::All,
        }
    }

    pub fn prev(self) -> Self {
        match self {
            Self::All => Self::Finished,
            Self::Active => Self::All,
            Self::Attention => Self::Active,
            Self::Paused => Self::Attention,
            Self::Finished => Self::Paused,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::Active => "Active",
            Self::Attention => "Attention",
            Self::Paused => "Paused",
            Self::Finished => "Finished",
        }
    }

    pub fn matches(self, row: &Row<'_>) -> bool {
        match self {
            Self::All => true,
            Self::Active => row.group == 0,
            Self::Attention => matches!(
                row.job.state,
                JobState::Failed | JobState::AwaitingApproval | JobState::WaitingForSource
            ),
            Self::Paused => row.job.state == JobState::Paused,
            Self::Finished => row.group == FINISHED,
        }
    }
}

/// The queue as the dashboard lists it: the panel's jobs, then recent
/// finished ones.
pub fn entries(live: &Live) -> Vec<Row<'_>> {
    let mut rows = live.panel();
    rows.extend(
        live.jobs()
            .iter()
            .enumerate()
            .filter(|(_, job)| live.group(job).is_none())
            .take(FINISHED_ROWS)
            .map(|(position, job)| Row {
                index: position + 1,
                group: FINISHED,
                job,
            }),
    );
    rows
}

/// Filters dashboard entries by `filter` and case-insensitive substring `query`
/// matching file name, job id, or source URL.
pub fn filtered_entries<'a>(live: &'a Live, filter: Filter, query: &str) -> Vec<Row<'a>> {
    let needle = query.trim().to_ascii_lowercase();
    entries(live)
        .into_iter()
        .filter(|row| filter.matches(row))
        .filter(|row| {
            if needle.is_empty() {
                return true;
            }
            crate::client::name(row.job)
                .to_ascii_lowercase()
                .contains(&needle)
                || row
                    .job
                    .job_id
                    .as_str()
                    .to_ascii_lowercase()
                    .contains(&needle)
                || row
                    .job
                    .source_display
                    .to_ascii_lowercase()
                    .contains(&needle)
        })
        .collect()
}

/// A single-key action on the chosen download.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Key {
    pub key: Bind,
    pub label: &'static str,
    pub deed: Deed,
}

/// The action a key does for a deed, and its label; `None` for deeds the
/// dashboard does not offer.
fn act_of(deed: Deed) -> Option<(Act, &'static str)> {
    Some(match deed {
        Deed::Control(Control::Pause) => (Act::Pause, "pause"),
        Deed::Control(Control::Resume) => (Act::Resume, "resume"),
        Deed::Control(Control::Retry) => (Act::Resume, "retry"),
        Deed::Control(Control::Cancel) => (Act::Cancel, "cancel"),
        Deed::Control(Control::Remove) => (Act::Remove, "remove"),
        Deed::Control(Control::Approve) => (Act::Approve, "approve"),
        Deed::Control(Control::Deny) => (Act::Deny, "deny"),
        // Typing a name belongs to the prompt's own card.
        Deed::Rename => return None,
        Deed::Reveal => (Act::Folder, "folder"),
        // The dashboard already shows the details.
        Deed::Show => return None,
    })
}

/// The keys that make sense for a job, from the same list `/queue` offers,
/// as bound in `cli.toml`. An action left without a key is not offered.
pub fn keys(job: &JobSnapshot, bound: &Keys) -> Vec<Key> {
    jobs_menu::deeds(job)
        .into_iter()
        .filter_map(|(deed, _)| {
            let (act, label) = act_of(deed)?;
            Some(Key {
                key: bound.get(act)?,
                label,
                deed,
            })
        })
        .collect()
}

/// Ranges in flight for one download, as last asked.
struct Details {
    job_id: JobId,
    segments: Vec<Segment>,
    at: Instant,
}

struct Dashboard {
    /// Followed by id, so the choice stays put as the list reorders.
    selected: Option<JobId>,
    first: usize,
    details: Option<Details>,
    /// What the last action said, shown above the key map.
    status: Option<Out>,
    /// Set by a first `c`; a second one cancels.
    armed: Option<JobId>,
    /// Everything said while open, printed into scrollback on leaving.
    said: Vec<Out>,
    /// Which screen row holds which list entry, from the last frame.
    rows: Vec<(u16, usize)>,
    filter: Filter,
    search: String,
    searching: bool,
}

impl Dashboard {
    fn new(live: &Live) -> Self {
        Self {
            selected: entries(live).first().map(|row| row.job.job_id.clone()),
            first: 0,
            details: None,
            status: None,
            armed: None,
            said: Vec::new(),
            rows: Vec::new(),
            filter: Filter::All,
            search: String::new(),
            searching: false,
        }
    }

    fn visible<'a>(&self, live: &'a Live) -> Vec<Row<'a>> {
        filtered_entries(live, self.filter, &self.search)
    }

    /// The chosen entry's position, falling back to the first entry when
    /// the chosen job has gone.
    fn position(&mut self, rows: &[Row]) -> usize {
        let found = self
            .selected
            .as_ref()
            .and_then(|id| rows.iter().position(|row| &row.job.job_id == id));
        let position = found.unwrap_or(0);
        self.selected = rows.get(position).map(|row| row.job.job_id.clone());
        position
    }

    fn select(&mut self, rows: &[Row], position: usize) {
        if let Some(row) = rows.get(position.min(rows.len().saturating_sub(1))) {
            self.selected = Some(row.job.job_id.clone());
        }
        self.disarm();
    }

    /// Forgets a first `c`, and the line asking for a second.
    fn disarm(&mut self) {
        if self.armed.take().is_some() {
            self.status = None;
        }
    }

    fn say(&mut self, lines: Vec<Out>) {
        if let Some(last) = lines.last() {
            self.status = Some(last.clone());
        }
        self.said.extend(lines);
    }

    /// Asks for the chosen download's ranges while it moves; a finished or
    /// waiting one has none.
    fn fetch_details(&mut self, session: &Session) {
        let rows = self.visible(&session.live);
        let position = self.position(&rows);
        let Some(row) = rows.get(position) else {
            self.details = None;
            return;
        };
        if row.group != 0 || row.job.kind != JobKind::File {
            self.details = None;
            return;
        }
        let fresh = self
            .details
            .as_ref()
            .is_some_and(|held| held.job_id == row.job.job_id && held.at.elapsed() < DETAILS_EVERY);
        if fresh {
            return;
        }
        let job_id = row.job.job_id.clone();
        self.details = match session.engine.send(Command::JobDetails {
            job_id: job_id.clone(),
        }) {
            Ok(CommandResult::Details { details }) => Some(Details {
                job_id,
                segments: details.segments,
                at: Instant::now(),
            }),
            // Not worth interrupting the view: the next ask may succeed.
            _ => None,
        };
    }

    /// Does `act` to the chosen download, if it applies to it. Delete
    /// removes whatever key `remove` has, so this takes the action.
    fn act(&mut self, session: &Session, act: Act) {
        let rows = self.visible(&session.live);
        let position = self.position(&rows);
        let Some(job) = rows.get(position).map(|row| row.job.clone()) else {
            return;
        };
        let bound = &session.config.keys;
        let deed = jobs_menu::deeds(&job)
            .into_iter()
            .map(|(deed, _)| deed)
            .find(|deed| act_of(*deed).is_some_and(|(offered, _)| offered == act));
        let Some(deed) = deed else {
            self.disarm();
            return;
        };
        if deed == Deed::Control(Control::Cancel) && self.armed.as_ref() != Some(&job.job_id) {
            self.armed = Some(job.job_id.clone());
            // Only the cancel key reaches here, so it has one.
            let key = bound.label(Act::Cancel).unwrap_or_default();
            self.status = Some(Out::new(
                Tone::Bad,
                format!("Press {key} again to cancel {}", crate::client::name(&job)),
            ));
            return;
        }
        self.armed = None;
        let lines = JobsMenu::act(&session.engine, deed, &job);
        self.say(lines);
    }

    /// Spacebar quick-toggle: pauses active jobs, resumes paused jobs, or retries failed jobs.
    fn toggle_selected(&mut self, session: &Session) {
        let rows = self.visible(&session.live);
        let position = self.position(&rows);
        let Some(job) = rows.get(position).map(|row| row.job) else {
            return;
        };
        match job.state {
            JobState::Running | JobState::Queued => self.act(session, Act::Pause),
            JobState::Paused | JobState::Failed => self.act(session, Act::Resume),
            _ => {}
        }
    }
}

/// The alternate screen with the mouse captured, left on every way out.
struct FullScreen;

impl FullScreen {
    fn enter() -> std::io::Result<Self> {
        crossterm::execute!(
            std::io::stdout(),
            terminal::EnterAlternateScreen,
            EnableMouseCapture,
            cursor::Hide
        )?;
        Ok(Self)
    }
}

impl Drop for FullScreen {
    fn drop(&mut self) {
        let _ = crossterm::execute!(
            std::io::stdout(),
            cursor::Show,
            DisableMouseCapture,
            terminal::LeaveAlternateScreen
        );
    }
}

/// What the dashboard hands back to the inline view.
pub struct Leave {
    /// For scrollback: receipts and what actions said.
    pub lines: Vec<Out>,
    /// The engine went away and could not be reached again.
    pub lost: Option<ProtocolError>,
}

pub fn run(session: &mut Session) -> std::io::Result<Leave> {
    let screen = FullScreen::enter()?;
    let mut terminal = Terminal::new(CrosstermBackend::new(std::io::stdout()))?;
    terminal.clear()?;
    let mut board = Dashboard::new(&session.live);
    let started = Instant::now();
    let mut dirty = true;
    let mut last_draw = Instant::now();
    let mut lost = None;
    'open: loop {
        let glyphs = &session.config.glyphs();
        let theme = session.config.theme;
        let moving = session.live.panel().iter().any(|row| row.group == 0);
        let every = Duration::from_millis(if moving { 100 } else { 500 });
        if dirty || last_draw.elapsed() >= every {
            board.fetch_details(session);
            let total_rows = entries(&session.live).len();
            let rows = board.visible(&session.live);
            let selected = board.position(&rows);
            let now = Instant::now();
            let speeds = rows
                .get(selected)
                .map(|row| session.speeds.series(&row.job.job_id, now))
                .unwrap_or_default();
            let offered = rows
                .get(selected)
                .map(|row| keys(row.job, &session.config.keys))
                .unwrap_or_default();
            let segments = board.details.as_ref().and_then(|details| {
                rows.get(selected)
                    .filter(|row| row.job.job_id == details.job_id)
                    .map(|_| details.segments.as_slice())
            });
            let mut drawn = draw::Drawn::default();
            terminal.draw(|frame| {
                let area = frame.area();
                drawn = Board {
                    rows: &rows,
                    total_rows,
                    selected,
                    first: board.first,
                    segments,
                    speeds: &speeds,
                    glyphs,
                    tick: (started.elapsed().as_millis() / 100) as u64,
                    status: board.status.as_ref(),
                    keys: &offered,
                    filter: board.filter,
                    search: &board.search,
                    searching: board.searching,
                }
                .render(area, frame.buffer_mut());
                theme.apply(frame.buffer_mut());
            })?;
            board.first = drawn.first;
            board.rows = drawn.rows;
            dirty = false;
            last_draw = Instant::now();
        }
        if event::poll(Duration::from_millis(40))? {
            dirty = true;
            let rows = board.visible(&session.live);
            let position = board.position(&rows);
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Release => dirty = false,
                Event::Key(key) if board.searching => match key.code {
                    KeyCode::Esc => {
                        board.search.clear();
                        board.searching = false;
                    }
                    KeyCode::Enter => {
                        board.searching = false;
                    }
                    KeyCode::Backspace => {
                        board.search.pop();
                    }
                    KeyCode::Up => board.select(&rows, position.saturating_sub(1)),
                    KeyCode::Down => board.select(&rows, position + 1),
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        break 'open;
                    }
                    KeyCode::Char(c)
                        if !key.modifiers.contains(KeyModifiers::CONTROL)
                            && !key.modifiers.contains(KeyModifiers::ALT) =>
                    {
                        board.search.push(c);
                    }
                    _ => {}
                },
                Event::Key(key) => match key.code {
                    // These keys keep their meaning whatever is bound.
                    KeyCode::Esc => {
                        if board.filter != Filter::All || !board.search.is_empty() {
                            board.filter = Filter::All;
                            board.search.clear();
                            board.disarm();
                        } else {
                            break 'open;
                        }
                    }
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        break 'open;
                    }
                    KeyCode::Tab => {
                        board.filter = board.filter.next();
                        board.disarm();
                    }
                    KeyCode::BackTab => {
                        board.filter = board.filter.prev();
                        board.disarm();
                    }
                    KeyCode::Char('/') => {
                        board.searching = true;
                        board.disarm();
                    }
                    KeyCode::Char(' ') => board.toggle_selected(session),
                    KeyCode::Up => board.select(&rows, position.saturating_sub(1)),
                    KeyCode::Down => board.select(&rows, position + 1),
                    KeyCode::PageUp => board.select(&rows, position.saturating_sub(10)),
                    KeyCode::PageDown => board.select(&rows, position + 10),
                    KeyCode::Home => board.select(&rows, 0),
                    KeyCode::End => board.select(&rows, rows.len().saturating_sub(1)),
                    KeyCode::Delete => board.act(session, Act::Remove),
                    _ => match session.config.keys.act(&key) {
                        Some(Act::Close | Act::Dashboard) => break 'open,
                        Some(Act::Up) => board.select(&rows, position.saturating_sub(1)),
                        Some(Act::Down) => board.select(&rows, position + 1),
                        Some(act) => board.act(session, act),
                        None => match key.code {
                            KeyCode::Char('g') => board.select(&rows, 0),
                            KeyCode::Char('G') => board.select(&rows, rows.len().saturating_sub(1)),
                            _ => board.disarm(),
                        },
                    },
                },
                Event::Mouse(mouse) => match mouse.kind {
                    MouseEventKind::Down(MouseButton::Left) => {
                        if let Some((_, entry)) =
                            board.rows.iter().find(|(y, _)| *y == mouse.row).copied()
                        {
                            board.select(&rows, entry);
                        }
                    }
                    MouseEventKind::ScrollUp => board.select(&rows, position.saturating_sub(1)),
                    MouseEventKind::ScrollDown => board.select(&rows, position + 1),
                    _ => dirty = false,
                },
                // The next draw fits the new size; ratatui clears on a
                // change of size, so nothing stale is left behind.
                Event::Resize(..) => {}
                _ => dirty = false,
            }
        }
        match session.poll(Duration::ZERO) {
            Ok(notices) => {
                if !notices.is_empty() {
                    let lines = super::inline::notice_lines(notices, glyphs);
                    board.say(lines);
                    dirty = true;
                }
            }
            Err(error) => {
                lost = Some(error);
                break 'open;
            }
        }
    }
    drop(terminal);
    drop(screen);
    Ok(Leave {
        lines: board.said,
        lost,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::live::tests::recorded;
    use fetchpath_protocol::message::ServerMessage;

    fn id(n: u8) -> JobId {
        serde_json::from_value(serde_json::json!(format!(
            "00000000-0000-4000-8000-0000000000{n:02}"
        )))
        .expect("an id")
    }

    #[test]
    fn filter_cycles_and_filters_entries_by_state_and_query() {
        assert_eq!(Filter::All.next().prev(), Filter::All);
        assert_eq!(Filter::All.prev().next(), Filter::All);

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
        assert_eq!(filtered_entries(&live, Filter::All, "").len(), 2);
        assert_eq!(filtered_entries(&live, Filter::Active, "").len(), 1);
        assert_eq!(filtered_entries(&live, Filter::Attention, "").len(), 1);
        assert_eq!(filtered_entries(&live, Filter::Paused, "").len(), 0);
        assert_eq!(filtered_entries(&live, Filter::All, "sample").len(), 1);
        assert_eq!(filtered_entries(&live, Filter::All, "MISSING").len(), 1);
    }

    #[test]
    fn the_speed_series_holds_a_second_per_column_and_marks_gaps() {
        let mut speeds = Speeds::default();
        let job = id(1);
        let start = Instant::now();
        let at = |seconds: u64| start + Duration::from_secs(seconds);
        speeds.record(&job, at(0), Some(100));
        speeds.record(&job, at(1), None);
        speeds.record(&job, at(2), Some(300));
        // A stall: nothing from 3 s until 9 s.
        speeds.record(&job, at(9), Some(50));
        let series = speeds.series(&job, at(10));
        assert_eq!(series.len(), 60);
        let tail: Vec<_> = series[49..].to_vec();
        assert_eq!(
            tail,
            [
                Some(100),
                Some(100),
                Some(300),
                Some(300),
                Some(300),
                Some(300),
                Some(0),
                Some(0),
                Some(0),
                Some(50),
                Some(50),
            ]
        );
        assert!(series[..49].iter().all(Option::is_none));
        assert!(speeds.series(&id(2), at(10)).is_empty());
    }

    #[test]
    fn samples_older_than_the_window_are_dropped() {
        let mut speeds = Speeds::default();
        let job = id(1);
        let start = Instant::now();
        for second in 0..200 {
            speeds.record(&job, start + Duration::from_secs(second), Some(second));
        }
        assert!(speeds.jobs[&job].len() <= 64);
    }
}
