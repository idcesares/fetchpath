//! Inline mode: the panel and prompt in a ratatui inline viewport at the
//! bottom of normal scrollback, with receipts and command output inserted
//! above it.

use super::Session;
use super::jobs_menu::{Effect as JobsEffect, JobsMenu};
use super::line::Open;
use super::line::{self, Context, Out, Tone};
use super::live::Notice;
use super::prompt::{Action, Prompt};
use super::review::{self, Answer, Card, Draft, Look};
use super::settings_menu::{Effect as SettingsEffect, SettingsMenu};
use super::setup::{Answer as SetupAnswer, Setup};
use super::view::{self, Drawn, Frame, Glyphs, Overlay, Target};
use crate::client::{self, EXIT_ENGINE, Engine};
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, MouseButton,
    MouseEventKind,
};
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

    fn draw(&mut self, frame: &Frame) -> std::io::Result<Drawn> {
        let width = self.terminal.size()?.width;
        self.set_height(frame.height(width))?;
        let mut drawn = Drawn::default();
        self.terminal.draw(|target| {
            let area = target.area();
            drawn = frame.render(area, target.buffer_mut());
            if let Some(cursor) = drawn.cursor {
                target.set_cursor_position(cursor);
            }
        })?;
        Ok(drawn)
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

/// Commands to offer while a command name is typed: every command whose
/// name starts with it, as Claude Code lists them.
fn palette(prompt: &Prompt) -> Vec<(&'static str, &'static str)> {
    let text = prompt.text();
    let Some(typed) = text.strip_prefix('/') else {
        return Vec::new();
    };
    if typed.contains(char::is_whitespace) || prompt.cursor() != text.len() {
        return Vec::new();
    }
    let typed = typed.to_ascii_lowercase();
    line::COMMANDS
        .iter()
        .filter(|spec| spec.name.starts_with(&typed))
        .map(|spec| (spec.name, spec.summary))
        .collect()
}

/// Most commands listed under the prompt at once.
const COMMAND_ROWS: usize = 8;

/// What choosing a command from the list does: one that needs words gets
/// them typed after it; any other runs at once.
enum Chosen {
    Type(String),
    Run(String),
}

fn choose_command(name: &str) -> Chosen {
    match line::find(name) {
        Some(spec) if spec.needs_words() => Chosen::Type(format!("/{name} ")),
        _ => Chosen::Run(format!("/{name}")),
    }
}

/// Everything below the panel that can hold the keyboard, most urgent
/// first.
#[derive(Default)]
struct Menus {
    setup: Option<Setup>,
    settings: Option<SettingsMenu>,
    jobs: Option<JobsMenu>,
}

impl Menus {
    fn any(&self) -> bool {
        self.setup.is_some() || self.settings.is_some() || self.jobs.is_some()
    }
}

/// Runs a typed or chosen line: echoes it, prints what it says, and opens
/// what it asks for. Returns true to leave.
fn submit(
    text: &str,
    session: &mut Session,
    screen: &mut Screen,
    reviews: &mut Reviews,
    menus: &mut Menus,
    glyphs: &Glyphs,
) -> std::io::Result<bool> {
    screen.print(&[Out::new(Tone::Dim, format!("{} {text}", glyphs.prompt))])?;
    let reply = session.run_line(text, true);
    screen.print(&reply.lines)?;
    if reply.quit {
        return Ok(true);
    }
    reviews.add(reply.drafts);
    if reply.set_up_tools && menus.setup.is_none() {
        match Setup::new(None) {
            Ok(opened) => menus.setup = Some(opened),
            Err(error) => screen.print(&[Out::new(Tone::Bad, error.message)])?,
        }
    }
    match reply.open {
        Some(Open::Settings) => match SettingsMenu::open(&session.engine, session.tools_ready) {
            Ok(menu) => menus.settings = Some(menu),
            Err(error) => screen.print(&[Out::new(Tone::Bad, error.message)])?,
        },
        Some(Open::Jobs(command)) => menus.jobs = Some(JobsMenu::open(&session.live, command)),
        None => {}
    }
    Ok(false)
}

/// Turns the mouse on while something below the panel takes clicks, and
/// off otherwise, so the terminal's own scrolling and text selection work
/// at the prompt.
fn set_mouse(on: bool, captured: &mut bool) -> std::io::Result<()> {
    if on != *captured {
        if on {
            crossterm::execute!(std::io::stdout(), EnableMouseCapture)?;
        } else {
            crossterm::execute!(std::io::stdout(), DisableMouseCapture)?;
        }
        *captured = on;
    }
    Ok(())
}

fn run_view(session: &mut Session, glyphs: &Glyphs) -> std::io::Result<i32> {
    let raw = RawMode::enable()?;
    let mut prompt = Prompt::default();
    let mut completion: Option<String> = None;
    let mut reviews = Reviews::default();
    let mut menus = Menus::default();
    let mut chosen_command = 0_usize;
    let mut drawn = Drawn::default();
    let mut mouse = false;
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
            palette: &[],
            palette_selected: 0,
        };
        Screen::open(frame.height(terminal::size().map_or(80, |(width, _)| width)))?
    };
    let mut code = 0;
    let mut dirty = true;
    let mut last_draw = Instant::now();
    'running: loop {
        let commands = if menus.any() || reviews.card.is_some() {
            Vec::new()
        } else {
            palette(&prompt)
        };
        chosen_command = chosen_command.min(commands.len().saturating_sub(1));
        // The list shows a window of commands that follows the selection.
        let first_command = chosen_command.saturating_sub(COMMAND_ROWS - 1);
        set_mouse(
            menus.any() || reviews.card.is_some() || !commands.is_empty(),
            &mut mouse,
        )?;
        // Animate spinners and bars while anything moves.
        let moving = reviews.card.is_some()
            || menus.setup.is_some()
            || menus.jobs.is_some()
            || session.live.panel().iter().any(|row| row.group == 0);
        let every = Duration::from_millis(if moving { 100 } else { 500 });
        if dirty || last_draw.elapsed() >= every {
            if let Some(jobs) = &mut menus.jobs {
                jobs.refresh(&session.live);
            }
            let panel = session.live.panel();
            let hint_text = hint(&prompt, completion.as_deref());
            let editing = menus.settings.as_ref().and_then(SettingsMenu::editing);
            let overlay = if let Some(setup) = &menus.setup {
                Some(Overlay::Setup(setup))
            } else if let Some(settings) = &menus.settings {
                Some(Overlay::Menu(&settings.menu, editing))
            } else if let Some(jobs) = &menus.jobs {
                Some(Overlay::Menu(&jobs.menu, None))
            } else {
                reviews.card.as_ref().map(Overlay::Link)
            };
            drawn = screen.draw(&Frame {
                panel: &panel,
                prompt: prompt.text(),
                cursor: prompt.cursor(),
                hint: &hint_text,
                glyphs,
                max_rows: rows,
                card: overlay,
                tick: tick(),
                palette: &commands
                    [first_command..(first_command + COMMAND_ROWS).min(commands.len())],
                palette_selected: chosen_command - first_command,
            })?;
            dirty = false;
            last_draw = Instant::now();
        }
        if event::poll(Duration::from_millis(40))? {
            let input = event::read()?;
            dirty = true;
            let row_of = |row: u16| match drawn.target_at(row) {
                Some(Target::Item(index)) => Some(index),
                _ => None,
            };
            match input {
                // The setup card holds the keyboard until it ends.
                Event::Key(key) if menus.setup.is_some() => {
                    let current = menus.setup.as_mut().expect("checked");
                    match current.key(key) {
                        SetupAnswer::None => {}
                        SetupAnswer::Start => {
                            if let Err(error) = current.start() {
                                screen.print(&[Out::new(Tone::Bad, error.message)])?;
                                menus.setup = None;
                                reviews.next_if_idle();
                            }
                        }
                        SetupAnswer::Cancel => {
                            screen.print(&[Out::new(
                                Tone::Dim,
                                "Video tools not set up; nothing was downloaded.",
                            )])?;
                            menus.setup = None;
                            reviews.next_if_idle();
                        }
                    }
                }
                Event::Mouse(_) if menus.setup.is_some() => {}
                Event::Key(key) if menus.settings.is_some() => {
                    let context = Context {
                        jobs: session.live.jobs(),
                        settings: &session.settings,
                    };
                    let menu = menus.settings.as_mut().expect("checked");
                    match menu.key(&session.engine, key, &context) {
                        SettingsEffect::None => {}
                        SettingsEffect::Close => menus.settings = None,
                        SettingsEffect::SetUpTools => match Setup::new(None) {
                            Ok(opened) => menus.setup = Some(opened),
                            Err(error) => screen.print(&[Out::new(Tone::Bad, error.message)])?,
                        },
                    }
                }
                Event::Mouse(event) if menus.settings.is_some() => {
                    let menu = menus.settings.as_mut().expect("checked");
                    match menu.mouse(&session.engine, event, row_of) {
                        SettingsEffect::None => {}
                        SettingsEffect::Close => menus.settings = None,
                        SettingsEffect::SetUpTools => match Setup::new(None) {
                            Ok(opened) => menus.setup = Some(opened),
                            Err(error) => screen.print(&[Out::new(Tone::Bad, error.message)])?,
                        },
                    }
                }
                Event::Key(_) | Event::Mouse(_) if menus.jobs.is_some() => {
                    let menu = menus.jobs.as_mut().expect("checked");
                    let pick = match input {
                        Event::Key(key) => menu.menu.key(key),
                        Event::Mouse(event) => menu.menu.mouse(event, row_of),
                        _ => unreachable!("only keys and the mouse reach here"),
                    };
                    match menu.pick(&session.engine, &session.live, pick) {
                        JobsEffect::None => {}
                        JobsEffect::Close => menus.jobs = None,
                        JobsEffect::Say { lines, close } => {
                            screen.print(&lines)?;
                            if close {
                                menus.jobs = None;
                            }
                        }
                    }
                }
                Event::Mouse(event) if reviews.card.is_some() => {
                    let card = reviews.card.as_mut().expect("checked");
                    match event.kind {
                        MouseEventKind::Down(MouseButton::Left) => {
                            if let Some(Target::Choice(choice)) = drawn.target_at(event.row) {
                                card.choose(choice);
                            }
                        }
                        MouseEventKind::ScrollUp => card.choose(card.choice.saturating_sub(1)),
                        MouseEventKind::ScrollDown => card.choose(card.choice + 1),
                        _ => {}
                    }
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
                                menus.setup = Some(opened);
                            }
                            Err(error) => screen.print(&[Out::new(Tone::Bad, error.message)])?,
                        },
                    }
                }
                // The command list under the prompt takes the arrows, Tab,
                // Enter and clicks while it shows.
                Event::Mouse(event) if !commands.is_empty() => match event.kind {
                    MouseEventKind::Down(MouseButton::Left) => {
                        if let Some(Target::Command(index)) = drawn.target_at(event.row) {
                            match choose_command(commands[first_command + index].0) {
                                Chosen::Type(text) => prompt.set(text),
                                Chosen::Run(text) => {
                                    prompt.set(String::new());
                                    if submit(
                                        &text,
                                        session,
                                        &mut screen,
                                        &mut reviews,
                                        &mut menus,
                                        glyphs,
                                    )? {
                                        break 'running;
                                    }
                                }
                            }
                        }
                    }
                    MouseEventKind::ScrollUp => chosen_command = chosen_command.saturating_sub(1),
                    MouseEventKind::ScrollDown => chosen_command += 1,
                    _ => {}
                },
                Event::Key(key)
                    if !commands.is_empty()
                        && key.kind != KeyEventKind::Release
                        && matches!(
                            key.code,
                            KeyCode::Up | KeyCode::Down | KeyCode::Tab | KeyCode::Enter
                        ) =>
                {
                    let name = commands[chosen_command].0;
                    match key.code {
                        KeyCode::Up => chosen_command = chosen_command.saturating_sub(1),
                        KeyCode::Down => chosen_command += 1,
                        KeyCode::Tab => prompt.set(format!("/{name} ")),
                        _ => match choose_command(name) {
                            Chosen::Type(text) => prompt.set(text),
                            Chosen::Run(text) => {
                                prompt.set(String::new());
                                if submit(
                                    &text,
                                    session,
                                    &mut screen,
                                    &mut reviews,
                                    &mut menus,
                                    glyphs,
                                )? {
                                    break 'running;
                                }
                            }
                        },
                    }
                }
                Event::Key(key) => match prompt.handle(key) {
                    Action::None => dirty = false,
                    Action::Leave => break 'running,
                    Action::Edited => {
                        completion = None;
                        chosen_command = 0;
                    }
                    Action::Complete => completion = complete(&mut prompt, session),
                    Action::Submit(text) => {
                        completion = None;
                        if !text.trim().is_empty()
                            && submit(
                                &text,
                                session,
                                &mut screen,
                                &mut reviews,
                                &mut menus,
                                glyphs,
                            )?
                        {
                            break 'running;
                        }
                    }
                },
                Event::Paste(text) if reviews.card.is_none() && !menus.any() => {
                    prompt.insert(&text);
                    completion = None;
                }
                Event::Resize(..) => rows = max_rows(),
                _ => dirty = false,
            }
        }
        if reviews.settle() {
            dirty = true;
        }
        if let Some(outcome) = menus.setup.as_mut().and_then(Setup::poll) {
            let waiting = menus.setup.take().and_then(|done| done.then);
            match outcome {
                Ok(ready) => {
                    session.tools_ready = Some(true);
                    if let Some(settings) = &mut menus.settings {
                        settings.set_tools_ready(true);
                    }
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
            if let Some(settings) = &mut menus.settings {
                settings.set_tools_ready(session.tools_ready == Some(true));
            }
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
    set_mouse(false, &mut mouse)?;
    screen.close()?;
    drop(raw);
    std::io::stdout().flush()?;
    Ok(code)
}
