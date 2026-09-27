//! The small part of TOML that `cli.toml` uses: `[section]` headers,
//! `key = "text"` or `key = 'text'`, `key = ["a", "b"]` on one line, and
//! `#` comments. Each line stands alone, so a bad line is reported with its
//! number and skipped while the rest still apply; a full TOML parser would
//! reject the whole file. Edits rewrite only the line they change, so the
//! person's comments and order survive `/theme`, `/keys` and `/alias`.

use std::fmt::Write as _;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Value {
    Text(String),
    List(Vec<String>),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Entry {
    /// Empty for keys above the first section.
    pub section: String,
    pub key: String,
    pub value: Value,
    /// One-based line number.
    pub line: usize,
}

/// Something in the file that was not used, and why.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Problem {
    /// One-based; 0 for the file as a whole.
    pub line: usize,
    pub message: String,
}

impl Problem {
    pub fn new(line: usize, message: impl Into<String>) -> Self {
        Self {
            line,
            message: message.into(),
        }
    }
}

/// Every entry that reads, and a problem for every line that does not.
/// A key given twice in a section keeps its first value.
pub fn parse(text: &str) -> (Vec<Entry>, Vec<Problem>) {
    let mut entries: Vec<Entry> = Vec::new();
    let mut problems = Vec::new();
    let mut section = String::new();
    for (index, raw) in text.lines().enumerate() {
        let number = index + 1;
        match read_line(raw) {
            Ok(Line::Blank) => {}
            Ok(Line::Section(name)) => section = name,
            Ok(Line::Entry(key, value)) => {
                if entries
                    .iter()
                    .any(|entry| entry.section == section && entry.key == key)
                {
                    problems.push(Problem::new(
                        number,
                        format!("{key} is given again; the first one is used"),
                    ));
                    continue;
                }
                entries.push(Entry {
                    section: section.clone(),
                    key,
                    value,
                    line: number,
                });
            }
            Err(message) => {
                // Entries under a section that did not read are skipped too,
                // rather than landing in the section before it.
                if raw.trim_start().starts_with('[') {
                    section = "\u{0}".to_owned();
                }
                problems.push(Problem::new(number, message));
            }
        }
    }
    entries.retain(|entry| entry.section != "\u{0}");
    (entries, problems)
}

enum Line {
    Blank,
    Section(String),
    Entry(String, Value),
}

fn read_line(raw: &str) -> Result<Line, String> {
    let line = raw.trim();
    if line.is_empty() || line.starts_with('#') {
        return Ok(Line::Blank);
    }
    if let Some(rest) = line.strip_prefix('[') {
        let Some((name, after)) = rest.split_once(']') else {
            return Err("a section needs a closing ]".into());
        };
        let name = name.trim();
        if !is_bare_key(name) {
            return Err(format!("[{name}] is not a section name this file uses"));
        }
        end_of_line(after)?;
        return Ok(Line::Section(name.to_owned()));
    }
    let Some((key, value)) = line.split_once('=') else {
        return Err("expected key = value".into());
    };
    let key = key.trim();
    if !is_bare_key(key) {
        return Err(format!(
            "{key:?} is not a plain name (letters, digits, - and _)"
        ));
    }
    let mut rest = value.trim_start();
    let value = if let Some(inside) = rest.strip_prefix('[') {
        rest = inside;
        let mut items = Vec::new();
        loop {
            rest = rest.trim_start();
            if let Some(after) = rest.strip_prefix(']') {
                rest = after;
                break;
            }
            let (item, after) = string(rest)?;
            items.push(item);
            rest = after.trim_start();
            if let Some(after) = rest.strip_prefix(',') {
                rest = after;
            } else if !rest.starts_with(']') {
                return Err(
                    "a list needs commas between its texts and a closing ] on the same line".into(),
                );
            }
        }
        Value::List(items)
    } else {
        let (text, after) = string(rest)?;
        rest = after;
        Value::Text(text)
    };
    end_of_line(rest)?;
    Ok(Line::Entry(key.to_owned(), value))
}

pub fn is_bare_key(key: &str) -> bool {
    !key.is_empty()
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn end_of_line(rest: &str) -> Result<(), String> {
    let rest = rest.trim_start();
    if rest.is_empty() || rest.starts_with('#') {
        Ok(())
    } else {
        Err(format!("unexpected {rest:?} at the end of the line"))
    }
}

/// A quoted text at the start of `input` and what follows it.
fn string(input: &str) -> Result<(String, &str), String> {
    if let Some(rest) = input.strip_prefix('\'') {
        let Some(end) = rest.find('\'') else {
            return Err("a text in ' quotes needs its closing '".into());
        };
        return Ok((rest[..end].to_owned(), &rest[end + 1..]));
    }
    let Some(rest) = input.strip_prefix('"') else {
        return Err("values are texts in quotes, or lists of them in [ ]".into());
    };
    let mut text = String::new();
    let mut chars = rest.char_indices();
    while let Some((at, c)) = chars.next() {
        match c {
            '"' => return Ok((text, &rest[at + 1..])),
            '\\' => {
                let escaped = match chars.next().map(|(_, c)| c) {
                    Some('\\') => '\\',
                    Some('"') => '"',
                    Some('n') => '\n',
                    Some('t') => '\t',
                    Some(kind @ ('u' | 'U')) => {
                        let digits = if kind == 'u' { 4 } else { 8 };
                        let hex: String = (0..digits)
                            .filter_map(|_| chars.next().map(|(_, c)| c))
                            .collect();
                        u32::from_str_radix(&hex, 16)
                            .ok()
                            .filter(|_| hex.len() == digits)
                            .and_then(char::from_u32)
                            .ok_or_else(|| format!("\\{kind}{hex} is not a character"))?
                    }
                    other => {
                        return Err(format!(
                            "\\{} is not an escape here; write \\\\ for a backslash, or use ' quotes",
                            other.map(String::from).unwrap_or_default()
                        ));
                    }
                };
                text.push(escaped);
            }
            c => text.push(c),
        }
    }
    Err("a text in \" quotes needs its closing \"".into())
}

/// The value as a line of the file: literal quotes when they can hold it,
/// so Windows paths keep single backslashes.
pub fn quote(text: &str) -> String {
    if !text.contains('\'') && !text.chars().any(char::is_control) {
        return format!("'{text}'");
    }
    let mut quoted = String::from("\"");
    for c in text.chars() {
        match c {
            '"' => quoted.push_str("\\\""),
            '\\' => quoted.push_str("\\\\"),
            '\n' => quoted.push_str("\\n"),
            '\t' => quoted.push_str("\\t"),
            c if c.is_control() => {
                let _ = write!(quoted, "\\u{:04X}", c as u32);
            }
            c => quoted.push(c),
        }
    }
    quoted.push('"');
    quoted
}

fn render(key: &str, value: &Value) -> String {
    match value {
        Value::Text(text) => format!("{key} = {}", quote(text)),
        Value::List(items) => format!(
            "{key} = [{}]",
            items
                .iter()
                .map(|item| quote(item))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

/// `text` with `key` in `section` set to `value`, or removed when `value`
/// is `None`. The first line holding the key is rewritten in place; a new
/// key goes after the last entry of its section, and a missing section is
/// added at the end. Other lines are kept as they were.
pub fn edit(text: &str, section: &str, key: &str, value: Option<&Value>) -> String {
    let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
    let mut current = String::new();
    let mut found = None;
    // Where a new key of this section goes: after its last entry, or right
    // after its header; for top-level keys, before the first header.
    let mut insert_at = if section.is_empty() { Some(0) } else { None };
    let mut first_header = None;
    for (index, raw) in lines.iter().enumerate() {
        match read_line(raw) {
            Ok(Line::Section(name)) => {
                first_header.get_or_insert(index);
                current = name;
                if current == section {
                    insert_at = Some(index + 1);
                }
            }
            Ok(Line::Entry(name, _)) if current == section => {
                if name == key && found.is_none() {
                    found = Some(index);
                }
                insert_at = Some(index + 1);
            }
            Err(_) if raw.trim_start().starts_with('[') => current = "\u{0}".to_owned(),
            _ => {}
        }
    }
    if section.is_empty() && insert_at == Some(0) {
        // No top-level entry yet: just before the first section, below any
        // leading comments.
        insert_at = Some(first_header.unwrap_or(lines.len()));
    }
    match (found, value) {
        (Some(index), Some(value)) => lines[index] = render(key, value),
        (Some(index), None) => {
            lines.remove(index);
        }
        (None, None) => {}
        (None, Some(value)) => match insert_at {
            Some(index) => lines.insert(index, render(key, value)),
            None => {
                if lines.last().is_some_and(|line| !line.trim().is_empty()) {
                    lines.push(String::new());
                }
                lines.push(format!("[{section}]"));
                lines.push(render(key, value));
            }
        },
    }
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(value: &str) -> Value {
        Value::Text(value.to_owned())
    }

    #[test]
    fn reads_sections_texts_lists_and_comments() {
        let (entries, problems) = parse(
            "# mine\ntheme = \"high-contrast\"  # dark room\n\n[keys]\npause = 'P'\n[aliases]\ndl = 'add --to \"D:\\My Files\"'\ntidy = [\"rm 1\", 'history', ]\n",
        );
        assert!(problems.is_empty(), "{problems:?}");
        let found: Vec<_> = entries
            .iter()
            .map(|entry| {
                (
                    entry.section.as_str(),
                    entry.key.as_str(),
                    &entry.value,
                    entry.line,
                )
            })
            .collect();
        assert_eq!(
            found,
            vec![
                ("", "theme", &text("high-contrast"), 2),
                ("keys", "pause", &text("P"), 5),
                ("aliases", "dl", &text("add --to \"D:\\My Files\""), 7),
                (
                    "aliases",
                    "tidy",
                    &Value::List(vec!["rm 1".into(), "history".into()]),
                    8
                ),
            ]
        );
    }

    #[test]
    fn bad_lines_are_reported_by_number_and_the_rest_still_read() {
        let (entries, problems) = parse(
            "theme = plain\nglyphs = \"ascii\"\n[keys\npause = \"p\"\n[aliases]\nx = \"a\\q\"\ny = \"ok\" z\nok = [\"a\" \"b\"]\ngood = \"history\"\ngood = \"engine\"\n",
        );
        let lines: Vec<usize> = problems.iter().map(|problem| problem.line).collect();
        assert_eq!(lines, vec![1, 3, 6, 7, 8, 10]);
        let kept: Vec<_> = entries.iter().map(|entry| entry.key.as_str()).collect();
        // `pause` sits under the broken header, so it is not put anywhere.
        assert_eq!(kept, vec!["glyphs", "good"]);
        assert_eq!(entries[1].value, text("history"));
    }

    #[test]
    fn escapes_read_and_quote_round_trips() {
        let (entries, _) = parse("a = \"tab\\there \\\"q\\\" \\u00e9 \\\\\"\n");
        assert_eq!(entries[0].value, text("tab\there \"q\" é \\"));
        for sample in [
            "plain",
            "D:\\Downloads",
            "it's",
            "line\nbreak \"q\" \\",
            "\u{7}",
        ] {
            let (entries, problems) = parse(&format!("k = {}\n", quote(sample)));
            assert!(problems.is_empty(), "{sample:?}: {problems:?}");
            assert_eq!(entries[0].value, text(sample));
        }
    }

    #[test]
    fn edits_keep_comments_and_place_new_keys() {
        let original =
            "# my terminal\n\n[keys]\n# pausing\npause = \"p\"\n\n[aliases]\ndl = \"add\"\n";
        let changed = edit(original, "keys", "pause", Some(&text("P")));
        assert_eq!(changed, original.replace("pause = \"p\"", "pause = 'P'"));
        let added = edit(original, "keys", "cancel", Some(&text("k")));
        assert!(
            added.contains("pause = \"p\"\ncancel = 'k'\n\n[aliases]"),
            "{added}"
        );
        let top = edit(original, "", "theme", Some(&text("light")));
        assert!(
            top.starts_with("# my terminal\n\ntheme = 'light'\n[keys]"),
            "{top}"
        );
        let removed = edit(original, "aliases", "dl", None);
        assert!(!removed.contains("dl ="));
        assert!(removed.contains("[aliases]"));
        let fresh = edit(
            "",
            "aliases",
            "x",
            Some(&Value::List(vec!["engine".into()])),
        );
        assert_eq!(fresh, "[aliases]\nx = ['engine']\n");
        let fresh_top = edit("", "", "theme", Some(&text("plain")));
        assert_eq!(fresh_top, "theme = 'plain'\n");
    }
}
