//! Keybindings: which key opens the dashboard, and the dashboard's single
//! keys. Arrows, Page Up/Down, Home/End, Delete, Enter, Esc and Ctrl+C
//! always keep their meaning, so a binding can add a key but never take one
//! of those away.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::fmt;

/// A key a binding names: one character, or F1 to F12.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Bind {
    Char(char),
    F(u8),
}

impl Bind {
    pub fn parse(text: &str) -> Result<Self, String> {
        let trimmed = text.trim();
        let mut chars = trimmed.chars();
        if let (Some(c), None) = (chars.next(), chars.next()) {
            if c.is_whitespace() || c.is_control() {
                return Err("a key cannot be a space or a control character".into());
            }
            return Ok(Self::Char(c.to_lowercase().next().unwrap_or(c)));
        }
        if let Some(number) = trimmed
            .strip_prefix(['F', 'f'])
            .and_then(|digits| digits.parse::<u8>().ok())
            .filter(|number| (1..=12).contains(number))
        {
            return Ok(Self::F(number));
        }
        Err(format!(
            "{trimmed:?} is not a key; use one character or F1 to F12"
        ))
    }

    /// Whether a pressed key is this one. Letters match either case; a key
    /// held with Ctrl or Alt never matches, so those stay the terminal's.
    pub fn matches(self, key: &KeyEvent) -> bool {
        if key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            return false;
        }
        match (self, key.code) {
            (Self::Char(bound), KeyCode::Char(typed)) => typed.to_lowercase().next() == Some(bound),
            (Self::F(bound), KeyCode::F(pressed)) => bound == pressed,
            _ => false,
        }
    }
}

impl fmt::Display for Bind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Char(c) => write!(formatter, "{c}"),
            Self::F(number) => write!(formatter, "F{number}"),
        }
    }
}

/// What a key can be bound to.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Act {
    /// Opens the dashboard from the prompt, and closes it.
    Dashboard,
    Pause,
    /// Resumes a paused download or retries a failed one.
    Resume,
    Cancel,
    Remove,
    Approve,
    Deny,
    Folder,
    Up,
    Down,
    Close,
}

/// Every action with its name in the file, its default key and what it
/// does.
pub const ACTS: [(Act, &str, Bind, &str); 11] = [
    (
        Act::Dashboard,
        "dashboard",
        Bind::F(2),
        "open or close the dashboard (F1-F12 only)",
    ),
    (
        Act::Pause,
        "pause",
        Bind::Char('p'),
        "pause the chosen download",
    ),
    (Act::Resume, "resume", Bind::Char('r'), "resume or retry it"),
    (
        Act::Cancel,
        "cancel",
        Bind::Char('c'),
        "cancel it (pressed twice)",
    ),
    (
        Act::Remove,
        "remove",
        Bind::Char('x'),
        "remove it from the list",
    ),
    (
        Act::Approve,
        "approve",
        Bind::Char('a'),
        "let an agent's request download",
    ),
    (
        Act::Deny,
        "deny",
        Bind::Char('d'),
        "refuse an agent's request",
    ),
    (
        Act::Folder,
        "folder",
        Bind::Char('o'),
        "show its file in File Explorer",
    ),
    (Act::Up, "up", Bind::Char('k'), "choose the one above"),
    (Act::Down, "down", Bind::Char('j'), "choose the one below"),
    (Act::Close, "close", Bind::Char('q'), "leave the dashboard"),
];

pub fn act_named(name: &str) -> Option<Act> {
    ACTS.iter()
        .find(|(_, known, _, _)| known.eq_ignore_ascii_case(name))
        .map(|(act, ..)| *act)
}

pub fn act_name(act: Act) -> &'static str {
    ACTS.iter()
        .find(|(known, ..)| *known == act)
        .map_or("", |(_, name, ..)| name)
}

/// The key for each action. An action can end up with no key when another
/// action took its default; `Keys::build` says so.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Keys {
    binds: Vec<(Act, Option<Bind>)>,
}

impl Default for Keys {
    fn default() -> Self {
        Self {
            binds: ACTS
                .iter()
                .map(|(act, _, bind, _)| (*act, Some(*bind)))
                .collect(),
        }
    }
}

impl Keys {
    /// Keys from the person's choices, in the file's order. A choice that
    /// names an unknown action, an unusable key, or a key an earlier choice
    /// took is refused with a reason; an action whose default key was taken
    /// is left without one and reported. Returns the keys and, per refused
    /// or displacing choice, its index in `chosen` with the reason.
    pub fn build(chosen: &[(String, String)]) -> (Self, Vec<(usize, String)>) {
        let mut problems = Vec::new();
        let mut set: Vec<(Act, Bind, usize)> = Vec::new();
        for (index, (name, key)) in chosen.iter().enumerate() {
            let Some(act) = act_named(name) else {
                let known: Vec<&str> = ACTS.iter().map(|(_, name, ..)| *name).collect();
                problems.push((
                    index,
                    format!("{name} is not an action; they are {}", known.join(", ")),
                ));
                continue;
            };
            let bind = match Bind::parse(key) {
                Ok(bind) => bind,
                Err(message) => {
                    problems.push((index, message));
                    continue;
                }
            };
            if act == Act::Dashboard && !matches!(bind, Bind::F(_)) {
                problems.push((
                    index,
                    "the dashboard key must be F1 to F12, since letters type at the prompt".into(),
                ));
                continue;
            }
            if let Some((other, ..)) = set.iter().find(|(_, taken, _)| *taken == bind) {
                problems.push((index, format!("{bind} is already {}", act_name(*other))));
                continue;
            }
            set.retain(|(held, ..)| *held != act);
            set.push((act, bind, index));
        }
        let mut binds = Vec::new();
        for (act, name, default, _) in ACTS {
            if let Some((_, bind, _)) = set.iter().find(|(held, ..)| *held == act) {
                binds.push((act, Some(*bind)));
            } else if let Some((_, _, index)) = set.iter().find(|(_, bind, _)| *bind == default) {
                problems.push((
                    *index,
                    format!(
                        "{default} was {name}'s key, so {name} now has none (give it one in [keys])",
                    ),
                ));
                binds.push((act, None));
            } else {
                binds.push((act, Some(default)));
            }
        }
        (Self { binds }, problems)
    }

    pub fn get(&self, act: Act) -> Option<Bind> {
        self.binds
            .iter()
            .find(|(held, _)| *held == act)
            .and_then(|(_, bind)| *bind)
    }

    /// The action a pressed key stands for, if any.
    pub fn act(&self, key: &KeyEvent) -> Option<Act> {
        self.binds
            .iter()
            .find(|(_, bind)| bind.is_some_and(|bind| bind.matches(key)))
            .map(|(act, _)| *act)
    }

    /// Whether a pressed key is `act`'s.
    pub fn is(&self, act: Act, key: &KeyEvent) -> bool {
        self.get(act).is_some_and(|bind| bind.matches(key))
    }

    /// The key's name for hints, or `None` when the action has no key.
    pub fn label(&self, act: Act) -> Option<String> {
        self.get(act).map(|bind| bind.to_string())
    }

    /// One line per action for `/keys`.
    pub fn lines(&self) -> Vec<String> {
        ACTS.iter()
            .map(|(act, name, _, summary)| {
                let key = self.label(*act).unwrap_or_else(|| "(none)".into());
                format!("  {name:<10}{key:<5}{summary}")
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn chosen(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(name, key)| ((*name).to_owned(), (*key).to_owned()))
            .collect()
    }

    #[test]
    fn keys_parse_and_match_either_case() {
        assert_eq!(Bind::parse("P"), Ok(Bind::Char('p')));
        assert_eq!(Bind::parse("f5"), Ok(Bind::F(5)));
        assert!(Bind::parse("F13").is_err());
        assert!(Bind::parse(" ").is_err());
        assert!(Bind::parse("ctrl+p").is_err());
        let bind = Bind::Char('p');
        assert!(bind.matches(&press(KeyCode::Char('P'))));
        assert!(!bind.matches(&KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL)));
        assert!(Bind::F(3).matches(&press(KeyCode::F(3))));
    }

    #[test]
    fn swapping_two_keys_works_and_clashes_are_refused() {
        let (keys, problems) = Keys::build(&chosen(&[("pause", "r"), ("resume", "p")]));
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(keys.act(&press(KeyCode::Char('r'))), Some(Act::Pause));
        assert_eq!(keys.act(&press(KeyCode::Char('p'))), Some(Act::Resume));

        let (keys, problems) = Keys::build(&chosen(&[
            ("pause", "z"),
            ("cancel", "z"),
            ("fly", "f"),
            ("dashboard", "d"),
            ("folder", "F13"),
        ]));
        let refused: Vec<usize> = problems.iter().map(|(index, _)| *index).collect();
        assert_eq!(refused, vec![1, 2, 3, 4]);
        assert_eq!(keys.get(Act::Pause), Some(Bind::Char('z')));
        assert_eq!(keys.get(Act::Cancel), Some(Bind::Char('c')));
        assert_eq!(keys.get(Act::Dashboard), Some(Bind::F(2)));
    }

    #[test]
    fn taking_a_default_leaves_that_action_without_a_key_and_says_so() {
        let (keys, problems) = Keys::build(&chosen(&[("pause", "r")]));
        assert_eq!(keys.get(Act::Resume), None);
        assert_eq!(problems.len(), 1);
        assert!(
            problems[0].1.contains("resume now has none"),
            "{problems:?}"
        );
    }
}
