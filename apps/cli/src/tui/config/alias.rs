//! Aliases and custom commands. `dl = "add --to D:\Media"` makes `/dl LINK`
//! run `/add --to D:\Media LINK`; a list, `tidy = ["rm 3", "history"]`, is a
//! custom command that runs each line in turn. Every line must start with a
//! Fetchpath `/` command or another alias, so nothing here can reach a shell
//! or start a program: expansion only produces lines for the prompt's own
//! commands.

use std::collections::BTreeMap;

/// Most aliases one line may pass through, which also stops loops.
pub const MAX_DEPTH: usize = 8;
/// Most commands one line may expand to.
pub const MAX_LINES: usize = 32;
/// Commands an alias may not run: `/tools` chooses and installs the video
/// helper programs, so a line in a file must never reach it.
const REFUSED: [&str; 1] = ["tools"];

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Aliases {
    /// By lower-case name: the lines it stands for, without their `/`.
    map: BTreeMap<String, Vec<String>>,
}

/// Why a name or its lines cannot be an alias.
pub fn check(
    name: &str,
    lines: &[String],
    is_command: &dyn Fn(&str) -> bool,
) -> Result<(String, Vec<String>), String> {
    let name = name.trim().trim_start_matches('/').to_ascii_lowercase();
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(format!(
            "{name:?} cannot be an alias name; use letters, digits, - and _"
        ));
    }
    if is_command(&name) {
        return Err(format!("/{name} is already a Fetchpath command"));
    }
    if lines.is_empty() {
        return Err(format!("/{name} needs at least one command"));
    }
    let mut kept = Vec::new();
    for line in lines {
        let line = line.trim();
        if line.chars().any(char::is_control) {
            return Err(format!(
                "/{name}: a command cannot hold line breaks or control characters"
            ));
        }
        let line = line.strip_prefix('/').unwrap_or(line).trim_start();
        if line.is_empty() {
            return Err(format!("/{name}: a command cannot be empty"));
        }
        if REFUSED.contains(&head(line).as_str()) {
            return Err(format!(
                "/{name}: /{} sets up outside programs, so only you can type it",
                head(line)
            ));
        }
        kept.push(line.to_owned());
    }
    Ok((name, kept))
}

/// The command word of a line, lower-cased.
fn head(line: &str) -> String {
    line.split_whitespace()
        .next()
        .unwrap_or("")
        .to_ascii_lowercase()
}

impl Aliases {
    /// Aliases from `(name, lines)` pairs in the file's order. Lines whose
    /// first word is neither a command nor another alias, and aliases that
    /// loop or grow past the limits, are refused with the pair's index.
    pub fn build(
        given: &[(String, Vec<String>)],
        is_command: &dyn Fn(&str) -> bool,
    ) -> (Self, Vec<(usize, String)>) {
        let mut problems = Vec::new();
        let mut checked: Vec<(usize, String, Vec<String>)> = Vec::new();
        for (index, (name, lines)) in given.iter().enumerate() {
            match check(name, lines, is_command) {
                Ok((name, lines)) => checked.push((index, name, lines)),
                Err(message) => problems.push((index, message)),
            }
        }
        // Drop lines that name neither a command nor a surviving alias until
        // nothing changes, since dropping one alias can strand another.
        loop {
            let names: Vec<String> = checked.iter().map(|(_, name, _)| name.clone()).collect();
            let bad = checked.iter().position(|(_, _, lines)| {
                lines.iter().any(|line| {
                    let word = head(line);
                    !is_command(&word) && !names.contains(&word)
                })
            });
            let Some(position) = bad else { break };
            let (index, name, lines) = checked.remove(position);
            let word = lines
                .iter()
                .map(|line| head(line))
                .find(|word| !is_command(word) && !names.contains(word))
                .unwrap_or_default();
            problems.push((
                index,
                format!(
                    "/{name} runs {word:?}, which is not a Fetchpath command; aliases can only run Fetchpath commands, never programs"
                ),
            ));
        }
        let mut aliases = Self {
            map: checked
                .iter()
                .map(|(_, name, lines)| (name.clone(), lines.clone()))
                .collect(),
        };
        // Loops and runaway growth, found by expanding each alias once.
        let looping: Vec<(usize, String, String)> = checked
            .iter()
            .filter_map(|(index, name, _)| {
                aliases
                    .expand(&format!("/{name}"))
                    .err()
                    .map(|message| (*index, name.clone(), message))
            })
            .collect();
        for (index, name, message) in looping {
            aliases.map.remove(&name);
            problems.push((index, message));
        }
        problems.sort_by_key(|(index, _)| *index);
        (aliases, problems)
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    pub fn get(&self, name: &str) -> Option<&[String]> {
        self.map
            .get(&name.trim_start_matches('/').to_ascii_lowercase())
            .map(Vec::as_slice)
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.map.keys().map(String::as_str)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &[String])> {
        self.map
            .iter()
            .map(|(name, lines)| (name.as_str(), lines.as_slice()))
    }

    /// What an alias shows in lists: its line, or its lines joined by `;`.
    pub fn describe(lines: &[String]) -> String {
        lines
            .iter()
            .map(|line| format!("/{line}"))
            .collect::<Vec<_>>()
            .join("; ")
    }

    /// The lines a typed line runs: itself when it is not an alias, or what
    /// the alias stands for with the typed words after a single command. A
    /// custom command (several lines) takes no words.
    pub fn expand(&self, line: &str) -> Result<Vec<String>, String> {
        let mut out = Vec::new();
        self.expand_into(line, 0, &mut out)?;
        Ok(out)
    }

    fn expand_into(&self, line: &str, depth: usize, out: &mut Vec<String>) -> Result<(), String> {
        let trimmed = line.trim();
        let Some(rest) = trimmed.strip_prefix('/') else {
            out.push(line.to_owned());
            return Ok(());
        };
        let rest = rest.trim_start();
        let (name, words) = match rest.find(char::is_whitespace) {
            Some(at) => (&rest[..at], rest[at..].trim()),
            None => (rest, ""),
        };
        let Some(lines) = self.map.get(&name.to_ascii_lowercase()) else {
            out.push(line.to_owned());
            return Ok(());
        };
        if depth >= MAX_DEPTH {
            return Err(format!(
                "/{name} goes through more than {MAX_DEPTH} aliases; does it name itself?"
            ));
        }
        if lines.len() > 1 && !words.is_empty() {
            return Err(format!(
                "/{name} runs several commands, so it takes nothing after it"
            ));
        }
        for expanded in lines {
            let next = if words.is_empty() {
                format!("/{expanded}")
            } else {
                format!("/{expanded} {words}")
            };
            self.expand_into(&next, depth + 1, out)?;
            if out.len() > MAX_LINES {
                return Err(format!("/{name} runs more than {MAX_LINES} commands"));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn is_command(name: &str) -> bool {
        [
            "add", "history", "rm", "pause", "engine", "quit", "q", "tools",
        ]
        .contains(&name)
    }

    fn given(pairs: &[(&str, &[&str])]) -> Vec<(String, Vec<String>)> {
        pairs
            .iter()
            .map(|(name, lines)| {
                (
                    (*name).to_owned(),
                    lines.iter().map(|line| (*line).to_owned()).collect(),
                )
            })
            .collect()
    }

    #[test]
    fn an_alias_puts_its_command_before_the_typed_words() {
        let (aliases, problems) = Aliases::build(
            &given(&[
                ("dl", &["add --to \"D:\\My Files\""]),
                ("get", &["/dl --quality 720p"]),
                ("tidy", &["rm 3", "history"]),
            ]),
            &is_command,
        );
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(
            aliases.expand("/GET https://example.test/v").unwrap(),
            vec!["/add --to \"D:\\My Files\" --quality 720p https://example.test/v"]
        );
        assert_eq!(aliases.expand("/tidy").unwrap(), vec!["/rm 3", "/history"]);
        assert!(aliases.expand("/tidy 4").is_err());
        // Lines that are not aliases pass through untouched.
        assert_eq!(aliases.expand("/pause 2").unwrap(), vec!["/pause 2"]);
        assert_eq!(
            aliases.expand("https://example.test/f").unwrap(),
            vec!["https://example.test/f"]
        );
    }

    #[test]
    fn shells_and_programs_are_refused() {
        let (aliases, problems) = Aliases::build(
            &given(&[
                ("sh", &["!del C:\\x"]),
                ("calc", &["calc.exe"]),
                ("ps", &["powershell -c rm x"]),
                ("mixed", &["history", "cmd /c dir"]),
                ("broken", &["history\nrm 1"]),
                ("add", &["history"]),
                ("bad name", &["history"]),
                ("chain", &["calc"]),
                ("helpers", &["tools use D:\\helpers"]),
                ("ok", &["engine"]),
            ]),
            &is_command,
        );
        let refused: Vec<usize> = problems.iter().map(|(index, _)| *index).collect();
        assert_eq!(refused, vec![0, 1, 2, 3, 4, 5, 6, 7, 8]);
        assert!(problems[1].1.contains("never programs"), "{problems:?}");
        assert_eq!(aliases.names().collect::<Vec<_>>(), vec!["ok"]);
    }

    #[test]
    fn loops_and_runaway_growth_stop_at_the_limits() {
        let (aliases, problems) = Aliases::build(
            &given(&[
                ("a", &["b"]),
                ("b", &["a"]),
                ("me", &["me"]),
                ("x2", &["x1", "x1"]),
                ("x1", &["x0", "x0"]),
                ("x0", &["y", "y"]),
                ("y", &["z", "z"]),
                ("z", &["w", "w"]),
                ("w", &["v", "v"]),
                ("v", &["engine", "engine"]),
                ("deep", &["d1"]),
                ("d1", &["d2"]),
                ("d2", &["d3"]),
                ("d3", &["d4"]),
                ("d4", &["d5"]),
                ("d5", &["d6"]),
                ("d6", &["d7"]),
                ("d7", &["d8"]),
                ("d8", &["engine"]),
            ]),
            &is_command,
        );
        let names: Vec<&str> = problems
            .iter()
            .map(|(index, _)| {
                ["a", "b", "me", "x2", "x1", "x0", "y", "z", "w", "v", "deep"][*index]
            })
            .collect();
        // 2^7 = 128 lines for x2, 64 for x1, 32 for x0 (at the limit).
        assert_eq!(names, vec!["a", "b", "me", "x2", "x1", "deep"]);
        assert!(problems[0].1.contains("name itself"), "{problems:?}");
        assert_eq!(aliases.expand("/x0").unwrap().len(), 32);
        assert_eq!(aliases.expand("/d1").unwrap(), vec!["/engine"]);
    }
}
