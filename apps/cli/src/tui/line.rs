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
    /// Interactive AI agent permissions menu.
    Agents,
    /// The full-screen dashboard.
    Dashboard,
    /// Cards for agents' requests that wait for the person.
    Approvals,
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

/// Grouping for `/help` so commands are easy to scan.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Category {
    Downloads,
    Queue,
    Agents,
    Setup,
}

impl Category {
    const ALL: [Self; 4] = [Self::Downloads, Self::Queue, Self::Agents, Self::Setup];

    fn title(self) -> &'static str {
        match self {
            Self::Downloads => "Downloads",
            Self::Queue => "Queue & Control",
            Self::Agents => "Agents & Inbox",
            Self::Setup => "Setup & Terminal",
        }
    }
}

pub struct Spec {
    pub name: &'static str,
    aliases: &'static [&'static str],
    pub usage: &'static str,
    pub summary: &'static str,
    takes: Takes,
    category: Category,
}

impl Spec {
    /// Whether the command needs words after it before it can run. Only
    /// link-taking commands do; job commands without a job open a picker instead.
    pub fn needs_words(&self) -> bool {
        self.takes == Takes::Links
    }

    pub fn aliases(&self) -> &'static [&'static str] {
        self.aliases
    }
}

pub const COMMANDS: &[Spec] = &[
    Spec {
        name: "add",
        aliases: &[],
        usage: "/add LINK... [--to FOLDER] [--at TIME] [--sha256 HEX] [--quality Q] [--upload]",
        summary: "Look at links and confirm to download (or just paste one)",
        takes: Takes::Links,
        category: Category::Downloads,
    },
    Spec {
        name: "torrent",
        aliases: &[],
        usage: "/torrent MAGNET|HTTPS_TORRENT|TORRENT_FILE [--to NEW_FOLDER] [--upload]",
        summary: "Add a torrent; peers see your IP address, upload stays off unless chosen",
        takes: Takes::Links,
        category: Category::Downloads,
    },
    Spec {
        name: "inspect",
        aliases: &[],
        usage: "/inspect LINK",
        summary: "Inspect a link's file size, media formats, or repository files",
        takes: Takes::Links,
        category: Category::Downloads,
    },
    Spec {
        name: "status",
        aliases: &["st"],
        usage: "/status",
        summary: "One-glance engine and queue summary",
        takes: Takes::Nothing,
        category: Category::Queue,
    },
    Spec {
        name: "queue",
        aliases: &["ls", "list"],
        usage: "/queue [--active | --paused | --failed | --finished | --approvals]",
        summary: "Choose a download to pause, resume, retry or remove",
        takes: Takes::Nothing,
        category: Category::Queue,
    },
    Spec {
        name: "dashboard",
        aliases: &["dash"],
        usage: "/dashboard",
        summary: "Full screen: filterable queue with speed and connections (F2)",
        takes: Takes::Nothing,
        category: Category::Queue,
    },
    Spec {
        name: "show",
        aliases: &[],
        usage: "/show JOB...",
        summary: "Describe downloads in full",
        takes: Takes::Jobs,
        category: Category::Queue,
    },
    Spec {
        name: "pause",
        aliases: &[],
        usage: "/pause JOB...|all",
        summary: "Pause downloads (or `all` active)",
        takes: Takes::Jobs,
        category: Category::Queue,
    },
    Spec {
        name: "resume",
        aliases: &[],
        usage: "/resume JOB...|all",
        summary: "Resume paused or scheduled downloads (or `all`)",
        takes: Takes::Jobs,
        category: Category::Queue,
    },
    Spec {
        name: "cancel",
        aliases: &[],
        usage: "/cancel JOB...",
        summary: "Cancel downloads",
        takes: Takes::Jobs,
        category: Category::Queue,
    },
    Spec {
        name: "retry",
        aliases: &[],
        usage: "/retry JOB...|all",
        summary: "Try failed downloads again (or `all`)",
        takes: Takes::Jobs,
        category: Category::Queue,
    },
    Spec {
        name: "rm",
        aliases: &["remove"],
        usage: "/rm JOB...|all",
        summary: "Remove finished downloads from the list (keeps files)",
        takes: Takes::Jobs,
        category: Category::Queue,
    },
    Spec {
        name: "clean",
        aliases: &["clear-done"],
        usage: "/clean [--failed]",
        summary: "Remove finished and cancelled downloads from the list",
        takes: Takes::Nothing,
        category: Category::Queue,
    },
    Spec {
        name: "rename",
        aliases: &[],
        usage: "/rename JOB NAME",
        summary: "Save a download stopped by an existing file under another name",
        takes: Takes::Jobs,
        category: Category::Queue,
    },
    Spec {
        name: "folder",
        aliases: &["open", "reveal"],
        usage: "/folder [JOB]",
        summary: "Show a download's file in File Explorer",
        takes: Takes::Jobs,
        category: Category::Queue,
    },
    Spec {
        name: "history",
        aliases: &[],
        usage: "/history [TEXT] [--limit N]",
        summary: "Search finished downloads",
        takes: Takes::Text,
        category: Category::Queue,
    },
    Spec {
        name: "approvals",
        aliases: &[],
        usage: "/approvals",
        summary: "Answer agents' requests that wait for you",
        takes: Takes::Nothing,
        category: Category::Agents,
    },
    Spec {
        name: "approve",
        aliases: &[],
        usage: "/approve JOB...|all",
        summary: "Let an agent's request download",
        takes: Takes::Jobs,
        category: Category::Agents,
    },
    Spec {
        name: "deny",
        aliases: &[],
        usage: "/deny JOB...|all",
        summary: "Refuse an agent's request",
        takes: Takes::Jobs,
        category: Category::Agents,
    },
    Spec {
        name: "inbox",
        aliases: &[],
        usage: "/inbox [accept OFFER [--to FOLDER] | decline OFFER]",
        summary: "View, accept, or decline files an agent offered you",
        takes: Takes::Text,
        category: Category::Agents,
    },
    Spec {
        name: "agents",
        aliases: &[],
        usage: "/agents [mode NAME|--all ask|scoped|auto|full | grant NAME FOLDER... | revoke NAME [FOLDER...] | limit NAME --size S --per-hour N | auto NAME on|off]",
        summary: "Manage AI agent permission modes, folders, and limits",
        takes: Takes::Text,
        category: Category::Agents,
    },
    Spec {
        name: "setup",
        aliases: &["doctor"],
        usage: "/setup [media]",
        summary: "Check all requirements (media, JS runtime, torrent, AI agents)",
        takes: Takes::Nothing,
        category: Category::Setup,
    },
    Spec {
        name: "tools",
        aliases: &[],
        usage: "/tools [install | use FOLDER]",
        summary: "Video tools (yt-dlp, ffmpeg): check or set them up",
        takes: Takes::Nothing,
        category: Category::Setup,
    },
    Spec {
        name: "settings",
        aliases: &[],
        usage: "/settings [NAME [VALUE]]",
        summary: "Change settings from a menu",
        takes: Takes::Setting,
        category: Category::Setup,
    },
    Spec {
        name: "rules",
        aliases: &[],
        usage: "/rules [add ... | rm ID | test LINK]",
        summary: "Where downloads go by site, type or size, and why",
        takes: Takes::Text,
        category: Category::Setup,
    },
    Spec {
        name: "hub",
        aliases: &[],
        usage: "/hub [on | off]",
        summary: "Keep the engine running in the background (always on)",
        takes: Takes::Text,
        category: Category::Setup,
    },
    Spec {
        name: "web",
        aliases: &[],
        usage: "/web on | off | open | sign-out",
        summary: "Control the local loopback web UI",
        takes: Takes::Text,
        category: Category::Setup,
    },
    Spec {
        name: "theme",
        aliases: &[],
        usage: "/theme [NAME | glyphs auto|unicode|ascii | density comfortable|compact]",
        summary: "Colors, symbols and density of this terminal",
        takes: Takes::Nothing,
        category: Category::Setup,
    },
    Spec {
        name: "keys",
        aliases: &[],
        usage: "/keys [ACTION KEY | ACTION default]",
        summary: "Keys for the dashboard and its actions",
        takes: Takes::Nothing,
        category: Category::Setup,
    },
    Spec {
        name: "alias",
        aliases: &[],
        usage: "/alias [NAME COMMAND... | remove NAME]",
        summary: "Your own names for Fetchpath commands",
        takes: Takes::Nothing,
        category: Category::Setup,
    },
    Spec {
        name: "forget",
        aliases: &[],
        usage: "/forget",
        summary: "Forget the lines typed at this prompt",
        takes: Takes::Nothing,
        category: Category::Setup,
    },
    Spec {
        name: "engine",
        aliases: &[],
        usage: "/engine",
        summary: "Show the engine's status",
        takes: Takes::Nothing,
        category: Category::Setup,
    },
    Spec {
        name: "help",
        aliases: &["?"],
        usage: "/help [COMMAND]",
        summary: "Show commands and keys",
        takes: Takes::Nothing,
        category: Category::Setup,
    },
    Spec {
        name: "quit",
        aliases: &["exit", "q"],
        usage: "/quit",
        summary: "Leave; downloads carry on in the background",
        takes: Takes::Nothing,
        category: Category::Setup,
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
    category: Category::Setup,
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
        let candidates = COMMANDS
            .iter()
            .flat_map(|spec| std::iter::once(spec.name).chain(spec.aliases.iter().copied()));
        if let Some(suggestion) = crate::cli_help::suggest(&args[0], candidates) {
            reply.say(
                Tone::Bad,
                format!(
                    "There is no /{} command. Did you mean /{suggestion}? Type /help to see them.",
                    args[0]
                ),
            );
        } else {
            reply.say(
                Tone::Bad,
                format!("There is no /{} command. Type /help to see them.", args[0]),
            );
        }
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
        "agents" => Some(Open::Agents),
        "dashboard" => Some(Open::Dashboard),
        "approvals" => Some(Open::Approvals),
        "queue" => Some(Open::Jobs(None)),
        "show" | "pause" | "resume" | "cancel" | "retry" | "rm" | "folder" | "approve" | "deny"
        | "rename" => Some(Open::Jobs(Some(spec.name))),
        _ => None,
    }
}

/// `/rules`: the same words as `fetchpath rules`.
fn rules(
    engine: &Engine,
    spec: &Spec,
    args: &[String],
    reply: &mut Reply,
) -> Result<(), ProtocolError> {
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    let lines = match words.as_slice() {
        [] | ["list"] => {
            crate::rules::list_lines(&crate::rules::fetch(engine, Command::ListRules)?)
        }
        ["add", rest @ ..] => {
            let (rule, position) =
                crate::rules::parse_add(rest).map_err(|message| usage_error(spec, &message))?;
            crate::rules::list_lines(&crate::rules::fetch(
                engine,
                Command::AddRule {
                    rule: Box::new(rule),
                    position,
                },
            )?)
        }
        ["rm", id] => {
            let rule_id = id
                .trim_start_matches('#')
                .parse()
                .map_err(|_| usage_error(spec, "Give the rule's number"))?;
            crate::rules::list_lines(&crate::rules::fetch(
                engine,
                Command::RemoveRule { rule_id },
            )?)
        }
        ["test", link] => {
            crate::rules::verdict_lines(crate::rules::inspect(engine, link)?.rules.as_ref())
        }
        _ => return Err(usage_error(spec, "Say add, rm or test")),
    };
    for line in lines {
        reply.say(Tone::Normal, line);
    }
    Ok(())
}

/// `/agents`: the same words and engine requests as `fetchpath agents`.
fn agents(
    engine: &Engine,
    spec: &Spec,
    args: &[String],
    reply: &mut Reply,
) -> Result<(), ProtocolError> {
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    let request = crate::agents::Request::parse(&words)
        .ok_or_else(|| usage_error(spec, "Say grant, revoke, limit or auto"))?;
    for line in crate::agents::lines(&request.perform(engine)?) {
        reply.say(Tone::Normal, line);
    }
    Ok(())
}

/// `/hub`: always on, as `fetchpath hub` sets it.
fn hub(
    engine: &Engine,
    spec: &Spec,
    args: &[String],
    reply: &mut Reply,
) -> Result<(), ProtocolError> {
    let words: Vec<String> = match args {
        [] => vec!["hub-mode".into()],
        [word] if word == "on" => vec!["hub-mode".into(), "true".into()],
        [word] if word == "off" => vec!["hub-mode".into(), "false".into()],
        _ => return Err(usage_error(spec, "Say on or off")),
    };
    queue::settings_outcome(engine, &words)?;
    for line in queue::hub_lines(engine)? {
        reply.say(Tone::Normal, line);
    }
    Ok(())
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
    if spec.name == "rules" {
        return rules(engine, spec, args, reply);
    }
    if spec.name == "agents" {
        return agents(engine, spec, args, reply);
    }
    if spec.name == "hub" {
        return hub(engine, spec, args, reply);
    }
    if spec.name == "inbox" {
        for line in crate::inbox::run_lines(engine, args)? {
            reply.say(Tone::Normal, line);
        }
        return Ok(());
    }
    if spec.name == "web" {
        let [action] = args else {
            return Err(usage_error(spec, "Say on, off, open, or sign-out"));
        };
        reply.say(Tone::Normal, crate::web::act_message(engine, action)?);
        return Ok(());
    }
    let flags: &[&str] = match spec.name {
        "add" => &[
            "--to",
            "--at",
            "--sha256",
            "--quality",
            "--discover-peers",
            "--upload",
        ],
        "torrent" => &["--to", "--discover-peers", "--upload"],
        "queue" => &[
            "--active",
            "--failed",
            "--finished",
            "--paused",
            "--approvals",
        ],
        "clean" => &["--failed", "--all"],
        "pause" | "resume" | "retry" | "rm" | "approve" | "deny" => &["--all"],
        "history" => &["--limit"],
        _ => &[],
    };
    let parsed = queue::parse(args, flags).map_err(|message| usage_error(spec, &message))?;
    match spec.name {
        "add" => add(engine, spec, &parsed, reply),
        "torrent" => {
            let [source] = parsed.words.as_slice() else {
                return Err(usage_error(
                    spec,
                    "give one magnet, HTTPS torrent link, or local .torrent file",
                ));
            };
            let destination = parsed.to.as_deref().unwrap_or("");
            let job =
                queue::create_torrent_job(engine, source, destination, Some(true), parsed.upload)?;
            reply.say(
                Tone::Good,
                format!(
                    "Added torrent {}  {}",
                    client::short_id(&job),
                    job.destination.as_deref().unwrap_or(destination)
                ),
            );
            reply.say(Tone::Dim, queue::torrent_note());
            Ok(())
        }
        "inspect" => {
            let [link] = parsed.words.as_slice() else {
                return Err(usage_error(spec, "Give one link to inspect"));
            };
            for line in queue::inspect_lines(engine, link)? {
                reply.say(Tone::Normal, line);
            }
            Ok(())
        }
        "status" => {
            for (warn, line) in queue::status_lines(engine)? {
                reply.say(if warn { Tone::Bad } else { Tone::Normal }, line);
            }
            Ok(())
        }
        "queue" => {
            let all = engine.jobs(JobFilter::All)?;
            let shown: Vec<&JobSnapshot> = all
                .iter()
                .filter(|job| queue::matches_ls_filter(job, &parsed))
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
        "clean" => {
            if !parsed.words.is_empty() {
                return Err(usage_error(spec, "clean takes no job names"));
            }
            let queue::CleanOutcome { removed, failed } =
                queue::clean_jobs(engine, parsed.failed || parsed.all)?;
            if removed.is_empty() && failed.is_empty() {
                reply.say(Tone::Normal, "Nothing to clean.");
            } else if !removed.is_empty() {
                let noun = if removed.len() == 1 {
                    "download"
                } else {
                    "downloads"
                };
                reply.say(
                    Tone::Good,
                    format!(
                        "Removed {} {noun} from the list (saved files kept).",
                        removed.len()
                    ),
                );
            }
            for (job, error) in failed {
                reply.say(
                    Tone::Bad,
                    format!(
                        "{} {}: {}",
                        client::short_id(&job),
                        client::name(&job),
                        error.message
                    ),
                );
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
        "folder" => {
            if parsed.words.is_empty() {
                return Err(usage_error(spec, "Name a download"));
            }
            for job in resolve(jobs, &parsed.words)? {
                match crate::reveal::show_job(&job) {
                    Ok(said) => reply.say(Tone::Normal, said),
                    Err(message) => reply.say(Tone::Bad, message),
                }
            }
            Ok(())
        }
        "pause" | "resume" | "cancel" | "retry" | "rm" | "approve" | "deny" => {
            let action = match spec.name {
                "pause" => Control::Pause,
                "resume" => Control::Resume,
                "cancel" => Control::Cancel,
                "retry" => Control::Retry,
                "approve" => Control::Approve,
                "deny" => Control::Deny,
                _ => Control::Remove,
            };
            let bulk = parsed.all
                || (parsed.words.len() == 1 && parsed.words[0].eq_ignore_ascii_case("all"));
            let targets = if bulk {
                if parsed.all && !parsed.words.is_empty() {
                    return Err(usage_error(
                        spec,
                        "Use either --all or specific downloads, not both",
                    ));
                }
                queue::bulk_targets(action, jobs).map_err(|message| usage_error(spec, &message))?
            } else {
                if parsed.words.is_empty() {
                    return Err(usage_error(spec, "Name at least one download"));
                }
                resolve(jobs, &parsed.words)?
            };
            if targets.is_empty() {
                reply.say(Tone::Normal, "No matching downloads.");
                return Ok(());
            }
            for job in targets {
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
        "setup" => match parsed
            .words
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .as_slice()
        {
            [] | ["check"] | ["status"] => {
                let report = crate::setup_cmd::inspect();
                for line in crate::setup_cmd::report_lines(&report) {
                    let tone = if line.contains("[missing]") || line.contains("[warn]") {
                        Tone::Bad
                    } else if line.contains("[ok]") {
                        Tone::Good
                    } else {
                        Tone::Normal
                    };
                    reply.say(tone, line);
                }
                Ok(())
            }
            ["media"] => {
                reply.set_up_tools = true;
                Ok(())
            }
            _ => Err(usage_error(spec, "Use /setup to check, or /setup media")),
        },
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
        "approvals" => {
            let waiting: Vec<&JobSnapshot> = jobs
                .iter()
                .filter(|job| job.state == JobState::AwaitingApproval)
                .collect();
            if waiting.is_empty() {
                reply.say(Tone::Normal, "No agent is waiting for an answer.");
            }
            for job in waiting {
                for line in super::flows::approval::lines(job, client::index_of(jobs, job)) {
                    reply.say(Tone::Normal, line);
                }
            }
            Ok(())
        }
        "rename" => {
            let [job, name] = parsed.words.as_slice() else {
                return Err(usage_error(spec, "Name a download and the new file name"));
            };
            let job = resolve(jobs, std::slice::from_ref(job))?.remove(0);
            reply.say(
                Tone::Normal,
                super::flows::conflict::rename(engine, &job, name)
                    .map_err(|message| client::input_error(&message))?,
            );
            Ok(())
        }
        "dashboard" => {
            reply.say(
                Tone::Normal,
                "The dashboard needs the full-screen view; plain mode lists the queue with /queue.",
            );
            Ok(())
        }
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
            if let Some(notice) = status.inbox_notice {
                reply.say(Tone::Bad, notice);
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
        // The terminal's own settings; the session runs these before a line
        // reaches the engine.
        "theme" | "keys" | "alias" | "forget" => Err(client::input_error(&format!(
            "/{} works only at the interactive prompt.",
            spec.name
        ))),
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
fn add(
    engine: &Engine,
    spec: &Spec,
    parsed: &queue::Args,
    reply: &mut Reply,
) -> Result<(), ProtocolError> {
    if parsed.words.is_empty() {
        return Err(usage_error(spec, "Give a link"));
    }
    if parsed.sha256.is_some() && parsed.words.len() > 1 {
        return Err(client::input_error(
            "A checksum describes one file; add links with --sha256 one at a time.",
        ));
    }
    if let Some(destination) = torrent_add_destination(spec, parsed)? {
        let source = &parsed.words[0];
        let job =
            queue::create_torrent_job(engine, source, destination, Some(true), parsed.upload)?;
        reply.say(
            Tone::Good,
            format!(
                "Added torrent {}  {}",
                client::short_id(&job),
                job.destination.as_deref().unwrap_or(destination)
            ),
        );
        reply.say(Tone::Dim, queue::torrent_note());
        return Ok(());
    }
    let at = parsed
        .at
        .as_deref()
        .map(|at| when::parse(at, Timestamp::now()))
        .transpose()
        .map_err(|message| client::input_error(&message))?;
    for link in &parsed.words {
        // A repository is queued file by file at once, pinned to one commit;
        // there is nothing to choose on a card.
        if queue::is_repository_link(link) {
            match queue::queue_repository(engine, link, parsed.to.as_deref(), at) {
                Ok((repository, folder, jobs)) => {
                    for line in queue::repository_lines(&repository, &folder, &jobs) {
                        reply.say(Tone::Normal, line);
                    }
                }
                Err(error) => reply.say(Tone::Bad, format!("{link}: {}", error.message)),
            }
            continue;
        }
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

fn torrent_add_destination<'a>(
    spec: &Spec,
    parsed: &'a queue::Args,
) -> Result<Option<&'a str>, ProtocolError> {
    if !parsed.words.iter().any(|word| queue::is_torrent_link(word)) {
        return Ok(None);
    }
    if parsed.words.len() != 1 {
        return Err(usage_error(spec, "add torrents one at a time"));
    }
    if parsed.sha256.is_some() || parsed.quality.is_some() || parsed.at.is_some() {
        return Err(usage_error(
            spec,
            "torrents cannot use --sha256, --quality, or --at",
        ));
    }
    Ok(Some(parsed.to.as_deref().unwrap_or("")))
}

fn help(topic: Option<&str>, reply: &mut Reply) {
    if let Some(topic) = topic {
        let clean = topic.trim_start_matches('/');
        match find(clean) {
            Some(spec) => {
                reply.say(Tone::Normal, spec.usage);
                reply.say(Tone::Normal, format!("  {}", spec.summary));
                if !spec.aliases.is_empty() {
                    let aliases: Vec<String> = spec
                        .aliases
                        .iter()
                        .map(|alias| format!("/{alias}"))
                        .collect();
                    reply.say(Tone::Dim, format!("  Aliases: {}", aliases.join(", ")));
                }
            }
            None => {
                let candidates = COMMANDS.iter().flat_map(|spec| {
                    std::iter::once(spec.name).chain(spec.aliases.iter().copied())
                });
                if let Some(suggestion) = crate::cli_help::suggest(clean, candidates) {
                    reply.say(
                        Tone::Bad,
                        format!("There is no /{clean} command. Did you mean /{suggestion}?"),
                    );
                } else {
                    reply.say(Tone::Bad, format!("There is no /{clean} command."));
                }
            }
        }
        return;
    }
    reply.say(
        Tone::Normal,
        "Paste or type a link to download it. Commands start with / (use /help COMMAND for details):",
    );
    for category in Category::ALL {
        reply.say(Tone::Dim, format!("\n{}:", category.title()));
        for spec in COMMANDS.iter().filter(|spec| spec.category == category) {
            reply.say(
                Tone::Normal,
                format!("  {:<12}{}", format!("/{}", spec.name), spec.summary),
            );
        }
    }
    reply.say(
        Tone::Normal,
        "\nJOB is a number from the panel or /queue, or the start of its id. Bulk commands also accept `all`.",
    );
    reply.say(
        Tone::Dim,
        "Keys: Tab completes, Up/Down recall, Esc clears, Ctrl+L refreshes, F2 opens the dashboard (Tab filter, / find, Space pause/resume), Ctrl+C on an empty line leaves.",
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
    /// The person's alias names, completed with the commands.
    pub aliases: &'a [String],
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
            .map(|spec| spec.name)
            .chain(context.aliases.iter().map(String::as_str))
            .filter(|name| name.starts_with(&typed))
            .map(|name| Candidate {
                insert: format!("/{name}"),
                label: format!("/{name}"),
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
            Takes::Links if word.starts_with("http") || word.contains("://") => {
                past_links(word, context.jobs)
            }
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

/// Links of earlier downloads that start with the typed text, as the engine
/// shows them: without user info, query or fragment.
fn past_links(word: &str, jobs: &[JobSnapshot]) -> Vec<Candidate> {
    let mut found: Vec<Candidate> = Vec::new();
    for job in jobs {
        let link = &job.source_display;
        if link.starts_with(word) && !found.iter().any(|seen| &seen.insert == link) {
            found.push(Candidate {
                insert: link.clone(),
                label: link.clone(),
                finished: true,
            });
        }
    }
    found
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
    fn torrent_add_discovers_peers_by_default_and_leaves_the_folder_to_the_engine() {
        let spec = find("add").unwrap();
        let without_consent = queue::parse(
            &[
                "magnet:?xt=urn:btih:abc".into(),
                "--to".into(),
                r"C:\Downloads\debian".into(),
            ],
            &["--to"],
        )
        .unwrap();
        assert_eq!(
            torrent_add_destination(spec, &without_consent).unwrap(),
            Some(r"C:\Downloads\debian")
        );
        assert_eq!(without_consent.discover_peers, None);
        let allowed = queue::parse(
            &[
                "https://example.test/debian.torrent".into(),
                "--to".into(),
                r"C:\Downloads\debian".into(),
                "--discover-peers".into(),
            ],
            &["--to", "--discover-peers"],
        )
        .unwrap();
        assert_eq!(
            torrent_add_destination(spec, &allowed).unwrap(),
            Some(r"C:\Downloads\debian")
        );
        let automatic = queue::parse(
            &[
                "https://example.test/debian.torrent".into(),
                "--discover-peers".into(),
            ],
            &["--to", "--discover-peers"],
        )
        .unwrap();
        assert_eq!(torrent_add_destination(spec, &automatic).unwrap(), Some(""));
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
            aliases: &[],
        };
        let inserts = |line: &str| -> Vec<String> {
            complete(line, &context)
                .candidates
                .into_iter()
                .map(|candidate| candidate.insert)
                .collect()
        };
        assert_eq!(inserts("/pa"), ["/pause"]);
        assert_eq!(inserts("/re"), ["/resume", "/retry", "/rename"]);
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
        // Past links, once each, as the engine shows them.
        assert_eq!(inserts("https://a.t"), ["https://a.test/x"]);
        assert_eq!(inserts("https://b."), Vec::<String>::new());
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

    #[test]
    fn new_slash_commands_aliases_and_grouped_help_work() {
        assert_eq!(parse("/doctor").unwrap().0.name, "setup");
        assert_eq!(parse("/st").unwrap().0.name, "status");
        assert_eq!(parse("/clear-done").unwrap().0.name, "clean");
        assert_eq!(
            parse("/inspect https://a.test/x").unwrap().0.name,
            "inspect"
        );
        assert_eq!(parse("/inbox").unwrap().0.name, "inbox");
        assert_eq!(parse("/web on").unwrap().0.name, "web");

        let mut reply = Reply::default();
        help(None, &mut reply);
        let text: String = reply
            .lines
            .iter()
            .map(|l| l.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("Downloads:"));
        assert!(text.contains("Queue & Control:"));
        assert!(text.contains("Agents & Inbox:"));
        assert!(text.contains("Setup & Terminal:"));
        assert!(text.contains("/setup"));
        assert!(text.contains("/status"));
        assert!(text.contains("/clean"));

        let mut topic_reply = Reply::default();
        help(Some("doctor"), &mut topic_reply);
        let topic_text: String = topic_reply
            .lines
            .iter()
            .map(|l| l.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(topic_text.contains("/setup"));
        assert!(topic_text.contains("/doctor"));

        let mut typo_reply = Reply::default();
        help(Some("stauts"), &mut typo_reply);
        assert!(typo_reply.lines[0].text.contains("Did you mean /status?"));
    }
}
