//! Inline mode: the panel and prompt in a ratatui inline viewport at the
//! bottom of normal scrollback, with receipts and command output inserted
//! above it.

use super::Session;
use super::line::{self, Context, Out, Tone};
use super::live::Notice;
use super::prompt::{Action, Prompt};
use super::review::{self, Answer, Card, Draft, Look};
use super::setup::{Answer as SetupAnswer, Setup};
use super::view::Overlay;
use super::view::{self, Frame, Glyphs};
use crate::client::{self, EXIT_ENGINE, Engine};
use crossterm::event::{self, Event};
use crossterm::terminal;
use fetchpath_protocol::ProtocolError;
use ratatui::backend::{Backend, ClearType, CrosstermBackend};
use ratatui::layout::Position;
use ratatui::{Terminal, TerminalOptions, Viewport};
use std::collections::VecDeque;
use std::io::{Stdout, Write};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::{Duration, Instant};

type Term = Terminal<CrosstermBackend<Stdout>>;

/// Raw mode for as long as the view is open, restored on every way out,
/// a panic included.
struct RawMode;

impl RawMode {
    fn enable() -> std::io::Result<Self> {
        terminal::enable_raw_mode()?;
        Ok(Self)
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        let _ = terminal::disable_raw_mode();
    }
}

struct Screen {
    terminal: Term,
    height: u16,
}

impl Screen {
    fn open(height: u16) -> std::io::Result<Self> {
        let terminal = Terminal::with_options(
            CrosstermBackend::new(std::io::stdout()),
            TerminalOptions {
                viewport: Viewport::Inline(height),
            },
        )?;
        Ok(Self { terminal, height })
    }

    /// Ratatui's inline viewport has a fixed height, so a new height means
    /// clearing the old viewport and opening a new one where it began. A
    /// taller one scrolls earlier output up to make room.
    fn set_height(&mut self, height: u16) -> std::io::Result<()> {
        if height == self.height {
            return Ok(());
        }
        let top = self.terminal.get_frame().area().top();
        let backend = self.terminal.backend_mut();
        backend.set_cursor_position(Position::new(0, top))?;
        backend.clear_region(ClearType::AfterCursor)?;
        Backend::flush(backend)?;
        *self = Self::open(height)?;
        Ok(())
    }

    /// Prints lines into scrollback above the viewport.
    fn print(&mut self, lines: &[Out]) -> std::io::Result<()> {
        if lines.is_empty() {
            return Ok(());
        }
        let width = self.terminal.size()?.width;
        let rows = view::scrollback(lines, width);
        let height = u16::try_from(rows.len()).unwrap_or(u16::MAX);
        self.terminal.insert_before(height, |buffer| {
            for (y, row) in rows.iter().enumerate() {
                buffer.set_line(0, y as u16, row, width);
            }
        })
    }

    fn draw(&mut self, frame: &Frame) -> std::io::Result<()> {
        let width = self.terminal.size()?.width;
        self.set_height(frame.height(width))?;
        self.terminal.draw(|target| {
            let area = target.area();
            if let Some(cursor) = frame.render(area, target.buffer_mut()) {
                target.set_cursor_position(cursor);
            }
        })?;
        Ok(())
    }

    /// Clears the viewport so the shell prompt follows the last output.
    fn close(mut self) -> std::io::Result<()> {
        let top = self.terminal.get_frame().area().top();
        let backend = self.terminal.backend_mut();
        backend.set_cursor_position(Position::new(0, top))?;
        backend.clear_region(ClearType::AfterCursor)?;
        backend.show_cursor()?;
        Backend::flush(backend)
    }
}

/// The panel's row limit: at most eight, and never more than a third of
/// the window, so scrollback stays visible.
fn max_rows() -> usize {
    let height = terminal::size().map_or(24, |(_, height)| height) as usize;
    (height / 3).clamp(1, 8)
}

/// The hint line: live command suggestions while a command name is typed,
/// the last completion list, or what the prompt accepts.
fn hint(prompt: &Prompt, completion: Option<&str>) -> String {
    if let Some(text) = completion {
        return text.to_owned();
    }
    let text = prompt.text();
    if text.is_empty() {
        return "Paste a link to download it · /help for commands · Ctrl+C to leave".into();
    }
    if let Some(name) = text.strip_prefix('/')
        && !name.contains(' ')
    {
        let matches: Vec<&line::Spec> = line::COMMANDS
            .iter()
            .filter(|spec| spec.name.starts_with(&name.to_ascii_lowercase()))
            .collect();
        return match matches.as_slice() {
            [] => "No such command; /help lists them".into(),
            [spec] => format!("{}  {}", spec.usage, spec.summary),
            many => many
                .iter()
                .map(|spec| format!("/{}", spec.name))
                .collect::<Vec<_>>()
                .join("  "),
        };
    }
    "Enter runs it · Tab completes · Esc clears".into()
}

/// Completes the word before the cursor: one candidate is filled in, several
/// fill in what they share and are listed on the hint line.
fn complete(prompt: &mut Prompt, session: &Session) -> Option<String> {
    let context = Context {
        jobs: session.live.jobs(),
        settings: &session.settings,
    };
    let completion = line::complete(prompt.before_cursor(), &context);
    match completion.candidates.as_slice() {
        [] => Some("Nothing to complete here".into()),
        [only] => {
            let space = if only.finished { " " } else { "" };
            prompt.replace_before_cursor(completion.start, &format!("{}{space}", only.insert));
            None
        }
        many => {
            let shared = line::common_prefix(many);
            if shared.len() > prompt.before_cursor().len() - completion.start {
                prompt.replace_before_cursor(completion.start, &shared);
            }
            Some(
                many.iter()
                    .map(|candidate| candidate.label.as_str())
                    .collect::<Vec<_>>()
                    .join("  "),
            )
        }
    }
}

fn notice_lines(notices: Vec<Notice>, glyphs: &Glyphs) -> Vec<Out> {
    notices
        .into_iter()
        .filter_map(|notice| match notice {
            Notice::Receipt(job) => Some(view::receipt(&job, glyphs)),
            // The panel already shows state changes; only problems print.
            Notice::Event {
                name, text, loud, ..
            } => loud.then(|| Out::new(Tone::Dim, format!("  {name}: {text}"))),
        })
        .collect()
}

pub fn run(mut session: Session) -> i32 {
    let glyphs = view::glyphs_for_terminal();
    match run_view(&mut session, &glyphs) {
        Ok(code) => {
            println!("Downloads carry on in the background. Run fetchpath to come back.");
            code
        }
        Err(error) => {
            eprintln!("fetchpath: the terminal view failed: {error}");
            EXIT_ENGINE
        }
    }
}

/// Links waiting to be looked at and confirmed, one card at a time.
#[derive(Default)]
struct Reviews {
    waiting: VecDeque<Draft>,
    card: Option<Card>,
    answer: Option<Receiver<Result<Look, ProtocolError>>>,
}

impl Reviews {
    /// Shows the next link's card and looks at it on its own thread, so the
    /// view keeps moving while a media page is inspected.
    fn next(&mut self) {
        self.card = None;
        self.answer = None;
        let Some(draft) = self.waiting.pop_front() else {
            return;
        };
        let (send, receive) = mpsc::channel();
        let asked = draft.clone();
        std::thread::spawn(move || {
            let result = Engine::connect().and_then(|engine| review::look(&engine, &asked));
            let _ = send.send(result);
        });
        self.card = Some(Card::new(draft));
        self.answer = Some(receive);
    }

    /// Shows the next waiting card if none is showing.
    fn next_if_idle(&mut self) {
        if self.card.is_none() {
            self.next();
        }
    }

    fn add(&mut self, drafts: Vec<Draft>) {
        self.waiting.extend(drafts);
        if self.card.is_none() {
            self.next();
        }
    }

    /// Takes the engine's answer when it has come; true if the card changed.
    fn settle(&mut self) -> bool {
        let (Some(card), Some(answer)) = (&mut self.card, &self.answer) else {
            return false;
        };
        match answer.try_recv() {
            Ok(result) => {
                card.set(result);
                self.answer = None;
                true
            }
            Err(TryRecvError::Empty) => false,
            Err(TryRecvError::Disconnected) => {
                card.set(Err(client::input_error("The link could not be looked at.")));
                self.answer = None;
                true
            }
        }
    }
}

/// Queues a confirmed card and says where the download goes.
pub(super) fn confirmed(session: &Session, card: &Card) -> Out {
    match card.confirm(&session.engine) {
        Ok(job) => {
            let starting = job
                .not_before
                .map(|at| format!(", starting {}", crate::when::local(at)))
                .unwrap_or_default();
            let quality = job
                .quality_label
                .as_deref()
                .map(|label| format!(" ({label})"))
                .unwrap_or_default();
            Out::new(
                Tone::Normal,
                format!(
                    "Added {}{quality}  {}{starting}",
                    client::short_id(&job),
                    job.destination.as_deref().unwrap_or(&job.source_display)
                ),
            )
        }
        Err(error) => Out::new(Tone::Bad, format!("{}: {}", card.draft.link, error.message)),
    }
}

fn run_view(session: &mut Session, glyphs: &Glyphs) -> std::io::Result<i32> {
    let raw = RawMode::enable()?;
    let mut prompt = Prompt::default();
    let mut completion: Option<String> = None;
    let mut reviews = Reviews::default();
    let mut setup: Option<Setup> = None;
    let mut rows = max_rows();
    let started = Instant::now();
    let tick = || (started.elapsed().as_millis() / 100) as u64;
    let mut screen = {
        let panel = session.live.panel();
        let frame = Frame {
            panel: &panel,
            prompt: "",
            cursor: 0,
            hint: "",
            glyphs,
            max_rows: rows,
            card: None,
            tick: 0,
        };
        Screen::open(frame.height(terminal::size().map_or(80, |(width, _)| width)))?
    };
    let mut code = 0;
    let mut dirty = true;
    let mut drawn = Instant::now();
    'running: loop {
        // Animate spinners and bars while anything moves.
        let moving = reviews.card.is_some()
            || setup.is_some()
            || session.live.panel().iter().any(|row| row.group == 0);
        let every = Duration::from_millis(if moving { 100 } else { 500 });
        if dirty || drawn.elapsed() >= every {
            let panel = session.live.panel();
            let hint_text = hint(&prompt, completion.as_deref());
            screen.draw(&Frame {
                panel: &panel,
                prompt: prompt.text(),
                cursor: prompt.cursor(),
                hint: &hint_text,
                glyphs,
                max_rows: rows,
                card: match (&setup, &reviews.card) {
                    (Some(setup), _) => Some(Overlay::Setup(setup)),
                    (None, Some(card)) => Some(Overlay::Link(card)),
                    (None, None) => None,
                },
                tick: tick(),
            })?;
            dirty = false;
            drawn = Instant::now();
        }
        if event::poll(Duration::from_millis(40))? {
            match event::read()? {
                Event::Key(key) if setup.is_some() => {
                    let current = setup.as_mut().expect("checked");
                    match current.key(key) {
                        SetupAnswer::None => {}
                        SetupAnswer::Start => {
                            if let Err(error) = current.start() {
                                screen.print(&[Out::new(Tone::Bad, error.message)])?;
                                setup = None;
                                reviews.next_if_idle();
                            }
                        }
                        SetupAnswer::Cancel => {
                            screen.print(&[Out::new(
                                Tone::Dim,
                                "Video tools not set up; nothing was downloaded.",
                            )])?;
                            setup = None;
                            reviews.next_if_idle();
                        }
                    }
                    dirty = true;
                }
                Event::Key(key) if reviews.card.is_some() => {
                    let card = reviews.card.as_mut().expect("checked");
                    match card.key(key) {
                        Answer::None => {}
                        Answer::Confirm => {
                            let line = confirmed(session, card);
                            screen.print(&[line])?;
                            reviews.next();
                        }
                        Answer::Cancel => {
                            screen.print(&[Out::new(
                                Tone::Dim,
                                format!("Not downloaded: {}", card.draft.link),
                            )])?;
                            reviews.next();
                        }
                        Answer::SetUpTools => match Setup::new(Some(card.draft.clone())) {
                            Ok(opened) => {
                                // The card waits for the setup's outcome.
                                reviews.card = None;
                                setup = Some(opened);
                            }
                            Err(error) => screen.print(&[Out::new(Tone::Bad, error.message)])?,
                        },
                    }
                    dirty = true;
                }
                Event::Key(key) => match prompt.handle(key) {
                    Action::None => {}
                    Action::Leave => break 'running,
                    Action::Edited => {
                        completion = None;
                        dirty = true;
                    }
                    Action::Complete => {
                        completion = complete(&mut prompt, session);
                        dirty = true;
                    }
                    Action::Submit(text) => {
                        completion = None;
                        dirty = true;
                        if text.trim().is_empty() {
                            continue;
                        }
                        screen
                            .print(&[Out::new(Tone::Dim, format!("{} {text}", glyphs.prompt))])?;
                        let reply = session.run_line(&text);
                        screen.print(&reply.lines)?;
                        if reply.quit {
                            break 'running;
                        }
                        reviews.add(reply.drafts);
                        if reply.set_up_tools && setup.is_none() {
                            match Setup::new(None) {
                                Ok(opened) => setup = Some(opened),
                                Err(error) => {
                                    screen.print(&[Out::new(Tone::Bad, error.message)])?
                                }
                            }
                        }
                    }
                },
                Event::Paste(text) if reviews.card.is_none() && setup.is_none() => {
                    prompt.insert(&text);
                    completion = None;
                    dirty = true;
                }
                Event::Resize(..) => {
                    rows = max_rows();
                    dirty = true;
                }
                _ => {}
            }
        }
        if reviews.settle() {
            dirty = true;
        }
        if let Some(outcome) = setup.as_mut().and_then(Setup::poll) {
            let waiting = setup.take().and_then(|done| done.then);
            match outcome {
                Ok(ready) => {
                    let lines: Vec<Out> = ready
                        .into_iter()
                        .map(|line| Out::new(Tone::Good, line))
                        .collect();
                    screen.print(&lines)?;
                    // Look at the video that asked for the tools again.
                    if let Some(draft) = waiting {
                        reviews.waiting.push_front(draft);
                    }
                }
                Err(message) => screen.print(&[Out::new(Tone::Bad, message)])?,
            }
            reviews.next_if_idle();
            dirty = true;
        }
        if let Some(lines) = session.tools_notice() {
            let lines: Vec<Out> = lines
                .into_iter()
                .map(|line| Out::new(Tone::Dim, line))
                .collect();
            screen.print(&lines)?;
            dirty = true;
        }
        match session.poll(Duration::ZERO) {
            Ok(notices) => {
                if !notices.is_empty() {
                    screen.print(&notice_lines(notices, glyphs))?;
                    dirty = true;
                }
            }
            Err(error) => {
                screen.print(&[Out::new(
                    Tone::Bad,
                    format!("Lost the engine: {}", error.message),
                )])?;
                code = EXIT_ENGINE;
                break 'running;
            }
        }
    }
    screen.close()?;
    drop(raw);
    std::io::stdout().flush()?;
    Ok(code)
}
