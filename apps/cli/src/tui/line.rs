//! What the prompt accepts: links, and `/` commands that mirror the command
//! line, with completion from commands, jobs, recent folders and folders on
//! disk.

use super::review::Draft;
use crate::client::{self, Engine};
use crate::queue::{self, Control};
use crate::tools;
use crate::when;
use fetchpath_protocol::command::{Command, JobFilter};
use fetchpath_protocol::message::CommandResult;
use fetchpath_protocol::model::JobState;
use fetchpath_protocol::{JobSnapshot, ProtocolError, SensitiveUrl, Timestamp};
use std::path::{Path, PathBuf};

/// How a line of output should look; plain mode prints only the text.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Tone {
    Normal,
    Dim,
    Good,
    Bad,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Out {
    pub tone: Tone,
    pub text: String,
}

impl Out {
    pub fn new(tone: Tone, text: impl Into<String>) -> Self {
        Self {
            tone,
            text: text.into(),
        }
    }
}

#[derive(Default, Debug)]
pub struct Reply {
    pub lines: Vec<Out>,
    pub quit: bool,
    /// Links to look at and confirm before they download.
    pub drafts: Vec<Draft>,
    /// Show the video tools setup card.
    pub set_up_tools: bool,
    /// A menu to open instead of printing (inline mode only).
    pub open: Option<Open>,
}

/// Menus a command can open in the inline view.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Open {
    Settings,
    /// The job list; with a command name, choosing a job runs it.
    Jobs(Option<&'static str>),
}

impl Reply {
    fn say(&mut self, tone: Tone, text: impl Into<String>) {
        self.lines.push(Out::new(tone, text));
    }
}

/// What a command's words stand for, for completion.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Takes {
    Nothing,
    Links,
    Jobs,
    Text,
    Setting,
}

pub struct Spec {
    pub name: &'static str,
    aliases: &'static [&'static str],
    pub usage: &'static str,
    pub summary: &'static str,
    takes: Takes,
}

impl Spec {
    /// Whether the command needs words after it before it can run. Only
    /// `/add` does; job commands without a job open a picker instead.
    pub fn needs_words(&self) -> bool {
        self.takes == Takes::Links
    }
}

pub const COMMANDS: &[Spec] = &[
    Spec {
        name: "add",
        aliases: &[],
        usage: "/add LINK... [--to FOLDER] [--at TIME] [--sha256 HEX] [--quality Q]",
        summary: "Look at links and confirm to download (or just paste one)",
        takes: Takes::Links,
    },
    Spec {
        name: "queue",
        aliases: &["ls", "list"],
        usage: "/queue [--active | --failed]",
        summary: "Choose a download to pause, resume, retry or remove",
        takes: Takes::Nothing,
    },
    Spec {
        name: "show",
        aliases: &[],
        usage: "/show JOB...",
        summary: "Describe downloads in full",
        takes: Takes::Jobs,
    },
    Spec {
        name: "pause",
        aliases: &[],
        usage: "/pause JOB...",
        summary: "Pause downloads",
        takes: Takes::Jobs,
    },
    Spec {
        name: "resume",
        aliases: &[],
        usage: "/resume JOB...",
        summary: "Resume paused or scheduled downloads",
        takes: Takes::Jobs,
    },
    Spec {
        name: "cancel",
        aliases: &[],
        usage: "/cancel JOB...",
        summary: "Cancel downloads",
        takes: Takes::Jobs,
    },
    Spec {
        name: "retry",
        aliases: &[],
        usage: "/retry JOB...",
        summary: "Try failed downloads again",
        takes: Takes::Jobs,
    },
    Spec {
        name: "rm",
        aliases: &["remove"],
        usage: "/rm JOB...",
        summary: "Remove finished downloads from the list",
        takes: Takes::Jobs,
    },
    Spec {
        name: "history",
        aliases: &[],
        usage: "/history [TEXT] [--limit N]",
        summary: "Search finished downloads",
        takes: Takes::Text,
    },
    Spec {
        name: "settings",
        aliases: &[],
        usage: "/settings [NAME [VALUE]]",
        summary: "Change settings from a menu",
        takes: Takes::Setting,
    },
    Spec {
        name: "tools",
        aliases: &[],
        usage: "/tools [install | use FOLDER]",
        summary: "Video tools (yt-dlp, ffmpeg): check or set them up",
        takes: Takes::Nothing,
    },
    Spec {
        name: "engine",
        aliases: &[],
        usage: "/engine",
        summary: "Show the engine's status",
        takes: Takes::Nothing,
    },
    Spec {
        name: "help",
        aliases: &["?"],
        usage: "/help [COMMAND]",
        summary: "Show commands and keys",
        takes: Takes::Nothing,
    },
    Spec {
        name: "quit",
        aliases: &["exit", "q"],
        usage: "/quit",
        summary: "Leave; downloads carry on in the background",
        takes: Takes::Nothing,
    },
];

pub fn find(name: &str) -> Option<&'static Spec> {
    let name = name.to_ascii_lowercase();
    COMMANDS
        .iter()
        .find(|spec| spec.name == name || spec.aliases.contains(&name.as_str()))
}

/// Splits a line into words at spaces. Double quotes keep spaces inside a
/// word, so folders such as `"C:\My Files"` stay whole; backslashes are
/// ordinary characters, as in Windows paths.
pub fn words(line: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut quoted = false;
    let mut started = false;
    for character in line.chars() {
        match character {
            '"' => {
                quoted = !quoted;
                started = true;
            }
            c if c.is_whitespace() && !quoted => {
                if started {
                    words.push(std::mem::take(&mut word));
                    started = false;
                }
            }
            c => {
                word.push(c);
                started = true;
            }
        }
    }
    if started {
        words.push(word);
    }
    words
}

/// The command a line runs: `/name args`, or a bare line of links, which
/// is `/add`.
pub fn parse(line: &str) -> Option<(&'static Spec, Vec<String>)> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    match line.strip_prefix('/') {
        Some(rest) => {
            let mut words = words(rest);
            if words.is_empty() {
                return Some((find("help").expect("help exists"), Vec::new()));
            }
            let name = words.remove(0);
            match find(&name) {
                Some(spec) => Some((spec, words)),
                None => Some((&UNKNOWN, vec![name])),
            }
        }
        None => Some((find("add").expect("add exists"), words(line))),
    }
}

const UNKNOWN: Spec = Spec {
    name: "",
    aliases: &[],
    usage: "",
    summary: "",
    takes: Takes::Nothing,
};

/// Runs one line against the engine. Errors become lines, never a crash:
/// the prompt stays open whatever a command does.
/// `menus` is true in the inline view, where commands that would otherwise
/// need typed names open a menu to choose from.
pub fn run(engine: &Engine, jobs: &[JobSnapshot], line: &str, menus: bool) -> Reply {
    let mut reply = Reply::default();
    let Some((spec, args)) = parse(line) else {
        return reply;
    };
    if spec.name.is_empty() {
        reply.say(
            Tone::Bad,
            format!("There is no /{} command. Type /help to see them.", args[0]),
        );
        return reply;
    }
    if menus && let Some(open) = menu_for(spec, &args) {
        reply.open = Some(open);
        return reply;
    }
    if let Err(error) = execute(engine, jobs, spec, &args, &mut reply) {
        reply.say(Tone::Bad, error.message);
    }
    reply
}

/// The menu a command opens when given nothing to act on.
fn menu_for(spec: &Spec, args: &[String]) -> Option<Open> {
    if !args.is_empty() {
        return None;
    }
    match spec.name {
        "settings" => Some(Open::Settings),
        "queue" => Some(Open::Jobs(None)),
        "show" | "pause" | "resume" | "cancel" | "retry" | "rm" => {
            Some(Open::Jobs(Some(spec.name)))
        }
        _ => None,
    }
}

fn usage_error(spec: &Spec, message: &str) -> ProtocolError {
    client::input_error(&format!("{message}. Usage: {}", spec.usage))
}

fn execute(
    engine: &Engine,
    jobs: &[JobSnapshot],
    spec: &Spec,
    args: &[String],
    reply: &mut Reply,
) -> Result<(), ProtocolError> {
    let flags: &[&str] = match spec.name {
        "add" => &["--to", "--at", "--sha256", "--quality"],
        "queue" => &["--active", "--failed"],
        "history" => &["--limit"],
        _ => &[],
    };
    let parsed = queue::parse(args, flags).map_err(|message| usage_error(spec, &message))?;
    match spec.name {
        "add" => add(spec, &parsed, reply),
        "queue" => {
            let all = engine.jobs(JobFilter::All)?;
            let shown: Vec<&JobSnapshot> = all
                .iter()
                .filter(|job| {
                    if parsed.failed {
                        job.state == JobState::Failed
                    } else if parsed.active {
                        !job.state.is_terminal() && job.state != JobState::Failed
                    } else {
                        true
                    }
                })
                .collect();
            if shown.is_empty() {
                reply.say(Tone::Normal, "No downloads.");
                return Ok(());
            }
            for line in queue::table_lines(&all, &shown) {
                reply.say(Tone::Normal, line);
            }
            Ok(())
        }
        "show" => {
            if parsed.words.is_empty() {
                return Err(usage_error(spec, "Name a download"));
            }
            for (number, job) in resolve(jobs, &parsed.words)?.iter().enumerate() {
                if number > 0 {
                    reply.say(Tone::Normal, "");
                }
                for line in queue::job_lines(job) {
                    reply.say(Tone::Normal, line);
                }
            }
            Ok(())
        }
        "pause" | "resume" | "cancel" | "retry" | "rm" => {
            let action = match spec.name {
                "pause" => Control::Pause,
                "resume" => Control::Resume,
                "cancel" => Control::Cancel,
                "retry" => Control::Retry,
                _ => Control::Remove,
            };
            if parsed.words.is_empty() {
                return Err(usage_error(spec, "Name at least one download"));
            }
            for job in resolve(jobs, &parsed.words)? {
                match engine.send(queue::control_command(action, &job)) {
                    Ok(result) => reply.say(Tone::Normal, queue::describe(action, &job, &result)),
                    Err(error) => reply.say(
                        Tone::Bad,
                        format!(
                            "{} {}: {}",
                            client::short_id(&job),
                            client::name(&job),
                            error.message
                        ),
                    ),
                }
            }
            Ok(())
        }
        "history" => {
            let query = (!parsed.words.is_empty()).then(|| parsed.words.join(" "));
            let found = match engine.send(Command::History {
                query,
                limit: parsed.limit,
            })? {
                CommandResult::Jobs { jobs } => jobs,
                other => return Err(client::unexpected(&other)),
            };
            if found.is_empty() {
                reply.say(Tone::Normal, "No finished downloads match.");
                return Ok(());
            }
            let all = engine.jobs(JobFilter::All)?;
            for line in queue::table_lines(&all, &found.iter().collect::<Vec<_>>()) {
                reply.say(Tone::Normal, line);
            }
            Ok(())
        }
        "settings" => {
            if parsed.words.len() > 2 {
                return Err(usage_error(spec, "Give at most a name and a value"));
            }
            let (_, lines) = queue::settings_outcome(engine, &parsed.words)?;
            for line in lines {
                reply.say(Tone::Normal, line);
            }
            Ok(())
        }
        "tools" => match parsed
            .words
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .as_slice()
        {
            [] | ["status"] => {
                let status = tools::status(engine)?;
                for line in tools::status_lines(&status) {
                    reply.say(Tone::Normal, line);
                }
                if !status.ready {
                    reply.say(
                        Tone::Normal,
                        "Set them up with /tools install, or use copies you have with /tools use FOLDER.",
                    );
                }
                Ok(())
            }
            ["install"] => {
                reply.set_up_tools = true;
                Ok(())
            }
            ["use", folder] => {
                let accepted = fetchpath_media::setup::use_directory(std::path::Path::new(folder))
                    .map_err(|message| client::input_error(&message))?;
                tools::record(engine, &accepted)?;
                for line in tools::status_lines(&tools::status(engine)?) {
                    reply.say(Tone::Good, line);
                }
                Ok(())
            }
            _ => Err(usage_error(spec, "Say install, or use and a folder")),
        },
        "engine" => {
            let status = match engine.send(Command::EngineStatus)? {
                CommandResult::EngineStatus { status } => status,
                other => return Err(client::unexpected(&other)),
            };
            reply.say(
                Tone::Normal,
                format!(
                    "Engine {} running since {}; {} connected, {} active.",
                    status.engine_version,
                    when::local(status.started_at),
                    status.connected_clients,
                    status.active_jobs
                ),
            );
            if let Some(reason) = status.queue_read_only {
                reply.say(Tone::Bad, reason.message);
            }
            Ok(())
        }
        "help" => {
            help(parsed.words.first().map(String::as_str), reply);
            Ok(())
        }
        "quit" => {
            reply.quit = true;
            Ok(())
        }
        _ => unreachable!("every command in the table runs"),
    }
}

fn resolve(jobs: &[JobSnapshot], references: &[String]) -> Result<Vec<JobSnapshot>, ProtocolError> {
    references
        .iter()
        .map(|reference| {
            client::resolve(jobs, reference)
                .cloned()
                .map_err(|message| client::input_error(&message))
        })
        .collect()
}

/// Turns links into drafts to look at and confirm; nothing starts here.
fn add(spec: &Spec, parsed: &queue::Args, reply: &mut Reply) -> Result<(), ProtocolError> {
    if parsed.words.is_empty() {
        return Err(usage_error(spec, "Give a link"));
    }
    if parsed.sha256.is_some() && parsed.words.len() > 1 {
        return Err(client::input_error(
            "A checksum describes one file; add links with --sha256 one at a time.",
        ));
    }
    let at = parsed
        .at
        .as_deref()
        .map(|at| when::parse(at, Timestamp::now()))
        .transpose()
        .map_err(|message| client::input_error(&message))?;
    for link in &parsed.words {
        match SensitiveUrl::try_from(link.clone()) {
            Ok(url) => reply.drafts.push(Draft {
                link: link.clone(),
                url,
                to: parsed.to.clone(),
                at,
                sha256: parsed.sha256.clone(),
                quality: parsed.quality.clone(),
            }),
            Err(message) => reply.say(Tone::Bad, format!("{link} cannot be used: {message}.")),
        }
    }
    Ok(())
}

fn help(topic: Option<&str>, reply: &mut Reply) {
    if let Some(topic) = topic {
        match find(topic.trim_start_matches('/')) {
            Some(spec) => {
                reply.say(Tone::Normal, spec.usage);
                reply.say(Tone::Normal, format!("  {}", spec.summary));
            }
            None => reply.say(Tone::Bad, format!("There is no /{topic} command.")),
        }
        return;
    }
    reply.say(
        Tone::Normal,
        "Paste or type a link to download it. Commands start with /:",
    );
    for spec in COMMANDS {
        reply.say(
            Tone::Normal,
            format!("  {:<11}{}", format!("/{}", spec.name), spec.summary),
        );
    }
    reply.say(
        Tone::Normal,
        "JOB is a number from the panel or /queue, or the start of its id.",
    );
    reply.say(
        Tone::Normal,
        "Keys: Tab completes, Up and Down recall earlier lines, Esc clears the line, Ctrl+C on an empty line leaves. Typing / lists the commands to choose from; menus and cards take the mouse too.",
    );
}

// ---------------------------------------------------------------- completion

/// A word the prompt could put in place of the one being typed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Candidate {
    pub insert: String,
    /// What the hint line shows for it.
    pub label: String,
    /// Whether a space follows once it is the only choice.
    pub finished: bool,
}

pub struct Completion {
    /// Byte offset in the line where the replaced word starts.
    pub start: usize,
    pub candidates: Vec<Candidate>,
}

/// What completion knows besides the line: the jobs, and folders downloads
/// went to recently.
pub struct Context<'a> {
    pub jobs: &'a [JobSnapshot],
    pub settings: &'a [String],
}

impl Context<'_> {
    /// Folders of saved or planned downloads, most recent first.
    fn recent_folders(&self) -> Vec<String> {
        let mut folders: Vec<String> = Vec::new();
        for job in self.jobs {
            if let Some(folder) = job
                .destination
                .as_deref()
                .and_then(|path| Path::new(path).parent())
                .map(|folder| format!("{}\\", folder.display()))
                && !folders.contains(&folder)
            {
                folders.push(folder);
            }
        }
        folders
    }
}

/// Candidates for the word before the cursor: a command name after `/`,
/// a folder after `--to`, a job for commands that take jobs, a setting's
/// name for `/settings`.
pub fn complete(before_cursor: &str, context: &Context) -> Completion {
    let start = word_start(before_cursor);
    let word = before_cursor[start..].trim_start_matches('"');
    let previous = words(&before_cursor[..start]);
    let command_word = before_cursor.starts_with('/') && previous.is_empty();
    let candidates = if command_word {
        let typed = word.trim_start_matches('/').to_ascii_lowercase();
        COMMANDS
            .iter()
            .filter(|spec| spec.name.starts_with(&typed))
            .map(|spec| Candidate {
                insert: format!("/{}", spec.name),
                label: format!("/{}", spec.name),
                finished: true,
            })
            .collect()
    } else if previous.last().is_some_and(|last| last == "--to") {
        folders(word, context)
    } else if word.starts_with('-') {
        Vec::new()
    } else {
        let takes = match before_cursor.strip_prefix('/') {
            Some(rest) => words(rest)
                .first()
                .and_then(|name| find(name))
                .map_or(Takes::Nothing, |spec| spec.takes),
            None => Takes::Links,
        };
        match takes {
            Takes::Jobs => jobs(word, context.jobs),
            Takes::Setting if previous.len() == 1 => context
                .settings
                .iter()
                .filter(|name| name.starts_with(&word.to_ascii_lowercase()))
                .map(|name| Candidate {
                    insert: name.clone(),
                    label: name.clone(),
                    finished: true,
                })
                .collect(),
            _ => Vec::new(),
        }
    };
    Completion {
        start: before_cursor.len() - before_cursor[start..].len(),
        candidates,
    }
}

/// Where the word under the cursor starts, respecting an open quote.
fn word_start(line: &str) -> usize {
    let mut quoted = false;
    let mut start = 0;
    for (index, character) in line.char_indices() {
        match character {
            '"' => quoted = !quoted,
            c if c.is_whitespace() && !quoted => start = index + c.len_utf8(),
            _ => {}
        }
    }
    start
}

/// Jobs whose number, id or name starts with the typed text; the insert is
/// the short id, which does not shift as the queue grows.
fn jobs(word: &str, jobs: &[JobSnapshot]) -> Vec<Candidate> {
    let typed = word.to_lowercase();
    jobs.iter()
        .enumerate()
        .filter(|(position, job)| {
            if typed.is_empty() {
                return super::live::group(job).is_some();
            }
            (position + 1).to_string() == typed
                || job.job_id.as_str().starts_with(&typed)
                || client::name(job).to_lowercase().starts_with(&typed)
        })
        .map(|(position, job)| Candidate {
            insert: client::short_id(job).to_owned(),
            label: format!("{} {}", position + 1, client::name(job)),
            finished: true,
        })
        .collect()
}

/// Recent download folders matching the typed text, then folders on disk
/// under the typed path.
pub fn folders(word: &str, context: &Context) -> Vec<Candidate> {
    let typed = word.to_lowercase();
    let mut found: Vec<String> = context
        .recent_folders()
        .into_iter()
        .filter(|folder| folder.to_lowercase().starts_with(&typed))
        .collect();
    let (parent, prefix) = match word.rfind(['\\', '/']) {
        Some(at) => (PathBuf::from(&word[..=at]), &word[at + 1..]),
        None => (PathBuf::from("."), word),
    };
    if !word.is_empty()
        && let Ok(entries) = std::fs::read_dir(&parent)
    {
        let prefix = prefix.to_lowercase();
        let base = &word[..word.len() - prefix.len()];
        for entry in entries.flatten().take(500) {
            let name = entry.file_name().to_string_lossy().into_owned();
            if entry.file_type().is_ok_and(|kind| kind.is_dir())
                && name.to_lowercase().starts_with(&prefix)
            {
                let folder = format!("{base}{name}\\");
                if !found.contains(&folder) {
                    found.push(folder);
                }
            }
            if found.len() >= 50 {
                break;
            }
        }
    }
    found
        .into_iter()
        .map(|folder| Candidate {
            insert: if folder.contains(' ') {
                format!("\"{folder}\"")
            } else {
                folder.clone()
            },
            label: folder,
            finished: false,
        })
        .collect()
}

/// The longest start every candidate shares, ignoring case.
pub fn common_prefix(candidates: &[Candidate]) -> String {
    let Some(first) = candidates.first() else {
        return String::new();
    };
    let mut length = first.insert.len();
    for candidate in &candidates[1..] {
        length = first
            .insert
            .char_indices()
            .zip(candidate.insert.chars())
            .take_while(|((_, a), b)| a.to_lowercase().eq(b.to_lowercase()))
            .map(|((index, a), _)| index + a.len_utf8())
            .last()
            .unwrap_or(0)
            .min(length);
    }
    first.insert[..length].to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(id: &str, name: &str, state: &str) -> JobSnapshot {
        serde_json::from_value(serde_json::json!({
            "job_id": id, "kind": "file", "state": state, "job_revision": 1,
            "last_seq": 1, "source_display": "https://a.test/x",
            "destination": format!("C:\\Users\\person\\Downloads\\{name}"),
            "progress": { "bytes_received": 0 },
            "created_at": "2026-09-26T10:00:00Z",
        }))
        .unwrap()
    }

    #[test]
    fn words_keep_quoted_folders_whole_and_backslashes_literal() {
        assert_eq!(
            words(r#"https://a.test/x --to "C:\My Files\"  --at 18:00"#),
            ["https://a.test/x", "--to", r"C:\My Files\", "--at", "18:00"]
        );
        assert_eq!(words("  "), Vec::<String>::new());
        assert_eq!(words(r#"a "" b"#), ["a", "", "b"]);
    }

    #[test]
    fn a_bare_line_adds_its_links_and_a_slash_runs_a_command() {
        let (spec, args) = parse("https://a.test/x https://a.test/y").unwrap();
        assert_eq!((spec.name, args.len()), ("add", 2));
        let (spec, args) = parse("/LS --active").unwrap();
        assert_eq!((spec.name, args), ("queue", vec!["--active".to_owned()]));
        assert_eq!(parse("/").unwrap().0.name, "help");
        assert_eq!(parse("/nope").unwrap().0.name, "");
        assert!(parse("   ").is_none());
    }

    #[test]
    fn completion_offers_commands_jobs_settings_and_recent_folders() {
        let jobs = [
            job(
                "3f1c2a9b-0000-4000-8000-000000000001",
                "ubuntu.iso",
                "running",
            ),
            job(
                "9a000000-0000-4000-8000-000000000002",
                "notes.pdf",
                "completed",
            ),
        ];
        let settings = [
            "max-concurrent-jobs".to_owned(),
            "default-destination-dir".to_owned(),
        ];
        let context = Context {
            jobs: &jobs,
            settings: &settings,
        };
        let inserts = |line: &str| -> Vec<String> {
            complete(line, &context)
                .candidates
                .into_iter()
                .map(|candidate| candidate.insert)
                .collect()
        };
        assert_eq!(inserts("/pa"), ["/pause"]);
        assert_eq!(inserts("/re"), ["/resume", "/retry"]);
        assert_eq!(inserts("/pause ubu"), ["3f1c2a9b"]);
        assert_eq!(inserts("/show 2"), ["9a000000"]);
        // With nothing typed, only jobs still in the panel are offered.
        assert_eq!(inserts("/pause "), ["3f1c2a9b"]);
        assert_eq!(inserts("/settings max"), ["max-concurrent-jobs"]);
        assert_eq!(
            inserts("/settings max-concurrent-jobs "),
            Vec::<String>::new()
        );
        assert_eq!(
            inserts(r"https://a.test/x --to C:\Users\person\Dow"),
            [r"C:\Users\person\Downloads\"]
        );
        let completion = complete("/pause 1 ubu", &context);
        assert_eq!(completion.start, "/pause 1 ".len());
    }

    #[test]
    fn the_common_prefix_ignores_case() {
        let candidate = |insert: &str| Candidate {
            insert: insert.to_owned(),
            label: insert.to_owned(),
            finished: true,
        };
        assert_eq!(
            common_prefix(&[candidate("/resume"), candidate("/retry")]),
            "/re"
        );
        assert_eq!(common_prefix(&[candidate("Abc"), candidate("abd")]), "Ab");
        assert_eq!(common_prefix(&[candidate("x")]), "x");
    }
}
