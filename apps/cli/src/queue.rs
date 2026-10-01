//! The queue commands, each a thin call to the engine (FP-058).
//!
//! `--json` prints protocol types exactly as the engine sends them: one
//! `CommandResult` per line (one per job for commands that take several),
//! events and progress as `ServerMessage`s, and a failure as `{"error": …}`.

use crate::client::{self, EXIT_ENGINE, Engine};
use crate::download::{self, EXIT_CANCELLED, EXIT_USAGE};
use crate::wait::{self, OnInterrupt};
use crate::when;
use fetchpath_protocol::command::{
    Command, ConflictPolicy, DestinationIntent, JobFilter, JobInput, JobRequest,
};
use fetchpath_protocol::message::{CommandResult, ControlOutcome, EventPayload, ServerMessage};
use fetchpath_protocol::model::{EngineSettings, LinkKind, MediaInspection, MediaVariantKind};
use fetchpath_protocol::{JobId, JobSnapshot, ProtocolError, SensitiveUrl, StreamItem, Timestamp};
use std::collections::HashMap;
use std::io::{BufRead, IsTerminal};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Flags and positional words, parsed the same way for every command.
pub(crate) struct Args {
    pub words: Vec<String>,
    json: bool,
    quiet: bool,
    all: bool,
    pub active: bool,
    pub failed: bool,
    wait: bool,
    pub to: Option<String>,
    pub sha256: Option<String>,
    pub quality: Option<String>,
    pub at: Option<String>,
    pub limit: Option<u32>,
    pub discover_peers: bool,
    pub upload: bool,
}

/// Which flags a command accepts; anything else is refused.
const VALUE_FLAGS: &[&str] = &["--to", "--sha256", "--quality", "--at", "--limit"];

pub(crate) fn parse(args: &[String], allowed: &[&str]) -> Result<Args, String> {
    let mut parsed = Args {
        words: Vec::new(),
        json: false,
        quiet: false,
        all: false,
        active: false,
        failed: false,
        wait: false,
        to: None,
        sha256: None,
        quality: None,
        at: None,
        limit: None,
        discover_peers: false,
        upload: false,
    };
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        let (flag, inline) = match arg.split_once('=') {
            Some((flag, value)) if flag.starts_with("--") => (flag, Some(value.to_owned())),
            _ => (arg.as_str(), None),
        };
        let flag = if flag == "-q" { "--quiet" } else { flag };
        if !flag.starts_with('-') || flag == "-" {
            parsed.words.push(arg.clone());
            continue;
        }
        if !allowed.contains(&flag) {
            return Err(format!("unknown option {flag}"));
        }
        if VALUE_FLAGS.contains(&flag) {
            let value = match inline {
                Some(value) => value,
                None => iter
                    .next()
                    .ok_or_else(|| format!("{flag} needs a value after it"))?
                    .clone(),
            };
            match flag {
                "--to" => parsed.to = Some(value),
                "--sha256" => parsed.sha256 = Some(value),
                "--quality" => parsed.quality = Some(value),
                "--at" => parsed.at = Some(value),
                _ => {
                    parsed.limit = Some(
                        value
                            .parse()
                            .map_err(|_| "--limit needs a whole number".to_owned())?,
                    )
                }
            }
            continue;
        }
        match flag {
            "--json" => parsed.json = true,
            "--quiet" => parsed.quiet = true,
            "--all" => parsed.all = true,
            "--active" => parsed.active = true,
            "--failed" => parsed.failed = true,
            "--wait" => parsed.wait = true,
            "--discover-peers" => parsed.discover_peers = true,
            "--upload" => parsed.upload = true,
            _ => unreachable!("every allowed flag is handled"),
        }
    }
    Ok(parsed)
}

fn usage(message: &str) -> i32 {
    eprintln!("fetchpath: {message}\nRun `fetchpath --help` for usage.");
    EXIT_USAGE
}

/// Parses, connects and runs `body`, turning an error into its exit code.
fn with_engine(
    args: &[String],
    allowed: &[&str],
    body: impl FnOnce(&mut Engine, &Args) -> Result<i32, ProtocolError>,
) -> i32 {
    let parsed = match parse(args, allowed) {
        Ok(parsed) => parsed,
        Err(message) => return usage(&message),
    };
    match Engine::connect().and_then(|mut engine| body(&mut engine, &parsed)) {
        Ok(code) => code,
        Err(error) => client::fail(&error, parsed.json),
    }
}

// ---------------------------------------------------------------- add

pub fn add(args: &[String]) -> i32 {
    let allowed = [
        "--to",
        "--sha256",
        "--quality",
        "--at",
        "--wait",
        "--json",
        "--quiet",
        "--discover-peers",
        "--upload",
    ];
    let parsed = match parse(args, &allowed) {
        Ok(parsed) => parsed,
        Err(message) => return usage(&message),
    };
    if parsed.words.is_empty() {
        return usage("add needs at least one link");
    }
    let links: Vec<(String, Option<String>)> = parsed
        .words
        .iter()
        .map(|link| (link.clone(), None))
        .collect();
    add_links(&parsed, &links, true)
}

/// Adds a torrent with an explicit peer-discovery decision. Upload remains off
/// unless the person asks for it in this command.
pub fn torrent(args: &[String]) -> i32 {
    let allowed = ["--to", "--discover-peers", "--upload", "--wait", "--json"];
    let parsed = match parse(args, &allowed) {
        Ok(parsed) => parsed,
        Err(message) => return usage(&message),
    };
    let [source] = parsed.words.as_slice() else {
        return usage("torrent needs one magnet, HTTPS .torrent link, or local .torrent file");
    };
    // Without --to the engine picks the folder: the default folder and rules
    // decide where, and the torrent's own name decides what.
    let destination = parsed.to.as_deref().unwrap_or("");
    if !parsed.discover_peers {
        return usage("torrent needs --discover-peers to contact the swarm");
    }
    let outcome = Engine::connect().and_then(|mut engine| {
        let job = create_torrent_job(
            &engine,
            source,
            destination,
            parsed.discover_peers,
            parsed.upload,
        )?;
        if parsed.json {
            client::print_json(&CommandResult::Job { job: job.clone() });
        } else {
            println!(
                "Added torrent {}  {}",
                client::short_id(&job),
                job.destination.as_deref().unwrap_or(destination)
            );
        }
        if parsed.wait {
            client::catch_interrupt();
            let waited = wait::follow(&mut engine, job, !parsed.json, OnInterrupt::Leave)?;
            Ok(client::job_exit_code(&waited.job))
        } else {
            Ok(0)
        }
    });
    match outcome {
        Ok(code) => code,
        Err(error) => client::fail(&error, parsed.json),
    }
}

pub(crate) fn create_torrent_job(
    engine: &Engine,
    source: &str,
    destination: &str,
    discover_peers: bool,
    upload: bool,
) -> Result<JobSnapshot, ProtocolError> {
    let input = if is_local_torrent_file(source) {
        let path = std::fs::canonicalize(source)
            .map_err(|_| client::input_error("That .torrent file could not be found."))?;
        JobInput::TorrentFile {
            path: path.display().to_string(),
        }
    } else {
        let url = SensitiveUrl::try_from(source.to_owned())
            .map_err(|_| client::input_error("That torrent link cannot be read."))?;
        JobInput::Url { url }
    };
    let created = engine.send(Command::CreateJob {
        request: JobRequest::Torrent {
            input,
            destination: DestinationIntent {
                path: destination.to_owned(),
                conflict: ConflictPolicy::Ask,
            },
            not_before: None,
            discover_peers,
            upload,
        },
    })?;
    let CommandResult::Job { job } = created else {
        return Err(client::unexpected(&created));
    };
    Ok(job)
}

/// `fetchpath batch FILE|-`: one link per line, optionally followed by a
/// destination; blank lines and lines starting with `#` are skipped.
pub fn batch(args: &[String]) -> i32 {
    let allowed = ["--to", "--at", "--wait", "--json", "--quiet"];
    let parsed = match parse(args, &allowed) {
        Ok(parsed) => parsed,
        Err(message) => return usage(&message),
    };
    let [source] = parsed.words.as_slice() else {
        return usage("batch needs one file of links, or - to read them from standard input");
    };
    let text = if source == "-" {
        let mut lines = Vec::new();
        for line in std::io::stdin().lock().lines() {
            match line {
                Ok(line) => lines.push(line),
                Err(error) => return usage(&format!("could not read standard input: {error}")),
            }
        }
        lines.join("\n")
    } else {
        match std::fs::read_to_string(source) {
            Ok(text) => text,
            Err(error) => return usage(&format!("could not read {source}: {error}")),
        }
    };
    let links: Vec<(String, Option<String>)> = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| match line.split_once(char::is_whitespace) {
            Some((link, destination)) => (link.to_owned(), Some(destination.trim().to_owned())),
            None => (line.to_owned(), None),
        })
        .collect();
    if links.is_empty() {
        return usage("the batch has no links");
    }
    // A batch does not look at each link first: rules by site and type
    // still apply, a rule by size cannot.
    add_links(&parsed, &links, false)
}

/// `look`: ask the engine what each link is first, so a video page is saved
/// as video and a rule by size can decide.
fn add_links(parsed: &Args, links: &[(String, Option<String>)], look: bool) -> i32 {
    if links.iter().any(|(link, _)| is_torrent_link(link)) {
        if links.len() != 1 {
            return usage("add torrents one at a time");
        }
        if !parsed.discover_peers {
            return usage("a torrent needs --discover-peers before contacting the swarm");
        }
        if parsed.sha256.is_some() || parsed.quality.is_some() || parsed.at.is_some() {
            return usage("torrent links cannot use --sha256, --quality, or --at");
        }
    }
    if parsed.sha256.is_some() && links.len() > 1 {
        return usage("a checksum describes one file; add links with --sha256 one at a time");
    }
    let not_before = match parsed
        .at
        .as_deref()
        .map(|at| when::parse(at, Timestamp::now()))
    {
        None => None,
        Some(Ok(at)) => Some(at),
        Some(Err(message)) => return usage(&message),
    };
    if parsed.wait {
        client::catch_interrupt();
    }
    let mut engine = match Engine::connect() {
        Ok(engine) => engine,
        Err(error) => return client::fail(&error, parsed.json),
    };
    let mut worst = 0;
    let mut created = Vec::new();
    for (link, own_destination) in links {
        let target = own_destination.as_deref().or(parsed.to.as_deref());
        if is_repository_link(link) {
            match add_repository(&engine, parsed, link, target, not_before) {
                Ok(jobs) => created.extend(jobs),
                Err(error) => {
                    if parsed.json {
                        client::print_json(&serde_json::json!({ "error": error }));
                    } else {
                        eprintln!("fetchpath: {link}: {}", error.message);
                    }
                    worst = first_failure(worst, client::exit_code(&error));
                }
            }
            continue;
        }
        let added = if is_torrent_link(link) {
            create_torrent_job(
                &engine,
                link,
                target.unwrap_or(""),
                parsed.discover_peers,
                parsed.upload,
            )
        } else {
            add_one(&engine, parsed, link, target, look, not_before)
        };
        match added {
            Ok(job) => {
                if parsed.json {
                    client::print_json(&CommandResult::Job { job: job.clone() });
                } else if parsed.quiet {
                    println!("{}", job.job_id);
                } else {
                    let when = job
                        .not_before
                        .map(|at| format!(", starting {}", when::local(at)))
                        .unwrap_or_default();
                    println!(
                        "Added {}  {}{when}",
                        client::short_id(&job),
                        job.destination.as_deref().unwrap_or(&job.source_display)
                    );
                }
                created.push(job);
            }
            Err(error) => {
                if parsed.json {
                    client::print_json(&serde_json::json!({ "error": error }));
                } else {
                    eprintln!("fetchpath: {link}: {}", error.message);
                }
                worst = first_failure(worst, client::exit_code(&error));
            }
        }
    }
    if !parsed.wait {
        return worst;
    }
    let live = created.len() == 1 && !parsed.json && !parsed.quiet;
    for job in created {
        match wait::follow(&mut engine, job, live, OnInterrupt::Leave) {
            Ok(waited) if waited.left => return EXIT_CANCELLED,
            Ok(waited) => {
                report_settled(&waited.job, parsed.json);
                worst = first_failure(worst, client::job_exit_code(&waited.job));
            }
            Err(error) => return client::fail(&error, parsed.json),
        }
    }
    worst
}

/// Recognize torrent input before inspecting it as an ordinary HTTP file.
pub(crate) fn is_torrent_link(source: &str) -> bool {
    if is_local_torrent_file(source) {
        return true;
    }
    if source.starts_with("magnet:?") {
        return true;
    }
    let Ok(url) = url::Url::parse(source) else {
        return false;
    };
    url.scheme() == "https" && url.path().to_ascii_lowercase().ends_with(".torrent")
}

fn is_local_torrent_file(source: &str) -> bool {
    if source.starts_with("https://")
        || source.starts_with("http://")
        || source.starts_with("magnet:")
    {
        return false;
    }
    let path = std::path::Path::new(source);
    path.extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("torrent"))
}

/// A Hugging Face link: the engine resolves it to one commit and its files.
pub(crate) fn is_repository_link(link: &str) -> bool {
    let link = link.trim();
    link.starts_with("hf://")
        || link.starts_with("https://huggingface.co/")
        || link.starts_with("https://www.huggingface.co/")
}

pub(crate) fn inspect_repository(
    engine: &Engine,
    link: &str,
) -> Result<fetchpath_protocol::model::RepositoryView, ProtocolError> {
    let url = SensitiveUrl::try_from(link.trim().to_owned())
        .map_err(|message| client::input_error(&format!("That link cannot be used: {message}.")))?;
    match engine.send(Command::InspectRepository { url })? {
        CommandResult::Repository { repository } => Ok(repository),
        other => Err(client::unexpected(&other)),
    }
}

/// Queues every file of a repository, pinned to one commit, in a folder
/// named after it, each large file checked against its stated SHA-256. One
/// job per file: each has its own checksum and subfolder, which a batch,
/// meant for links pasted together, does not allow.
pub(crate) fn queue_repository(
    engine: &Engine,
    link: &str,
    target: Option<&str>,
    not_before: Option<Timestamp>,
) -> Result<
    (
        fetchpath_protocol::model::RepositoryView,
        String,
        Vec<JobSnapshot>,
    ),
    ProtocolError,
> {
    let repository = inspect_repository(engine, link)?;
    let folder = match target {
        Some(folder) => PathBuf::from(folder),
        None => default_folder(engine)?
            .ok_or_else(|| client::input_error("choose a folder with --to"))?,
    };
    let folder = folder.display().to_string();
    let mut jobs = Vec::new();
    for mut request in repository.requests(&folder) {
        if let JobRequest::File { not_before: at, .. } = &mut request {
            *at = not_before;
        }
        match engine.send(Command::CreateJob { request })? {
            CommandResult::Job { job } => jobs.push(job),
            other => return Err(client::unexpected(&other)),
        }
    }
    Ok((repository, folder, jobs))
}

/// What `add` says about a queued repository, then each path left out.
pub(crate) fn repository_lines(
    repository: &fetchpath_protocol::model::RepositoryView,
    folder: &str,
    jobs: &[JobSnapshot],
) -> Vec<String> {
    let checked = repository
        .files
        .iter()
        .filter(|file| file.sha256.is_some())
        .count();
    let mut lines = vec![format!(
        "Added {} files of {} at commit {} ({}, {} checked against their SHA-256) into {}",
        jobs.len(),
        repository.repo,
        &repository.commit[..12.min(repository.commit.len())],
        fetchpath_protocol::describe::bytes(repository.total_bytes),
        checked,
        repository.folder_in(folder)
    )];
    for path in &repository.skipped {
        lines.push(format!(
            "Left out {path}: its name cannot be saved on Windows."
        ));
    }
    lines
}

fn add_repository(
    engine: &Engine,
    parsed: &Args,
    link: &str,
    target: Option<&str>,
    not_before: Option<Timestamp>,
) -> Result<Vec<JobSnapshot>, ProtocolError> {
    if parsed.sha256.is_some() {
        return Err(client::input_error(
            "a repository states each file's checksum itself; leave out --sha256",
        ));
    }
    let (repository, folder, jobs) = queue_repository(engine, link, target, not_before)?;
    if parsed.json {
        client::print_json(&CommandResult::Jobs { jobs: jobs.clone() });
    } else if parsed.quiet {
        for job in &jobs {
            println!("{}", job.job_id);
        }
    } else {
        for line in repository_lines(&repository, &folder, &jobs) {
            println!("{line}");
        }
    }
    Ok(jobs)
}

fn first_failure(current: i32, next: i32) -> i32 {
    if current == 0 { next } else { current }
}

fn report_settled(job: &JobSnapshot, json: bool) {
    if json {
        client::print_json(&CommandResult::Job { job: job.clone() });
        return;
    }
    let name = client::name(job);
    match (job.state, &job.error) {
        (fetchpath_protocol::model::JobState::Completed, _) => {
            println!("{}", job.destination.as_deref().unwrap_or(&name))
        }
        (_, Some(error)) => eprintln!("fetchpath: {name}: {}", error.message),
        (state, None) => eprintln!("fetchpath: {name} is {}", client::state_name(state)),
    }
}

/// Where a job goes without `--to`: the engine's default folder when one is
/// set, otherwise Downloads.
pub(crate) fn default_folder(engine: &Engine) -> Result<Option<PathBuf>, ProtocolError> {
    let view = match engine.send(Command::GetSettings)? {
        CommandResult::Settings { view } => view,
        other => return Err(client::unexpected(&other)),
    };
    Ok(view
        .settings
        .default_destination_dir
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("USERPROFILE").map(|home| PathBuf::from(home).join("Downloads"))
        }))
}

pub(crate) fn add_one(
    engine: &Engine,
    parsed: &Args,
    link: &str,
    target: Option<&str>,
    look: bool,
    not_before: Option<Timestamp>,
) -> Result<JobSnapshot, ProtocolError> {
    // A link that cannot be looked at is still added, as a file.
    let seen = if look {
        crate::rules::inspect(engine, link).ok()
    } else {
        None
    };
    add_named(
        engine,
        parsed,
        link,
        target,
        None,
        seen.as_ref(),
        not_before,
    )
}

/// `add_one` with a file name given instead of the one the link or page
/// suggests, and the link as already looked at (`None` treats it as a file
/// unless a quality is asked for). `name` must already be a safe file name;
/// with a folder as `target` it goes inside it, and without a target only the
/// name is sent.
pub(crate) fn add_named(
    engine: &Engine,
    parsed: &Args,
    link: &str,
    target: Option<&str>,
    name: Option<&str>,
    seen: Option<&fetchpath_protocol::model::LinkInspection>,
    not_before: Option<Timestamp>,
) -> Result<JobSnapshot, ProtocolError> {
    let url = SensitiveUrl::try_from(link.to_owned())
        .map_err(|message| client::input_error(&format!("That link cannot be used: {message}.")))?;
    // A video page is never saved as the page: the quality is the one asked
    // for, else a matching rule's, else the best up to 1080p.
    let quality = parsed.quality.clone().or_else(|| {
        let seen = seen.as_ref()?;
        (seen.kind == LinkKind::MediaPage).then(|| {
            seen.rules
                .as_ref()
                .and_then(|verdict| verdict.matched.as_ref()?.spec.then.media_quality.clone())
                .unwrap_or_else(|| "1080p".to_owned())
        })
    });
    let (request, suggested) = match &quality {
        None => (None, download::file_name_from_url(link)),
        Some(quality) => {
            let inspection = match engine.send(Command::InspectMedia { url: url.clone() })? {
                CommandResult::MediaInspection { inspection } => inspection,
                other => return Err(client::unexpected(&other)),
            };
            let variant = pick_variant(&inspection, quality)
                .map_err(|message| client::input_error(&message))?;
            let name = format!(
                "{}.{}",
                safe_title(&inspection.title, link),
                variant.extension
            );
            (Some((variant.id.clone(), variant.label.clone())), name)
        }
    };
    let suggested = name.map_or(suggested, str::to_owned);
    // Without --to only the name is sent, and the engine places it by rule
    // or in its default folder.
    let destination = match target {
        None => PathBuf::from(suggested),
        Some(_) => destination_path(target, None, &suggested)
            .map_err(|message| client::input_error(&message))?,
    };
    create_job(
        engine,
        url,
        &destination,
        not_before,
        parsed.sha256.clone(),
        request,
    )
}

/// Queues one job: a file, or with a chosen format (id and label) a video
/// or audio download. The engine asks before replacing an existing file.
pub(crate) fn create_job(
    engine: &Engine,
    url: SensitiveUrl,
    destination: &Path,
    not_before: Option<Timestamp>,
    sha256: Option<String>,
    media: Option<(String, String)>,
) -> Result<JobSnapshot, ProtocolError> {
    let destination = DestinationIntent {
        path: destination.display().to_string(),
        conflict: ConflictPolicy::Ask,
    };
    let input = JobInput::Url { url };
    let request = match media {
        None => JobRequest::File {
            input,
            destination,
            not_before,
            expected_sha256: sha256,
        },
        Some((variant_id, quality_label)) => {
            if sha256.is_some() {
                return Err(client::input_error(
                    "A checksum cannot be checked for a video or audio download.",
                ));
            }
            JobRequest::Media {
                input,
                destination,
                not_before,
                variant_id,
                quality_label,
            }
        }
    };
    match engine.send(Command::CreateJob { request })? {
        CommandResult::Job { job } => Ok(job),
        other => Err(client::unexpected(&other)),
    }
}

/// A folder (it exists, or ends in a slash) gets the suggested name; any
/// other target is the file itself.
pub(crate) fn destination_path(
    target: Option<&str>,
    default_folder: Option<&Path>,
    suggested: &str,
) -> Result<PathBuf, String> {
    let folder = match target {
        None => default_folder
            .map(Path::to_path_buf)
            .ok_or("could not find your Downloads folder; give one with --to")?,
        Some(value) if value.ends_with(['/', '\\']) || Path::new(value).is_dir() => {
            PathBuf::from(value)
        }
        Some(value) => return download::absolute(Path::new(value)),
    };
    download::absolute(&folder.join(suggested))
}

/// `best` is the tallest video, `audio` the first audio-only format;
/// anything else must equal a format's label or id, ignoring case.
pub(crate) fn pick_variant<'a>(
    inspection: &'a MediaInspection,
    quality: &str,
) -> Result<&'a fetchpath_protocol::model::MediaVariant, String> {
    let variants = &inspection.variants;
    let found = match quality.to_ascii_lowercase().as_str() {
        "best" => variants
            .iter()
            .filter(|variant| variant.kind == MediaVariantKind::Video)
            .max_by_key(|variant| (variant.height.unwrap_or(0), variant.fps.unwrap_or(0))),
        "audio" => variants
            .iter()
            .find(|variant| variant.kind == MediaVariantKind::Audio),
        wanted => variants
            .iter()
            .find(|variant| {
                variant.label.to_ascii_lowercase() == wanted
                    || variant.id.to_ascii_lowercase() == wanted
            })
            .or_else(|| {
                // A height such as 720p also accepts the tallest video
                // below it.
                let limit: u32 = wanted.strip_suffix('p')?.parse().ok()?;
                variants
                    .iter()
                    .filter(|variant| {
                        variant.kind == MediaVariantKind::Video
                            && variant.height.is_some_and(|height| height <= limit)
                    })
                    .max_by_key(|variant| (variant.height, variant.fps.unwrap_or(0)))
            }),
    };
    found.ok_or_else(|| {
        let labels: Vec<&str> = variants
            .iter()
            .map(|variant| variant.label.as_str())
            .collect();
        format!(
            "This page has no {quality:?} format. Choose best, audio, or one of: {}.",
            labels.join(", ")
        )
    })
}

/// The page title, which is untrusted text, reduced to a safe file name.
pub(crate) fn safe_title(title: &str, link: &str) -> String {
    match download::safe_file_name(title) {
        Some(name) => name,
        None => download::file_name_from_url(link),
    }
}

// ---------------------------------------------------------------- ls, history, show

pub fn ls(args: &[String]) -> i32 {
    with_engine(
        args,
        &["--all", "--active", "--failed", "--json"],
        |engine, parsed| {
            let jobs = engine.jobs(JobFilter::All)?;
            let shown: Vec<&JobSnapshot> = jobs
                .iter()
                .filter(|job| {
                    if parsed.failed {
                        job.state == fetchpath_protocol::model::JobState::Failed
                    } else if parsed.active {
                        !job.state.is_terminal()
                            && job.state != fetchpath_protocol::model::JobState::Failed
                    } else {
                        true
                    }
                })
                .collect();
            if parsed.json {
                client::print_json(&CommandResult::Jobs {
                    jobs: shown.into_iter().cloned().collect(),
                });
                return Ok(0);
            }
            // A queue from a newer Fetchpath is listed, but say why nothing moves.
            if let Ok(CommandResult::EngineStatus { status }) = engine.send(Command::EngineStatus)
                && let Some(reason) = status.queue_read_only
            {
                eprintln!("{}", reason.message);
            }
            if shown.is_empty() {
                println!("No downloads.");
                return Ok(0);
            }
            print_table(&jobs, &shown);
            Ok(0)
        },
    )
}

pub fn history(args: &[String]) -> i32 {
    with_engine(args, &["--limit", "--json"], |engine, parsed| {
        let query = (!parsed.words.is_empty()).then(|| parsed.words.join(" "));
        let jobs = match engine.send(Command::History {
            query,
            limit: parsed.limit,
        })? {
            CommandResult::Jobs { jobs } => jobs,
            other => return Err(client::unexpected(&other)),
        };
        if parsed.json {
            client::print_json(&CommandResult::Jobs { jobs });
            return Ok(0);
        }
        if jobs.is_empty() {
            println!("No finished downloads match.");
            return Ok(0);
        }
        let all = engine.jobs(JobFilter::All)?;
        print_table(&all, &jobs.iter().collect::<Vec<_>>());
        Ok(0)
    })
}

fn print_table(all: &[JobSnapshot], shown: &[&JobSnapshot]) {
    for line in table_lines(all, shown) {
        println!("{line}");
    }
}

/// The `ls` table: a header, then one row per shown job with its index in
/// `all`.
pub(crate) fn table_lines(all: &[JobSnapshot], shown: &[&JobSnapshot]) -> Vec<String> {
    let mut lines = vec![format!(
        "{:>4}  {:<8}  {:<14}  {:<22}  NAME",
        "#", "ID", "STATE", "PROGRESS"
    )];
    for job in shown {
        let index = client::index_of(all, job).map_or(String::new(), |index| index.to_string());
        let mut amount = client::amount(&job.progress);
        if job.state == fetchpath_protocol::model::JobState::Running
            && let Some(rate) = job.progress.rate_bytes_per_second
        {
            amount = format!("{amount}  {}/s", client::bytes(rate));
        }
        lines.push(format!(
            "{index:>4}  {:<8}  {:<14}  {amount:<22}  {}",
            client::short_id(job),
            client::state_label(job),
            client::name(job)
        ));
    }
    lines
}

pub fn show(args: &[String]) -> i32 {
    with_engine(args, &["--json"], |engine, parsed| {
        if parsed.words.is_empty() {
            return Ok(usage(
                "show needs a download: its number in `fetchpath ls`, or the start of its id",
            ));
        }
        let jobs = engine.resolve(&parsed.words)?;
        for job in jobs {
            if parsed.json {
                client::print_json(&CommandResult::Job { job });
                continue;
            }
            print_job(&job);
        }
        Ok(0)
    })
}

fn print_job(job: &JobSnapshot) {
    for line in job_lines(job) {
        println!("{line}");
    }
    println!();
}

/// `show`'s description of one job, one field per line.
pub(crate) fn job_lines(job: &JobSnapshot) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = |key: &str, value: &str| lines.push(format!("{key:<12} {value}"));
    line("Id", job.job_id.as_str());
    line("State", client::state_label(job));
    line("Link", &job.source_display);
    if let Some(destination) = &job.destination {
        line("Saved as", destination);
    }
    let amount = client::amount(&job.progress);
    if !amount.is_empty() {
        line("Progress", &amount);
    }
    if let Some(label) = &job.quality_label {
        line("Quality", label);
    }
    line("Added", &when::local(job.created_at));
    if let Some(at) = job.not_before {
        line("Starts", &when::local(at));
    }
    if let Some(at) = job.finished_at {
        line("Finished", &when::local(at));
    }
    if let Some(expected) = &job.expected_sha256 {
        line("Expected", &format!("SHA-256 {expected}"));
    }
    if let Some(fingerprint) = &job.from_paired_device {
        line(
            "Source",
            &format!("your paired computer {fingerprint}, checked against the checksum"),
        );
    }
    if job.reused_from_cache {
        line(
            "Source",
            "this computer's cache, checked again; nothing was transferred",
        );
    }
    if let Some(observed) = &job.observed_sha256 {
        line(
            "Received",
            &format!("SHA-256 {observed} (computed on this computer)"),
        );
    }
    if let Some(error) = &job.error {
        line("Problem", &format!("{} ({})", error.message, error.code));
    }
    if let Some(at) = job.retry_at {
        line(
            "Retrying",
            &format!("{} (attempt {})", when::local(at), job.attempt + 1),
        );
    }
    lines
}

// ---------------------------------------------------------------- controls

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Control {
    Pause,
    Resume,
    Cancel,
    Retry,
    Remove,
    /// Lets an agent's request that waits for the person run.
    Approve,
    /// Refuses an agent's request; it ends cancelled.
    Deny,
}

pub fn control(action: Control, args: &[String]) -> i32 {
    with_engine(args, &["--json"], |engine, parsed| {
        if parsed.words.is_empty() {
            return Ok(usage(
                "name at least one download: its number in `fetchpath ls`, or the start of its id",
            ));
        }
        let jobs = engine.resolve(&parsed.words)?;
        let mut worst = 0;
        for job in jobs {
            match engine.send(control_command(action, &job)) {
                Ok(result) => {
                    if parsed.json {
                        client::print_json(&result);
                    } else {
                        println!("{}", describe(action, &job, &result));
                    }
                }
                Err(error) => {
                    if parsed.json {
                        client::print_json(&serde_json::json!({ "error": error }));
                    } else {
                        eprintln!(
                            "fetchpath: {} {}: {}",
                            client::short_id(&job),
                            client::name(&job),
                            error.message
                        );
                    }
                    worst = first_failure(worst, client::exit_code(&error));
                }
            }
        }
        Ok(worst)
    })
}

pub(crate) fn control_command(action: Control, job: &JobSnapshot) -> Command {
    let job_id = job.job_id.clone();
    match action {
        Control::Pause => Command::Pause { job_id },
        Control::Resume => Command::Resume { job_id },
        Control::Cancel => Command::Cancel {
            job_id,
            retain_partial: false,
        },
        Control::Retry => Command::Retry {
            job_id,
            expected_sha256: None,
        },
        Control::Remove => Command::RemoveJob { job_id },
        Control::Approve => Command::ApproveJob { job_id },
        Control::Deny => Command::DenyJob { job_id },
    }
}

pub(crate) fn describe(action: Control, job: &JobSnapshot, result: &CommandResult) -> String {
    let who = format!("{} {}", client::short_id(job), client::name(job));
    let outcome = match result {
        CommandResult::Control { outcome, .. } => Some(*outcome),
        _ => None,
    };
    match (action, outcome) {
        (_, Some(ControlOutcome::AlreadyTerminal)) => format!("{who} has already ended."),
        (_, Some(ControlOutcome::NoOp)) => format!("{who}: nothing to do."),
        (_, Some(ControlOutcome::TooLate)) => format!("{who} finished before it could pause."),
        (_, Some(ControlOutcome::TooLateToCancel)) => {
            format!("{who} was already being saved and was not cancelled.")
        }
        (_, Some(ControlOutcome::CancelInProgress)) => format!("{who} is already cancelling."),
        (Control::Pause, _) => format!("Pausing {who}."),
        (Control::Resume, _) => format!("Resumed {who}."),
        (Control::Cancel, _) => format!("Cancelling {who}."),
        (Control::Retry, _) => format!("Retrying {who}."),
        (Control::Remove, _) => format!("Removed {who} from the list."),
        (Control::Approve, _) => format!("Approved {who}."),
        (Control::Deny, _) => format!("Denied {who}; it will not download."),
    }
}

// ---------------------------------------------------------------- watch

pub fn watch(args: &[String]) -> i32 {
    client::catch_interrupt();
    with_engine(args, &["--json"], |engine, parsed| {
        match parsed.words.as_slice() {
            [] => watch_queue(engine, parsed.json),
            [reference] => {
                let job = engine.resolve(std::slice::from_ref(reference))?.remove(0);
                if parsed.json {
                    return watch_job_json(engine, job);
                }
                let waited = wait::follow(engine, job, true, OnInterrupt::Leave)?;
                if waited.left {
                    return Ok(EXIT_CANCELLED);
                }
                report_settled(&waited.job, false);
                Ok(client::job_exit_code(&waited.job))
            }
            _ => Ok(usage("watch follows the whole queue, or one download")),
        }
    })
}

fn watch_job_json(engine: &Engine, job: JobSnapshot) -> Result<i32, ProtocolError> {
    let subscription = engine.subscribe(Command::SubscribeJob {
        job_id: job.job_id.clone(),
        after_seq: job.last_seq,
    })?;
    client::print_json(&subscription.start);
    let mut events = subscription.events;
    let mut current = job;
    loop {
        if client::interrupted() {
            return Ok(EXIT_CANCELLED);
        }
        if client::settled(&current) {
            return Ok(client::job_exit_code(&current));
        }
        match events.next_item(Duration::from_millis(500))? {
            Some(StreamItem::Event(event)) => {
                client::print_json(&ServerMessage::Event(event));
                current = engine.job(&current.job_id)?;
            }
            Some(StreamItem::Progress(sample)) => {
                client::print_json(&ServerMessage::Progress(sample));
            }
            None => current = engine.job(&current.job_id)?,
        }
    }
}

/// Follows every job from now until Ctrl+C: one line per durable event, or
/// with `--json` every event and progress sample.
fn watch_queue(engine: &Engine, json: bool) -> Result<i32, ProtocolError> {
    let cursor = match engine.send(Command::EngineStatus)? {
        CommandResult::EngineStatus { status } => status.queue_cursor,
        other => return Err(client::unexpected(&other)),
    };
    let subscription = engine.subscribe(Command::SubscribeQueue {
        after_cursor: cursor,
    })?;
    if json {
        client::print_json(&subscription.start);
    } else if std::io::stderr().is_terminal() {
        eprintln!("Following the queue. Press Ctrl+C to stop.");
    }
    let mut names: HashMap<JobId, String> = engine
        .jobs(JobFilter::All)?
        .into_iter()
        .map(|job| (job.job_id.clone(), client::name(&job)))
        .collect();
    let mut events = subscription.events;
    while !client::interrupted() {
        match events.next_item(Duration::from_millis(500))? {
            None => {}
            Some(StreamItem::Progress(sample)) => {
                if json {
                    client::print_json(&ServerMessage::Progress(sample));
                }
            }
            Some(StreamItem::Event(event)) => {
                if json {
                    client::print_json(&ServerMessage::Event(event));
                    continue;
                }
                if let EventPayload::JobCreated { job } = &event.payload {
                    names.insert(job.job_id.clone(), client::name(job));
                }
                let name = names.get(&event.job_id).cloned().unwrap_or_default();
                if let Some(text) = event_text(&event.payload) {
                    println!(
                        "{}  {}  {text}  {name}",
                        when::local(event.occurred_at),
                        &event.job_id.as_str()[..8]
                    );
                }
            }
        }
    }
    Ok(EXIT_CANCELLED)
}

pub(crate) fn event_text(payload: &EventPayload) -> Option<String> {
    Some(match payload {
        EventPayload::JobCreated { .. } => "added".into(),
        EventPayload::StateChanged { state, .. } => client::state_name(*state).into(),
        EventPayload::PolicyChanged {
            not_before: Some(at),
        } => format!("scheduled for {}", when::local(*at)),
        EventPayload::PolicyChanged { not_before: None } => "start now".into(),
        EventPayload::SourceChanged { .. } => "new link".into(),
        EventPayload::ErrorRecorded { error } => format!("problem: {}", error.message),
        EventPayload::PublicationCompleted { .. } => "saved".into(),
        EventPayload::JobRemoved => "removed".into(),
        EventPayload::Warning { message, .. } => format!("warning: {message}"),
        _ => return None,
    })
}

// ---------------------------------------------------------------- inspect

pub fn inspect(args: &[String]) -> i32 {
    with_engine(args, &["--json"], |engine, parsed| {
        let [link] = parsed.words.as_slice() else {
            return Ok(usage("inspect needs one link to a video or audio page"));
        };
        let url = SensitiveUrl::try_from(link.clone()).map_err(|message| {
            client::input_error(&format!("That link cannot be used: {message}."))
        })?;
        let result = engine.send(Command::InspectMedia { url })?;
        if parsed.json {
            client::print_json(&result);
            return Ok(0);
        }
        let CommandResult::MediaInspection { inspection } = result else {
            return Err(client::unexpected(&result));
        };
        println!(
            "{}",
            inspection
                .title
                .chars()
                .filter(|c| !c.is_control())
                .collect::<String>()
        );
        if let Some(seconds) = inspection.duration_seconds {
            println!("Length {}", client::duration(seconds as u64));
        }
        for variant in &inspection.variants {
            let kind = match variant.kind {
                MediaVariantKind::Video => "video",
                MediaVariantKind::Audio => "audio",
                MediaVariantKind::Unknown => "other",
            };
            println!("  {:<24} {kind:<6} .{}", variant.label, variant.extension);
        }
        println!("Add one with: fetchpath add LINK --quality LABEL (or best, audio)");
        Ok(0)
    })
}

// ---------------------------------------------------------------- settings

pub fn settings(args: &[String]) -> i32 {
    with_engine(args, &["--json"], |engine, parsed| {
        if parsed.words.len() > 2 {
            return Ok(usage("settings takes at most a name and a value"));
        }
        let (json, lines) = settings_outcome(engine, &parsed.words)?;
        if parsed.json {
            client::print_json(&json);
        } else {
            for line in lines {
                println!("{line}");
            }
        }
        Ok(0)
    })
}

/// Shows every setting, one, or changes one: the engine's reply for
/// `--json`, and the lines a person reads.
pub(crate) fn settings_outcome(
    engine: &Engine,
    words: &[String],
) -> Result<(serde_json::Value, Vec<String>), ProtocolError> {
    let view = match engine.send(Command::GetSettings)? {
        CommandResult::Settings { view } => view,
        other => return Err(client::unexpected(&other)),
    };
    let current = settings_map(&view.settings);
    let json =
        |result: &CommandResult| serde_json::to_value(result).expect("protocol types serialize");
    match words {
        [] => Ok((
            json(&CommandResult::Settings { view }),
            current
                .iter()
                .map(|(key, value)| format!("{} = {}", key.replace('_', "-"), plain(value)))
                .collect(),
        )),
        [key] => {
            let key = setting_key(key, &current)?;
            Ok((
                serde_json::json!({ key.clone(): current[&key] }),
                vec![plain(&current[&key])],
            ))
        }
        [key, value] => {
            let key = setting_key(key, &current)?;
            let mut next = current.clone();
            next.insert(key.clone(), setting_value(&key, &current[&key], value)?);
            let settings: EngineSettings = serde_json::from_value(serde_json::Value::Object(next))
                .map_err(|error| {
                    client::input_error(&format!("{value:?} does not suit {key}: {error}"))
                })?;
            let result = engine.send(Command::UpdateSettings { settings })?;
            let CommandResult::Settings { view } = &result else {
                return Err(client::unexpected(&result));
            };
            // The engine clamps values into range; show what it kept.
            let applied = settings_map(&view.settings);
            let line = format!("{} = {}", key.replace('_', "-"), plain(&applied[&key]));
            Ok((json(&result), vec![line]))
        }
        _ => Err(client::input_error(
            "settings takes at most a name and a value",
        )),
    }
}

/// Every setting's name as the command line spells it, for completion.
pub(crate) fn setting_names(engine: &Engine) -> Result<Vec<String>, ProtocolError> {
    match engine.send(Command::GetSettings)? {
        CommandResult::Settings { view } => Ok(settings_map(&view.settings)
            .keys()
            .map(|key| key.replace('_', "-"))
            .collect()),
        other => Err(client::unexpected(&other)),
    }
}

fn settings_map(settings: &EngineSettings) -> serde_json::Map<String, serde_json::Value> {
    let mut map = match serde_json::to_value(settings).expect("settings serialize") {
        serde_json::Value::Object(map) => map,
        _ => unreachable!("settings are an object"),
    };
    // Absent means off for this one; show it like the others.
    map.entry("start_engine_at_sign_in")
        .or_insert(serde_json::Value::Bool(false));
    map
}

fn setting_key(
    key: &str,
    current: &serde_json::Map<String, serde_json::Value>,
) -> Result<String, ProtocolError> {
    let key = key.replace('-', "_").to_ascii_lowercase();
    if current.contains_key(&key) {
        Ok(key)
    } else {
        let names: Vec<String> = current.keys().map(|key| key.replace('_', "-")).collect();
        Err(client::input_error(&format!(
            "There is no setting {key}. Settings: {}.",
            names.join(", ")
        )))
    }
}

/// A typed value from what the person wrote, shaped like the current one.
fn setting_value(
    key: &str,
    current: &serde_json::Value,
    text: &str,
) -> Result<serde_json::Value, ProtocolError> {
    use serde_json::Value;
    let wrong = |kind: &str| client::input_error(&format!("{key} needs {kind}."));
    Ok(match current {
        Value::Bool(_) => match text.to_ascii_lowercase().as_str() {
            "true" | "on" | "yes" | "1" => Value::Bool(true),
            "false" | "off" | "no" | "0" => Value::Bool(false),
            _ => return Err(wrong("on or off")),
        },
        Value::Number(_) => Value::Number(
            text.parse::<u64>()
                .map_err(|_| wrong("a whole number"))?
                .into(),
        ),
        // Optional folders: "none" clears them.
        _ if key.ends_with("_dir") => {
            if text.is_empty() || text.eq_ignore_ascii_case("none") {
                Value::Null
            } else {
                Value::String(
                    download::absolute(Path::new(text))
                        .map_err(|message| client::input_error(&message))?
                        .display()
                        .to_string(),
                )
            }
        }
        _ => Value::String(text.to_ascii_lowercase()),
    })
}

fn plain(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Null => "none".into(),
        serde_json::Value::Bool(true) => "on".into(),
        serde_json::Value::Bool(false) => "off".into(),
        serde_json::Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

/// `fetchpath engine status` for a person, or its protocol result.
pub fn engine_status(json: bool) -> i32 {
    let outcome = Engine::attach().and_then(|engine| engine.send(Command::EngineStatus));
    match outcome {
        Ok(result) if json => {
            client::print_json(&result);
            0
        }
        Ok(CommandResult::EngineStatus { status }) => {
            println!(
                "The Fetchpath engine {} is running, started {}.",
                status.engine_version,
                when::local(status.started_at)
            );
            // Its client count includes listeners that have gone but not
            // yet been noticed, so it is left to --json.
            println!(
                "{} active download{}.",
                status.active_jobs,
                if status.active_jobs == 1 { "" } else { "s" },
            );
            if let Some(reason) = &status.queue_read_only {
                println!("{}", reason.message);
            }
            0
        }
        Ok(other) => client::fail(&client::unexpected(&other), false),
        Err(error) if error.code.as_str() == "contract.engine_unavailable" => {
            if json {
                client::print_json(&serde_json::json!({ "error": error }));
            } else {
                println!("The Fetchpath engine is not running.");
            }
            EXIT_ENGINE
        }
        Err(error) => client::fail(&error, json),
    }
}

#[cfg(test)]
mod torrent_intake_tests {
    use super::*;

    #[test]
    fn recognizes_only_magnets_and_https_torrent_paths() {
        for source in [
            "magnet:?xt=urn:btih:abc",
            "https://example.test/debian.TORRENT",
            "https://example.test/debian.torrent?mirror=1",
        ] {
            assert!(is_torrent_link(source), "{source}");
        }
        for source in [
            "http://example.test/debian.torrent",
            "https://example.test/debian.torrent.zip",
            "https://example.test/file.iso?name=debian.torrent",
            "https://example.test/file.iso",
        ] {
            assert!(!is_torrent_link(source), "{source}");
        }
        assert!(is_torrent_link(r"C:\Downloads\debian.torrent"));
        assert!(is_torrent_link("debian.torrent"));
    }

    #[test]
    fn add_requires_explicit_peer_discovery_for_detected_torrent() {
        assert_eq!(
            add(&[
                "magnet:?xt=urn:btih:abc".into(),
                "--to".into(),
                "C:\\Downloads\\debian".into(),
            ]),
            EXIT_USAGE
        );
    }
}
