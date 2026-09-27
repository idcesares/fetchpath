//! The terminal's own configuration (FP-062): `cli.toml` in the data folder
//! holds the theme, glyph set, density, keybindings and aliases. The engine's
//! settings stay in `/settings`; nothing here reaches the engine.
//!
//! A missing file means the defaults. An entry that cannot be used is
//! reported with its line number and skipped, never fatal. `/theme`, `/keys`
//! and `/alias` rewrite only the line they change and apply at once.

pub mod alias;
pub mod file;
pub mod keys;
pub mod theme;

use super::line::{Out, Reply, Tone};
use super::view::{self, Glyphs};
use alias::Aliases;
use file::{Problem, Value};
use keys::Keys;
use std::path::{Path, PathBuf};
use theme::Theme;

pub const FILE_NAME: &str = "cli.toml";

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum GlyphSet {
    /// Unicode where the terminal is known to draw it, ASCII elsewhere.
    #[default]
    Auto,
    Unicode,
    Ascii,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Density {
    #[default]
    Comfortable,
    /// A shorter panel, and no hint line under an empty prompt.
    Compact,
}

#[derive(Clone, Debug, Default)]
pub struct Config {
    pub theme: Theme,
    pub glyphs: GlyphSet,
    pub density: Density,
    pub keys: Keys,
    pub aliases: Aliases,
    /// Where the file is, when the data folder is known.
    pub path: Option<PathBuf>,
}

/// `cli.toml` in the engine's data folder, beside the queue and settings:
/// `%APPDATA%\app.fetchpath.desktop`, or `FETCHPATH_APP_DATA_DIR`.
pub fn default_path() -> Option<PathBuf> {
    fetchpath_protocol::launch::EngineHome::from_env()
        .ok()
        .map(|home| home.dir().join(FILE_NAME))
}

fn is_command(name: &str) -> bool {
    super::line::find(name).is_some()
}

impl Config {
    /// Reads the file at `path`; a missing file is the defaults.
    pub fn load(path: Option<PathBuf>) -> (Self, Vec<Problem>) {
        let text = match &path {
            Some(path) => match std::fs::read_to_string(path) {
                Ok(text) => text,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
                Err(error) => {
                    let config = Self {
                        path: Some(path.clone()),
                        ..Self::default()
                    };
                    return (
                        config,
                        vec![Problem::new(
                            0,
                            format!("could not be read ({error}); using the defaults"),
                        )],
                    );
                }
            },
            None => String::new(),
        };
        let (mut config, problems) = Self::from_text(&text);
        config.path = path;
        (config, problems)
    }

    pub fn from_text(text: &str) -> (Self, Vec<Problem>) {
        let (entries, mut problems) = file::parse(text);
        let mut config = Self::default();
        let mut colors = Vec::new();
        let mut chosen_keys = Vec::new();
        let mut key_lines = Vec::new();
        let mut given_aliases = Vec::new();
        let mut alias_lines = Vec::new();
        for entry in entries {
            let line = entry.line;
            let text = match &entry.value {
                Value::Text(text) => Some(text.as_str()),
                Value::List(_) => None,
            };
            let mut refuse = |message: String| problems.push(Problem::new(line, message));
            match (entry.section.as_str(), entry.key.as_str()) {
                ("", "theme") => match text.and_then(Theme::named) {
                    Some(theme) => config.theme = theme,
                    None => refuse(format!("theme must be one of {}", theme_names())),
                },
                ("", "glyphs") => match text.map(str::to_ascii_lowercase).as_deref() {
                    Some("auto") => config.glyphs = GlyphSet::Auto,
                    Some("unicode") => config.glyphs = GlyphSet::Unicode,
                    Some("ascii") => config.glyphs = GlyphSet::Ascii,
                    _ => refuse("glyphs must be auto, unicode or ascii".into()),
                },
                ("", "density") => match text.map(str::to_ascii_lowercase).as_deref() {
                    Some("comfortable") => config.density = Density::Comfortable,
                    Some("compact") => config.density = Density::Compact,
                    _ => refuse("density must be comfortable or compact".into()),
                },
                ("colors", role) => match text.map(theme::parse_color) {
                    Some(Ok(color)) if theme::ROLES.iter().any(|(name, _)| *name == role) => {
                        colors.push((role.to_owned(), color));
                    }
                    Some(Err(message)) => refuse(message),
                    _ => refuse(format!(
                        "[colors] names {}, each set to one color",
                        theme::ROLES.map(|(name, _)| name).join(", ")
                    )),
                },
                ("keys", act) => match text {
                    Some(key) => {
                        chosen_keys.push((act.to_owned(), key.to_owned()));
                        key_lines.push(line);
                    }
                    None => refuse("a key is one text, such as \"p\" or \"F5\"".into()),
                },
                ("aliases", name) => {
                    let lines = match entry.value {
                        Value::Text(text) => vec![text],
                        Value::List(lines) => lines,
                    };
                    given_aliases.push((name.to_owned(), lines));
                    alias_lines.push(line);
                }
                ("", key) => refuse(format!(
                    "{key} is not a setting here; the file holds theme, glyphs, density, [colors], [keys] and [aliases]"
                )),
                (section, _) => refuse(format!(
                    "[{section}] is not a section here; they are [colors], [keys] and [aliases]"
                )),
            }
        }
        // Overrides apply after the theme, wherever the theme line is.
        for (role, color) in colors {
            config.theme.set(&role, color);
        }
        let (keys, key_problems) = Keys::build(&chosen_keys);
        config.keys = keys;
        problems.extend(
            key_problems
                .into_iter()
                .map(|(index, message)| Problem::new(key_lines[index], message)),
        );
        let (aliases, alias_problems) = Aliases::build(&given_aliases, &is_command);
        config.aliases = aliases;
        problems.extend(
            alias_problems
                .into_iter()
                .map(|(index, message)| Problem::new(alias_lines[index], message)),
        );
        problems.sort_by_key(|problem| problem.line);
        (config, problems)
    }

    pub fn glyphs(&self) -> Glyphs {
        match self.glyphs {
            GlyphSet::Auto => view::glyphs_for_terminal(),
            GlyphSet::Unicode => view::UNICODE,
            GlyphSet::Ascii => view::ASCII,
        }
    }

    /// The panel's row limit for a window height: at most eight rows (four
    /// when compact), never more than a third of the window.
    pub fn max_rows(&self, height: u16) -> usize {
        let most = match self.density {
            Density::Comfortable => 8,
            Density::Compact => 4,
        };
        (usize::from(height) / 3).clamp(1, most)
    }

    /// Where the file is, for messages.
    pub fn where_(&self) -> String {
        self.path
            .as_deref()
            .map_or_else(|| FILE_NAME.to_owned(), |path| path.display().to_string())
    }

    /// Changes one entry in the file and loads it again, so what applies is
    /// always what the file says. Lines the person wrote are kept. Returns
    /// only problems the change brought; the rest were reported at start.
    fn change(
        &mut self,
        section: &str,
        key: &str,
        value: Option<&Value>,
    ) -> Result<Vec<Problem>, String> {
        let Some(path) = self.path.clone() else {
            return Err("There is no data folder to keep cli.toml in (APPDATA is not set).".into());
        };
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(error) => return Err(format!("{} could not be read: {error}", path.display())),
        };
        let before: Vec<String> = Self::from_text(&text)
            .1
            .into_iter()
            .map(|problem| problem.message)
            .collect();
        let changed = file::edit(&text, section, key, value);
        write(&path, &changed)
            .map_err(|error| format!("{} could not be saved: {error}", path.display()))?;
        let (config, problems) = Self::load(Some(path));
        *self = config;
        Ok(problems
            .into_iter()
            .filter(|problem| !before.contains(&problem.message))
            .collect())
    }
}

/// Writes the whole file beside itself, then moves it into place, so a
/// failure never leaves half a file.
fn write(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(folder) = path.parent() {
        std::fs::create_dir_all(folder)?;
    }
    let temporary = path.with_extension("toml.new");
    std::fs::write(&temporary, text)?;
    std::fs::rename(&temporary, path)
}

pub fn theme_names() -> String {
    theme::BUILT_IN
        .iter()
        .map(|theme| theme.name)
        .collect::<Vec<_>>()
        .join(", ")
}

/// Problems as lines to print at start or after a change.
pub fn problem_lines(config: &Config, problems: &[Problem]) -> Vec<Out> {
    problems
        .iter()
        .map(|problem| {
            let place = if problem.line == 0 {
                config.where_()
            } else {
                format!("{} line {}", config.where_(), problem.line)
            };
            Out::new(Tone::Bad, format!("{place}: {}; ignored.", problem.message))
        })
        .collect()
}

/// `/theme`, `/keys` and `/alias`. Returns `None` for other commands.
pub fn command(config: &mut Config, name: &str, args: &[String]) -> Option<Reply> {
    let mut reply = Reply::default();
    let result = match name {
        "theme" => theme_command(config, args, &mut reply),
        "keys" => keys_command(config, args, &mut reply),
        "alias" => alias_command(config, args, &mut reply),
        _ => return None,
    };
    match result {
        Ok(problems) => reply.lines.extend(problem_lines(config, &problems)),
        Err(message) => reply.lines.push(Out::new(Tone::Bad, message)),
    }
    Some(reply)
}

fn say(reply: &mut Reply, tone: Tone, text: impl Into<String>) {
    reply.lines.push(Out::new(tone, text));
}

fn theme_command(
    config: &mut Config,
    args: &[String],
    reply: &mut Reply,
) -> Result<Vec<Problem>, String> {
    let words: Vec<String> = args.iter().map(|arg| arg.to_ascii_lowercase()).collect();
    match words
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        [] => {
            say(
                reply,
                Tone::Normal,
                format!(
                    "Theme {}, glyphs {}, density {}.",
                    config.theme.name,
                    glyph_name(config.glyphs),
                    density_name(config.density)
                ),
            );
            for theme in &theme::BUILT_IN {
                say(
                    reply,
                    Tone::Normal,
                    format!("  {:<15}{}", theme.name, theme.summary),
                );
            }
            say(
                reply,
                Tone::Dim,
                "/theme NAME changes it; /theme glyphs auto|unicode|ascii; /theme density comfortable|compact.",
            );
            Ok(Vec::new())
        }
        ["glyphs", choice @ ("auto" | "unicode" | "ascii")] => {
            let problems = config.change("", "glyphs", Some(&Value::Text((*choice).to_owned())))?;
            say(reply, Tone::Good, format!("Glyphs set to {choice}."));
            Ok(problems)
        }
        ["density", choice @ ("comfortable" | "compact")] => {
            let problems =
                config.change("", "density", Some(&Value::Text((*choice).to_owned())))?;
            say(reply, Tone::Good, format!("Density set to {choice}."));
            Ok(problems)
        }
        ["glyphs", ..] => Err("Say /theme glyphs auto, unicode or ascii.".into()),
        ["density", ..] => Err("Say /theme density comfortable or compact.".into()),
        [name] => {
            let Some(theme) = Theme::named(name) else {
                return Err(format!(
                    "There is no {name} theme; they are {}.",
                    theme_names()
                ));
            };
            let problems = config.change("", "theme", Some(&Value::Text(theme.name.to_owned())))?;
            say(reply, Tone::Good, format!("Theme set to {}.", theme.name));
            Ok(problems)
        }
        _ => Err("Usage: /theme [NAME | glyphs SET | density comfortable|compact]".into()),
    }
}

fn keys_command(
    config: &mut Config,
    args: &[String],
    reply: &mut Reply,
) -> Result<Vec<Problem>, String> {
    match args {
        [] => {
            say(
                reply,
                Tone::Normal,
                "Keys for the dashboard, and the one that opens it from the prompt:",
            );
            for line in config.keys.lines() {
                say(reply, Tone::Normal, line);
            }
            say(
                reply,
                Tone::Dim,
                "/keys ACTION KEY changes one; /keys ACTION default puts it back. Arrows, Enter, Esc and Delete always work.",
            );
            Ok(Vec::new())
        }
        [action, key] => {
            let Some(act) = keys::act_named(action) else {
                return Err(format!("There is no {action} action; /keys lists them."));
            };
            let name = keys::act_name(act);
            if key.eq_ignore_ascii_case("default") {
                let problems = config.change("keys", name, None)?;
                say(
                    reply,
                    Tone::Good,
                    format!(
                        "{name} is back to {}.",
                        config.keys.label(act).unwrap_or_else(|| "no key".into())
                    ),
                );
                return Ok(problems);
            }
            // Refuse here rather than write a line that would be ignored.
            let mut trial: Vec<(String, String)> = Vec::new();
            let (entries, _) = file::parse(
                &config
                    .path
                    .as_deref()
                    .and_then(|path| std::fs::read_to_string(path).ok())
                    .unwrap_or_default(),
            );
            for entry in entries
                .iter()
                .filter(|entry| entry.section == "keys" && entry.key != name)
            {
                if let Value::Text(text) = &entry.value {
                    trial.push((entry.key.clone(), text.clone()));
                }
            }
            trial.push((name.to_owned(), key.clone()));
            let (_, problems) = Keys::build(&trial);
            if let Some((_, message)) = problems.iter().find(|(index, _)| *index == trial.len() - 1)
                && !message.contains("now has none")
            {
                return Err(format!("{name} was not changed: {message}."));
            }
            let bind = keys::Bind::parse(key)
                .map_err(|message| format!("{name} was not changed: {message}."))?;
            let problems = config.change("keys", name, Some(&Value::Text(bind.to_string())))?;
            say(reply, Tone::Good, format!("{name} is now {bind}."));
            Ok(problems)
        }
        _ => Err("Usage: /keys [ACTION KEY | ACTION default]".into()),
    }
}

fn alias_command(
    config: &mut Config,
    args: &[String],
    reply: &mut Reply,
) -> Result<Vec<Problem>, String> {
    match args {
        [] => {
            if config.aliases.is_empty() {
                say(
                    reply,
                    Tone::Normal,
                    "No aliases yet. /alias NAME COMMAND... makes one, for example /alias dl add --to D:\\Media",
                );
            } else {
                for (name, lines) in config.aliases.iter() {
                    say(
                        reply,
                        Tone::Normal,
                        format!("  /{name:<10} {}", Aliases::describe(lines)),
                    );
                }
                say(
                    reply,
                    Tone::Dim,
                    "/alias NAME COMMAND... changes one; /alias remove NAME deletes it. Several commands in one: a list in cli.toml.",
                );
            }
            Ok(Vec::new())
        }
        [remove, name] if remove.eq_ignore_ascii_case("remove") => {
            let name = name.trim_start_matches('/').to_ascii_lowercase();
            if config.aliases.get(&name).is_none() {
                return Err(format!("There is no /{name} alias."));
            }
            let problems = config.change("aliases", &name, None)?;
            say(reply, Tone::Good, format!("/{name} removed."));
            Ok(problems)
        }
        [name] => match config.aliases.get(name) {
            Some(lines) => {
                say(
                    reply,
                    Tone::Normal,
                    format!(
                        "/{} runs {}",
                        name.trim_start_matches('/'),
                        Aliases::describe(lines)
                    ),
                );
                Ok(Vec::new())
            }
            None => Err(format!(
                "There is no /{} alias. /alias NAME COMMAND... makes one.",
                name.trim_start_matches('/')
            )),
        },
        [name, command @ ..] => {
            if name.eq_ignore_ascii_case("remove") {
                return Err(
                    "remove is how aliases are deleted, so it cannot be one: /alias remove NAME"
                        .into(),
                );
            }
            let line = join_words(command);
            let (name, lines) = alias::check(name, &[line], &is_command)?;
            // Try it with the others before saving, so a refused alias never
            // reaches the file.
            let mut given: Vec<(String, Vec<String>)> = config
                .aliases
                .iter()
                .filter(|(held, _)| *held != name)
                .map(|(held, lines)| (held.to_owned(), lines.to_vec()))
                .collect();
            given.push((name.clone(), lines.clone()));
            let (_, problems) = Aliases::build(&given, &is_command);
            if let Some((_, message)) = problems.iter().find(|(index, _)| *index == given.len() - 1)
            {
                return Err(format!("{message}."));
            }
            let problems = config.change("aliases", &name, Some(&Value::Text(lines[0].clone())))?;
            say(
                reply,
                Tone::Good,
                format!("/{name} runs {}", Aliases::describe(&lines)),
            );
            Ok(problems)
        }
    }
}

/// Words back into a line, quoting those with spaces as `line::words` reads
/// them.
fn join_words(words: &[String]) -> String {
    words
        .iter()
        .map(|word| {
            if word.is_empty() || word.contains(char::is_whitespace) {
                format!("\"{word}\"")
            } else {
                word.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn glyph_name(set: GlyphSet) -> &'static str {
    match set {
        GlyphSet::Auto => "auto",
        GlyphSet::Unicode => "unicode",
        GlyphSet::Ascii => "ascii",
    }
}

fn density_name(density: Density) -> &'static str {
    match density {
        Density::Comfortable => "comfortable",
        Density::Compact => "compact",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Color;

    #[test]
    fn a_file_sets_everything_and_bad_entries_are_reported_by_line() {
        let (config, problems) = Config::from_text(
            "theme = \"light\"\nglyphs = \"ascii\"\ndensity = \"compact\"\ncolour = \"x\"\n\
             [colors]\naccent = \"#112233\"\nsky = \"blue\"\n\
             [keys]\npause = \"z\"\ndashboard = \"F5\"\ncancel = \"z\"\n\
             [aliases]\ndl = \"add --to D:\\\\Media\"\nsh = \"cmd /c del x\"\n\
             [extra]\nx = \"y\"\n",
        );
        assert_eq!(config.theme.name, "light");
        assert_eq!(
            config.theme.color("accent"),
            Some(Color::Rgb(0x11, 0x22, 0x33))
        );
        assert_eq!(config.glyphs, GlyphSet::Ascii);
        assert_eq!(config.density, Density::Compact);
        assert_eq!(config.keys.label(keys::Act::Pause).as_deref(), Some("z"));
        assert_eq!(
            config.keys.label(keys::Act::Dashboard).as_deref(),
            Some("F5")
        );
        assert_eq!(config.keys.label(keys::Act::Cancel).as_deref(), Some("c"));
        assert_eq!(config.aliases.names().collect::<Vec<_>>(), vec!["dl"]);
        let lines: Vec<usize> = problems.iter().map(|problem| problem.line).collect();
        assert_eq!(lines, vec![4, 7, 11, 14, 16]);
        assert_eq!(config.max_rows(60), 4);
    }

    #[test]
    fn commands_edit_the_file_and_apply_at_once() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("sub").join(FILE_NAME);
        let (mut config, problems) = Config::load(Some(path.clone()));
        assert!(problems.is_empty());
        let words = |text: &str| super::super::line::words(text);

        let reply = command(&mut config, "theme", &words("high-contrast")).unwrap();
        assert_eq!(reply.lines[0].tone, Tone::Good, "{:?}", reply.lines);
        assert_eq!(config.theme.name, "high-contrast");

        let reply = command(&mut config, "keys", &words("pause F6")).unwrap();
        assert_eq!(reply.lines[0].tone, Tone::Good, "{:?}", reply.lines);
        let refused = command(&mut config, "keys", &words("cancel F6")).unwrap();
        assert_eq!(refused.lines[0].tone, Tone::Bad);
        let refused = command(&mut config, "keys", &words("dashboard x")).unwrap();
        assert_eq!(refused.lines[0].tone, Tone::Bad);

        let reply = command(&mut config, "alias", &words("dl add --to \"D:\\My Media\"")).unwrap();
        assert_eq!(reply.lines[0].tone, Tone::Good, "{:?}", reply.lines);
        let refused = command(&mut config, "alias", &words("x powershell -c whoami")).unwrap();
        assert_eq!(refused.lines[0].tone, Tone::Bad);
        assert!(config.aliases.get("x").is_none());

        let saved = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            saved,
            "theme = 'high-contrast'\n\n[keys]\npause = 'F6'\n\n[aliases]\ndl = 'add --to \"D:\\My Media\"'\n"
        );
        // What was saved reads back to the same configuration.
        let (again, problems) = Config::load(Some(path.clone()));
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(
            again.aliases.expand("/dl https://example.test/a").unwrap(),
            vec!["/add --to \"D:\\My Media\" https://example.test/a"]
        );
        assert_eq!(again.keys.label(keys::Act::Pause).as_deref(), Some("F6"));

        // Problems already in the file are not repeated by each change.
        let mut text = std::fs::read_to_string(&path).unwrap();
        text.push_str(
            "broken
",
        );
        std::fs::write(&path, text).unwrap();
        let reply = command(&mut config, "theme", &words("plain")).unwrap();
        assert_eq!(reply.lines.len(), 1, "{:?}", reply.lines);
        let reply = command(&mut config, "keys", &words("pause r")).unwrap();
        assert!(
            reply.lines[1].text.contains("resume now has none"),
            "{:?}",
            reply.lines
        );
        command(&mut config, "keys", &words("pause F6")).unwrap();

        command(&mut config, "alias", &words("remove dl")).unwrap();
        command(&mut config, "keys", &words("pause default")).unwrap();
        assert!(config.aliases.is_empty());
        assert_eq!(config.keys.label(keys::Act::Pause).as_deref(), Some("p"));
    }

    #[test]
    fn an_unreadable_file_is_the_defaults_with_a_note() {
        let folder = tempfile::tempdir().unwrap();
        // A folder where the file should be cannot be read as text.
        let path = folder.path().join(FILE_NAME);
        std::fs::create_dir(&path).unwrap();
        let (config, problems) = Config::load(Some(path));
        assert_eq!(config.theme.name, "default");
        assert_eq!(problems.len(), 1);
        assert_eq!(problems[0].line, 0);
    }
}
