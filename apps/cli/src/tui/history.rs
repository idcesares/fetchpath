//! What the terminal remembers between sessions (FP-063): the lines typed at
//! the prompt, in `cli-history`, and when it was last open, in `cli-seen`,
//! both beside `cli.toml`. Links lose their user info, query and fragment
//! before a line is written, because signed links are secrets.

use super::line::{Out, Tone};
use super::view;
use fetchpath_protocol::JobSnapshot;
use fetchpath_protocol::model::JobState;
use std::io::Write;
use std::path::{Path, PathBuf};

pub const FILE_NAME: &str = "cli-history";
const SEEN_FILE_NAME: &str = "cli-seen";
/// Lines kept; the file is compacted to this when it has grown to twice it.
const KEEP: usize = 500;
/// Finished downloads listed one by one in the summary.
const LISTED: usize = 8;

#[derive(Default)]
pub struct History {
    path: Option<PathBuf>,
    lines: Vec<String>,
}

impl History {
    /// Reads `cli-history` in `dir`; a missing or unreadable file is empty.
    pub fn load(dir: Option<&Path>) -> Self {
        let path = dir.map(|dir| dir.join(FILE_NAME));
        let mut lines: Vec<String> = path
            .as_deref()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .map(|text| {
                text.lines()
                    .filter(|line| !line.trim().is_empty())
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        if lines.len() > KEEP {
            lines.drain(..lines.len() - KEEP);
            if let Some(path) = &path {
                let _ = write_all(path, &lines);
            }
        }
        Self { path, lines }
    }

    /// The remembered lines, oldest first, for Up and Down.
    pub fn lines(&self) -> &[String] {
        &self.lines
    }

    /// Remembers a typed line with its links stripped. Saving is best effort:
    /// a history that cannot be written never stops the prompt.
    pub fn record(&mut self, line: &str) {
        let line = strip_links(line.trim());
        if line.is_empty() || self.lines.last() == Some(&line) {
            return;
        }
        self.lines.push(line.clone());
        let Some(path) = &self.path else { return };
        if self.lines.len() >= 2 * KEEP {
            self.lines.drain(..self.lines.len() - KEEP);
            let _ = write_all(path, &self.lines);
        } else if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            let _ = writeln!(file, "{line}");
        }
    }

    /// Forgets every remembered line, on disk too.
    pub fn clear(&mut self) -> Result<(), String> {
        self.lines.clear();
        match &self.path {
            Some(path) => match std::fs::remove_file(path) {
                Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                    Err(format!("Could not delete {}: {error}", path.display()))
                }
                _ => Ok(()),
            },
            None => Ok(()),
        }
    }
}

fn write_all(path: &Path, lines: &[String]) -> std::io::Result<()> {
    let mut text = lines.join("\n");
    text.push('\n');
    std::fs::write(path, text)
}

/// The line with every link cut to scheme, host, port and path: no user
/// info, query or fragment. A word that looks like a link but does not
/// parse is cut at its first `?` or `#` and after its last `@`.
pub fn strip_links(line: &str) -> String {
    line.split(' ')
        .map(|word| {
            let quote = if word.starts_with('"') { "\"" } else { "" };
            let bare = word.trim_matches('"');
            if !bare.contains("://") {
                return word.to_owned();
            }
            let tail = if quote.is_empty() || !word[1..].ends_with('"') {
                ""
            } else {
                "\""
            };
            format!("{quote}{}{tail}", strip_link(bare))
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn strip_link(link: &str) -> String {
    if let Ok(mut url) = url::Url::parse(link)
        && url.has_host()
    {
        let _ = url.set_username("");
        let _ = url.set_password(None);
        url.set_query(None);
        url.set_fragment(None);
        return url.to_string();
    }
    let cut = link.find(['?', '#']).map_or(link, |at| &link[..at]);
    match (cut.find("://"), cut.rfind('@')) {
        (Some(scheme), Some(at)) if at > scheme => {
            format!("{}{}", &cut[..scheme + 3], &cut[at + 1..])
        }
        _ => cut.to_owned(),
    }
}

/// When the terminal was last open, then records now. `None` the first
/// time, or without a data folder.
pub fn take_last_seen(dir: Option<&Path>) -> Option<i64> {
    let path = dir?.join(SEEN_FILE_NAME);
    let seen = std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| text.trim().parse().ok());
    let _ = std::fs::write(
        &path,
        fetchpath_protocol::Timestamp::now().unix_ms().to_string(),
    );
    seen
}

/// Downloads that finished or failed after `since`, from engine history:
/// a count, then failures first, then the most recent finished ones.
pub fn away_summary(history: &[JobSnapshot], since_ms: i64, glyphs: &view::Glyphs) -> Vec<Out> {
    let mut ended: Vec<&JobSnapshot> = history
        .iter()
        .filter(|job| matches!(job.state, JobState::Completed | JobState::Failed))
        .filter(|job| job.finished_at.is_some_and(|at| at.unix_ms() > since_ms))
        .collect();
    if ended.is_empty() {
        return Vec::new();
    }
    // Failures first, then newest first.
    ended.sort_by_key(|job| {
        (
            job.state != JobState::Failed,
            std::cmp::Reverse(job.finished_at.map(|at| at.unix_ms())),
        )
    });
    let failed = ended
        .iter()
        .filter(|job| job.state == JobState::Failed)
        .count();
    let finished = ended.len() - failed;
    let mut parts = Vec::new();
    if finished > 0 {
        parts.push(format!("{finished} finished"));
    }
    if failed > 0 {
        parts.push(format!("{failed} failed"));
    }
    let mut lines = vec![Out::new(
        Tone::Normal,
        format!("While you were away: {}.", parts.join(", ")),
    )];
    lines.extend(
        ended
            .iter()
            .take(LISTED)
            .map(|job| view::receipt(job, glyphs)),
    );
    if ended.len() > LISTED {
        lines.push(Out::new(
            Tone::Dim,
            format!("  and {} more: /history", ended.len() - LISTED),
        ));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_signed_link_never_reaches_the_history_file() {
        let dir = tempfile::tempdir().unwrap();
        let mut history = History::load(Some(dir.path()));
        history.record(
            "/add https://me:secret@cdn.example/a.iso?X-Amz-Signature=abc#frag --to D:\\ISOs",
        );
        history.record("\"https://cdn.example/b.zip?token=t\"");
        history.record("ftp://user:pw@files.example/c.bin");
        let text = std::fs::read_to_string(dir.path().join(FILE_NAME)).unwrap();
        for secret in [
            "secret",
            "me:",
            "Signature",
            "token",
            "frag",
            "user:",
            "pw@",
        ] {
            assert!(!text.contains(secret), "{secret} reached the file: {text}");
        }
        assert!(text.contains("/add https://cdn.example/a.iso --to D:\\ISOs"));
        assert!(text.contains("\"https://cdn.example/b.zip\""));

        let again = History::load(Some(dir.path()));
        assert_eq!(again.lines().len(), 3);
        let mut again = again;
        again.clear().unwrap();
        assert!(History::load(Some(dir.path())).lines().is_empty());
    }

    #[test]
    fn the_summary_counts_what_ended_since_last_time_failures_first() {
        let job = |id: u8, state: &str, finished: &str| -> JobSnapshot {
            serde_json::from_value(serde_json::json!({
                "job_id": format!("3f1c2a9b-0000-4000-8000-0000000000{id:02}"),
                "kind": "file", "state": state, "job_revision": 1, "last_seq": 1,
                "source_display": format!("https://a.test/{id}.zip"),
                "progress": { "bytes_received": 0 },
                "created_at": "2026-09-26T10:00:00Z",
                "finished_at": finished,
            }))
            .unwrap()
        };
        let history = [
            job(1, "completed", "2026-09-26T09:00:00Z"), // before: not counted
            job(2, "completed", "2026-09-26T12:00:00Z"),
            job(3, "failed", "2026-09-26T11:00:00Z"),
            job(4, "cancelled", "2026-09-26T12:30:00Z"), // the person did it
        ];
        let since = fetchpath_protocol::Timestamp::parse("2026-09-26T10:00:00Z")
            .unwrap()
            .unix_ms();
        let glyphs = view::ASCII;
        let lines = away_summary(&history, since, &glyphs);
        assert_eq!(lines[0].text, "While you were away: 1 finished, 1 failed.");
        assert_eq!(lines.len(), 3);
        assert!(lines[1].text.contains("3.zip"), "failures first: {lines:?}");
        assert!(away_summary(&history, i64::MAX, &glyphs).is_empty());
    }

    #[test]
    fn an_unparseable_link_is_still_cut() {
        assert_eq!(
            strip_links("https://a:b@[bad/x?q=1"),
            "https://[bad/x".to_owned()
        );
    }
}
