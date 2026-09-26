//! Inline mode: the panel and prompt in a ratatui inline viewport at the
//! bottom of normal scrollback, with receipts and command output inserted
//! above it.

use super::Session;
use super::line::{self, Context, Out, Tone};
use super::live::Notice;
use super::prompt::{Action, Prompt};
use super::view::{self, Frame, Glyphs};
use crate::client::EXIT_ENGINE;
use crossterm::event::{self, Event};
use crossterm::terminal;
use ratatui::backend::{Backend, ClearType, CrosstermBackend};
use ratatui::layout::Position;
use ratatui::{Terminal, TerminalOptions, Viewport};
use std::io::{Stdout, Write};
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
        self.set_height(frame.height())?;
        self.terminal.draw(|target| {
            let area = target.area();
            let cursor = frame.render(area, target.buffer_mut());
            target.set_cursor_position(cursor);
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

fn run_view(session: &mut Session, glyphs: &Glyphs) -> std::io::Result<i32> {
    let raw = RawMode::enable()?;
    let mut prompt = Prompt::default();
    let mut completion: Option<String> = None;
    let mut rows = max_rows();
    let mut screen = {
        let panel = session.live.panel();
        let frame = Frame {
            panel: &panel,
            prompt: "",
            cursor: 0,
            hint: "",
            glyphs,
            max_rows: rows,
        };
        Screen::open(frame.height())?
    };
    let mut code = 0;
    let mut dirty = true;
    let mut drawn = Instant::now();
    'running: loop {
        if dirty || drawn.elapsed() >= Duration::from_millis(200) {
            let panel = session.live.panel();
            let hint_text = hint(&prompt, completion.as_deref());
            screen.draw(&Frame {
                panel: &panel,
                prompt: prompt.text(),
                cursor: prompt.cursor(),
                hint: &hint_text,
                glyphs,
                max_rows: rows,
            })?;
            dirty = false;
            drawn = Instant::now();
        }
        if event::poll(Duration::from_millis(40))? {
            match event::read()? {
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
                    }
                },
                Event::Paste(text) => {
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
