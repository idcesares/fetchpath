//! `/settings` in the inline view: every engine setting as a menu row.
//! Enter or a click flips a switch or cycles a choice, ←/→ change a number,
//! a folder opens an edit line with Tab completion. Each change is sent to
//! the engine at once and the row shows what the engine kept.

use super::line::{self, Context};
use super::menu::{Item, Menu, Pick};
use super::prompt::{Action, Prompt};
use crate::client::{self, Engine};
use crate::download;
use fetchpath_protocol::ProtocolError;
use fetchpath_protocol::command::Command;
use fetchpath_protocol::message::CommandResult;
use fetchpath_protocol::model::{EngineSettings, SettingsView, Theme};
use std::path::Path;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Field {
    Heading,
    MaxActive,
    Folder,
    AutoRetry,
    Attempts,
    Delay,
    SignIn,
    VideoTools,
    ToolsFolder,
    Theme,
    Tray,
    ConfirmRemove,
    PowerMode,
}

/// Delays offered for the first retry, in seconds.
const DELAYS: &[u64] = &[5, 10, 15, 30, 60, 120, 300, 600];

/// What the settings menu asks of the view.
#[derive(Debug, Eq, PartialEq)]
pub enum Effect {
    None,
    Close,
    SetUpTools,
}

pub struct SettingsMenu {
    pub menu: Menu,
    view: SettingsView,
    fields: Vec<Field>,
    tools_ready: Option<bool>,
    /// A folder being typed, and which setting it is for.
    editing: Option<(Field, Prompt)>,
}

fn switch(on: bool) -> &'static str {
    if on { "on" } else { "off" }
}

fn theme_name(theme: Theme) -> &'static str {
    match theme {
        Theme::System => "match Windows",
        Theme::Light => "light",
        Theme::Dark => "dark",
        Theme::HighContrast => "high contrast",
        Theme::Unknown => "unknown",
    }
}

impl SettingsMenu {
    pub fn open(engine: &Engine, tools_ready: Option<bool>) -> Result<Self, ProtocolError> {
        let view = match engine.send(Command::GetSettings)? {
            CommandResult::Settings { view } => view,
            other => return Err(client::unexpected(&other)),
        };
        let mut this = Self {
            menu: Menu::new(
                "Settings",
                Vec::new(),
                "↑↓ choose · Enter change · ←→ adjust · Esc close",
            ),
            view,
            fields: Vec::new(),
            tools_ready,
            editing: None,
        };
        this.rebuild();
        this.menu.select(1, 1);
        Ok(this)
    }

    fn rebuild(&mut self) {
        let s = &self.view.settings;
        let folder =
            |dir: &Option<String>, none: &str| dir.clone().unwrap_or_else(|| none.to_owned());
        let heading = |text: &str| Item {
            inert: true,
            ..Item::new(text, "", "")
        };
        let rows: Vec<(Field, Item)> = vec![
            (Field::Heading, heading("Downloads")),
            (
                Field::MaxActive,
                Item::new(
                    "Downloads at the same time",
                    s.max_active_downloads.to_string(),
                    format!("←/→ between 1 and {}", self.view.max_active_limit),
                ),
            ),
            (
                Field::Folder,
                Item::new(
                    "Default save folder",
                    folder(&s.default_destination_dir, "your Downloads folder"),
                    "Enter to type a folder; leave it empty for your Downloads folder",
                ),
            ),
            (
                Field::AutoRetry,
                Item::new(
                    "Retry connection problems automatically",
                    switch(s.auto_retry),
                    "Enter to switch",
                ),
            ),
            (
                Field::Attempts,
                Item::new(
                    "Attempts before giving up",
                    s.auto_retry_max_attempts.to_string(),
                    format!("←/→ between 0 and {}", self.view.max_retry_attempts),
                ),
            ),
            (
                Field::Delay,
                Item::new(
                    "Wait before the first retry",
                    format!("{} s", s.auto_retry_base_delay_seconds),
                    "←/→ to change; each later retry waits twice as long",
                ),
            ),
            (
                Field::SignIn,
                Item::new(
                    "Start at Windows sign-in (keeps schedules)",
                    switch(s.start_engine_at_sign_in.unwrap_or(false)),
                    "Enter to switch",
                ),
            ),
            (Field::Heading, heading("Video and audio")),
            (
                Field::VideoTools,
                Item::new(
                    "yt-dlp and ffmpeg",
                    match self.tools_ready {
                        Some(true) => "ready",
                        Some(false) => "not set up",
                        None => "checking",
                    },
                    "Enter to see what would be downloaded and set them up",
                ),
            ),
            (
                Field::ToolsFolder,
                Item::new(
                    "Tools folder",
                    folder(&s.media_tools_dir, "found automatically"),
                    "Enter to type the folder holding yt-dlp and ffmpeg",
                ),
            ),
            (Field::Heading, heading("Desktop app")),
            (
                Field::Theme,
                Item::new("Appearance", theme_name(s.theme), "Enter or ←/→ to choose"),
            ),
            (
                Field::Tray,
                Item::new(
                    "Keep running in the notification area",
                    switch(s.close_to_tray),
                    "Enter to switch",
                ),
            ),
            (
                Field::ConfirmRemove,
                Item::new(
                    "Ask before removing a finished download",
                    switch(s.confirm_remove_completed),
                    "Enter to switch",
                ),
            ),
            (
                Field::PowerMode,
                Item::new(
                    "Power mode: transfer statistics",
                    switch(s.power_mode),
                    "Enter to switch",
                ),
            ),
        ];
        let selected = self.menu.selected;
        let offset = self.menu.offset;
        self.fields = rows.iter().map(|(field, _)| *field).collect();
        self.menu.items = rows.into_iter().map(|(_, item)| item).collect();
        self.menu.selected = selected;
        self.menu.offset = offset;
        if self.view.repaired && self.menu.note.is_none() {
            self.menu.note = Some("The stored settings were unusable; defaults are shown.".into());
        }
    }

    /// The folder being typed, with what it is for.
    pub fn editing(&self) -> Option<(&'static str, &Prompt)> {
        self.editing.as_ref().map(|(field, prompt)| {
            let label = if *field == Field::Folder {
                "Save folder"
            } else {
                "Tools folder"
            };
            (label, prompt)
        })
    }

    pub fn set_tools_ready(&mut self, ready: bool) {
        self.tools_ready = Some(ready);
        self.rebuild();
    }

    /// Sends a changed copy of the settings and shows what the engine kept.
    fn apply(&mut self, engine: &Engine, change: impl FnOnce(&mut EngineSettings)) {
        let mut next = self.view.settings.clone();
        change(&mut next);
        match engine.send(Command::UpdateSettings { settings: next }) {
            Ok(CommandResult::Settings { view }) => {
                self.view = view;
                self.menu.note = Some("Saved.".into());
            }
            Ok(other) => self.menu.note = Some(client::unexpected(&other).message),
            Err(error) => self.menu.note = Some(error.message),
        }
        self.rebuild();
    }

    fn adjust(&mut self, engine: &Engine, field: Field, by: i64) {
        let limit = self.view.max_active_limit;
        let attempts = self.view.max_retry_attempts;
        match field {
            Field::MaxActive => self.apply(engine, |s| {
                s.max_active_downloads =
                    (s.max_active_downloads as i64 + by).clamp(1, limit as i64) as u64;
            }),
            Field::Attempts => self.apply(engine, |s| {
                s.auto_retry_max_attempts =
                    (s.auto_retry_max_attempts as i64 + by).clamp(0, attempts as i64) as u32;
            }),
            Field::Delay => self.apply(engine, |s| {
                let at = DELAYS
                    .iter()
                    .position(|delay| *delay >= s.auto_retry_base_delay_seconds)
                    .unwrap_or(DELAYS.len() - 1) as i64;
                s.auto_retry_base_delay_seconds =
                    DELAYS[(at + by).clamp(0, DELAYS.len() as i64 - 1) as usize];
            }),
            Field::Theme => self.apply(engine, |s| {
                let themes = [Theme::System, Theme::Light, Theme::Dark, Theme::HighContrast];
                let at = themes
                    .iter()
                    .position(|theme| *theme == s.theme)
                    .unwrap_or(0) as i64;
                s.theme = themes[(at + by).rem_euclid(themes.len() as i64) as usize];
            }),
            // Switches flip either way.
            Field::AutoRetry
            | Field::SignIn
            | Field::Tray
            | Field::ConfirmRemove
            | Field::PowerMode => {
                self.choose(engine, field);
            }
            _ => {}
        }
    }

    fn choose(&mut self, engine: &Engine, field: Field) -> Effect {
        match field {
            Field::AutoRetry => self.apply(engine, |s| s.auto_retry = !s.auto_retry),
            Field::SignIn => self.apply(engine, |s| {
                s.start_engine_at_sign_in = Some(!s.start_engine_at_sign_in.unwrap_or(false));
            }),
            Field::Tray => self.apply(engine, |s| s.close_to_tray = !s.close_to_tray),
            Field::ConfirmRemove => self.apply(engine, |s| {
                s.confirm_remove_completed = !s.confirm_remove_completed;
            }),
            Field::PowerMode => self.apply(engine, |s| s.power_mode = !s.power_mode),
            Field::MaxActive | Field::Attempts | Field::Delay | Field::Theme => {
                self.adjust(engine, field, 1);
            }
            Field::Folder | Field::ToolsFolder => {
                let current = match field {
                    Field::Folder => self.view.settings.default_destination_dir.clone(),
                    _ => self.view.settings.media_tools_dir.clone(),
                };
                let mut prompt = Prompt::default();
                prompt.set(current.unwrap_or_default());
                self.editing = Some((field, prompt));
            }
            Field::VideoTools => return Effect::SetUpTools,
            Field::Heading => {}
        }
        Effect::None
    }

    fn pick(&mut self, engine: &Engine, pick: Pick) -> Effect {
        match pick {
            Pick::None => Effect::None,
            Pick::Close => Effect::Close,
            Pick::Choose(index) => self.choose(engine, self.fields[index]),
            Pick::Adjust(index, by) => {
                self.adjust(engine, self.fields[index], by);
                Effect::None
            }
        }
    }

    /// Saves a typed folder: empty clears it; anything else must be a
    /// folder that exists.
    fn save_folder(&mut self, engine: &Engine, field: Field, text: &str) {
        let text = text.trim().trim_matches('"');
        let value = if text.is_empty() {
            None
        } else {
            match download::absolute(Path::new(text)) {
                Ok(path) if path.is_dir() => Some(path.display().to_string()),
                Ok(path) => {
                    self.menu.note = Some(format!("{} is not a folder.", path.display()));
                    return;
                }
                Err(message) => {
                    self.menu.note = Some(message);
                    return;
                }
            }
        };
        if field == Field::ToolsFolder
            && let Some(folder) = &value
            && let Err(message) = fetchpath_media::setup::use_directory(Path::new(folder))
        {
            self.menu.note = Some(message);
            return;
        }
        self.apply(engine, |s| match field {
            Field::Folder => s.default_destination_dir = value,
            _ => s.media_tools_dir = value,
        });
    }

    pub fn key(
        &mut self,
        engine: &Engine,
        key: crossterm::event::KeyEvent,
        context: &Context,
    ) -> Effect {
        if let Some((field, prompt)) = &mut self.editing {
            let field = *field;
            match prompt.handle(key) {
                Action::Submit(text) => {
                    self.editing = None;
                    self.save_folder(engine, field, &text);
                }
                Action::Leave => self.editing = None,
                Action::Complete => {
                    let word = prompt.before_cursor().to_owned();
                    let found = line::folders(&word, context);
                    match found.as_slice() {
                        [only] => prompt.replace_before_cursor(0, &only.label),
                        [] => {}
                        many => {
                            let shared = line::common_prefix(many);
                            if shared.len() > word.len() {
                                prompt.replace_before_cursor(0, shared.trim_matches('"'));
                            }
                        }
                    }
                }
                _ if key.code == crossterm::event::KeyCode::Esc => self.editing = None,
                _ => {}
            }
            return Effect::None;
        }
        let pick = self.menu.key(key);
        self.pick(engine, pick)
    }

    pub fn mouse(
        &mut self,
        engine: &Engine,
        event: crossterm::event::MouseEvent,
        row_of: impl Fn(u16) -> Option<usize>,
    ) -> Effect {
        if self.editing.is_some() {
            return Effect::None;
        }
        let pick = self.menu.mouse(event, row_of);
        self.pick(engine, pick)
    }
}
