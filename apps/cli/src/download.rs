//! `fetchpath download`: the one command most people will use.
//!
//! It adds the download to the engine's shared queue and waits for it, so a
//! scripted download appears in the queue and history like any other
//! (FP-058). Output is for a person by default: progress on stderr while a
//! terminal is attached, then one plain result line. `--json` prints the
//! machine-readable record on stdout instead and suppresses progress. Exit
//! codes are stable and documented in `docs/user/CLI.md`.

use crate::client::{self, Engine};
use crate::wait::{self, OnInterrupt};
use fetchpath_protocol::command::{
    Command, ConflictPolicy, DestinationIntent, JobInput, JobRequest,
};
use fetchpath_protocol::message::CommandResult;
use fetchpath_protocol::model::JobState;
use fetchpath_protocol::{JobSnapshot, ProtocolError, SensitiveUrl};
use serde_json::json;
use std::path::{Path, PathBuf};

pub const EXIT_USAGE: i32 = 2;
pub const EXIT_CONFLICT: i32 = 3;
pub const EXIT_NETWORK: i32 = 4;
pub const EXIT_CHECKSUM: i32 = 5;
pub const EXIT_STORAGE: i32 = 6;
pub const EXIT_CANCELLED: i32 = 130;

struct Options {
    url: String,
    destination: Option<String>,
    sha256: Option<String>,
    json: bool,
    quiet: bool,
}

/// Returns the process exit code.
pub fn run(args: &[String]) -> i32 {
    let options = match parse(args) {
        Ok(options) => options,
        Err(message) => {
            eprintln!(
                "fetchpath: {message}
Run `fetchpath --help` for usage."
            );
            return EXIT_USAGE;
        }
    };
    let destination = match resolve_destination(&options.url, options.destination.as_deref()) {
        Ok(path) => path,
        Err(message) => {
            eprintln!("fetchpath: {message}");
            return EXIT_USAGE;
        }
    };
    if let Some(checksum) = &options.sha256
        && !is_sha256(checksum)
    {
        eprintln!("fetchpath: --sha256 needs a SHA-256 checksum: 64 characters, 0-9 and a-f.");
        return EXIT_USAGE;
    }
    let Ok(url) = SensitiveUrl::try_from(options.url.clone()) else {
        return report_failure(
            "input.invalid_url: the link is empty, too long or has control characters",
            options.json,
        );
    };

    client::catch_interrupt();
    let outcome = Engine::connect().and_then(|mut engine| {
        let created = engine.send(Command::CreateJob {
            request: JobRequest::File {
                input: JobInput::Url { url },
                destination: DestinationIntent {
                    path: destination.display().to_string(),
                    conflict: ConflictPolicy::Ask,
                },
                not_before: None,
                expected_sha256: options.sha256.clone(),
            },
        })?;
        let CommandResult::Job { job } = created else {
            return Err(client::unexpected(&created));
        };
        let live = !options.json && !options.quiet;
        wait::follow(&mut engine, job, live, OnInterrupt::Cancel).map(|waited| waited.job)
    });
    let done = match outcome {
        Ok(done) => done,
        Err(error) => return report_error(&error, options.json),
    };

    match done.state {
        JobState::Completed => {
            report_success(&done, options.sha256.is_some(), options.json, options.quiet);
            0
        }
        JobState::Cancelled => {
            if options.json {
                println!(
                    "{}",
                    json!({ "result": "cancelled", "job_id": done.job_id })
                );
            } else {
                eprintln!("Cancelled. Nothing was saved.");
            }
            EXIT_CANCELLED
        }
        _ => match &done.error {
            Some(error) => report_error(error, options.json),
            None => report_failure(
                &format!("internal.unknown: the download ended as {:?}", done.state),
                options.json,
            ),
        },
    }
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// A protocol error through the same wording and exit codes as before.
fn report_error(error: &ProtocolError, as_json: bool) -> i32 {
    let code = error.code.as_str();
    let detail = error
        .message
        .strip_prefix(code)
        .map(|rest| rest.trim_start_matches(':').trim_start())
        .unwrap_or(&error.message);
    report_failure(&format!("{code}: {detail}"), as_json)
}

fn parse(args: &[String]) -> Result<Options, String> {
    let mut positional = Vec::new();
    let mut sha256 = None;
    let mut json = false;
    let mut quiet = false;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--json" => json = true,
            "-q" | "--quiet" => quiet = true,
            "--sha256" => {
                sha256 = Some(
                    iter.next()
                        .ok_or("--sha256 needs a checksum after it")?
                        .clone(),
                )
            }
            flag if flag.starts_with("--sha256=") => {
                sha256 = Some(flag["--sha256=".len()..].to_owned())
            }
            flag if flag.starts_with('-') && flag.len() > 1 => {
                return Err(format!("unknown option {flag}"));
            }
            value => positional.push(value.to_owned()),
        }
    }
    let mut positional = positional.into_iter();
    let url = positional.next().ok_or("download needs a link")?;
    let destination = positional.next();
    if positional.next().is_some() {
        return Err("download takes a link and at most one destination".into());
    }
    Ok(Options {
        url,
        destination,
        sha256,
        json,
        quiet,
    })
}

/// A missing destination means the Downloads folder; a folder means "put it
/// in there under the name the link suggests". Anything else is a file path,
/// passed through unchanged so the engine's own checks apply to it.
fn resolve_destination(url: &str, destination: Option<&str>) -> Result<PathBuf, String> {
    let folder = match destination {
        None => downloads_dir()
            .ok_or("could not find your Downloads folder; give a destination explicitly")?,
        Some(value) if value.ends_with(['/', '\\']) || Path::new(value).is_dir() => {
            PathBuf::from(value)
        }
        Some(value) => return absolute(Path::new(value)),
    };
    absolute(&folder.join(file_name_from_url(url)))
}

/// The engine runs in its own folder, so a relative path is made full here,
/// against the folder the command was typed in.
pub fn absolute(path: &Path) -> Result<PathBuf, String> {
    std::path::absolute(path)
        .map_err(|error| format!("{} is not a usable path: {error}", path.display()))
}

fn downloads_dir() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .map(|home| PathBuf::from(home).join("Downloads"))
        .filter(|path| path.is_dir())
}

/// The last path segment of the link, decoded and made safe for Windows.
/// Falls back to `download` when the link names no file (for example `/`).
pub fn file_name_from_url(url: &str) -> String {
    let without_fragment = url.split('#').next().unwrap_or(url);
    let without_query = without_fragment
        .split('?')
        .next()
        .unwrap_or(without_fragment);
    let after_scheme = without_query
        .split_once("://")
        .map_or(without_query, |(_, rest)| rest);
    let segment = after_scheme
        .split_once('/')
        .map_or("", |(_, path)| path)
        .rsplit('/')
        .next()
        .unwrap_or("");
    safe_file_name(&percent_decode(segment)).unwrap_or_else(|| "download".into())
}

/// `text` made into a name Windows can create, or `None` when nothing
/// usable is left. Invisible direction and width marks are dropped, so a
/// name cannot display an extension other than its real one.
pub fn safe_file_name(text: &str) -> Option<String> {
    let cleaned: String = text
        .chars()
        .filter(|c| !is_invisible_format(*c))
        .map(|c| {
            if c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') {
                '_'
            } else {
                c
            }
        })
        .collect();
    let cleaned = cleaned.trim().trim_end_matches(['.', ' ']).to_owned();
    let stem = cleaned.split('.').next().unwrap_or("").to_ascii_uppercase();
    let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.as_bytes()[3].is_ascii_digit());
    if cleaned.is_empty() || reserved {
        return None;
    }
    Some(cleaned.chars().take(200).collect())
}

/// Bidirectional overrides, isolates and marks, zero-width characters and
/// the byte order mark: they change how a name reads, not what it is.
fn is_invisible_format(c: char) -> bool {
    matches!(
        c,
        '\u{061C}'
            | '\u{200B}'..='\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{2069}'
            | '\u{FEFF}'
    )
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        // Parsed from bytes so a multi-byte character after `%` cannot split
        // a UTF-8 boundary.
        let escaped = (bytes[i] == b'%' && i + 2 < bytes.len())
            .then(|| std::str::from_utf8(&bytes[i + 1..i + 3]).ok())
            .flatten()
            .and_then(|hex| u8::from_str_radix(hex, 16).ok());
        if let Some(byte) = escaped {
            out.push(byte);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn report_success(done: &JobSnapshot, checked: bool, as_json: bool, quiet: bool) {
    let destination = done.destination.clone().unwrap_or_default();
    let sha256 = done.observed_sha256.clone().unwrap_or_default();
    let received = done.progress.bytes_received;
    if as_json {
        println!(
            "{}",
            json!({
                "result": "downloaded_observed",
                "job_id": done.job_id,
                "destination": destination,
                "bytes": received,
                "observed_sha256": sha256,
                "checksum_matched": checked,
                // The engine removes staging bytes itself and reports only
                // that it has not finished; the old path is not known here.
                "staging_cleanup_pending": done.cleanup_pending.then_some("pending"),
            })
        );
        return;
    }
    println!("{destination}");
    if quiet {
        return;
    }
    eprintln!("Saved {} to {destination}", client::bytes(received));
    if checked {
        eprintln!("SHA-256 {sha256} matches the checksum you entered.");
    } else {
        eprintln!(
            "SHA-256 {sha256} (computed on this computer; compare it with the publisher's if they list one)"
        );
    }
}

/// Prints a failure for a person (or as JSON) and returns its exit code.
fn report_failure(error: &str, as_json: bool) -> i32 {
    let (code, detail) = error.split_once(": ").unwrap_or((error, ""));
    let (exit, advice) = match code.split('.').next().unwrap_or("") {
        "input" => (EXIT_USAGE, "Check the link and destination."),
        "storage" if code == "storage.destination_conflict" => (
            EXIT_CONFLICT,
            "Fetchpath never overwrites a file. Choose another name or folder.",
        ),
        "storage" => (
            EXIT_STORAGE,
            "Check the folder exists and you can write to it.",
        ),
        // The engine could not be reached or refused the request itself.
        "contract" => (
            client::EXIT_ENGINE,
            "Check that Fetchpath is installed correctly; `fetchpath engine status` shows the engine.",
        ),
        "integrity" | "verification" => (
            EXIT_CHECKSUM,
            "The file did not match the checksum and was not saved. Check you copied the right checksum, then try again.",
        ),
        _ if refused_by_server(detail) => (
            EXIT_NETWORK,
            "The website refused this link. Check it, or get a fresh one from the website.",
        ),
        _ => (
            EXIT_NETWORK,
            "The server or connection failed. Try again later.",
        ),
    };
    if as_json {
        println!(
            "{}",
            json!({ "result": "failed", "error_code": code, "detail": detail })
        );
    } else if detail.is_empty() {
        eprintln!("fetchpath: {code}\n{advice}");
    } else {
        eprintln!("fetchpath: {detail} ({code})\n{advice}");
    }
    exit
}

/// An HTTP client error that trying again will not change (404, 403…). 408
/// and 429 are the server asking for time.
fn refused_by_server(detail: &str) -> bool {
    detail
        .strip_prefix("HTTP status ")
        .and_then(|rest| rest.get(..3))
        .and_then(|code| code.parse::<u16>().ok())
        .is_some_and(|code| (400..500).contains(&code) && code != 408 && code != 429)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_name_comes_from_the_last_path_segment() {
        assert_eq!(
            file_name_from_url("https://a.test/dir/archive.zip?sig=x#y"),
            "archive.zip"
        );
        assert_eq!(
            file_name_from_url("https://a.test/My%20File.pdf"),
            "My File.pdf"
        );
        assert_eq!(file_name_from_url("https://a.test/%é.txt"), "%é.txt");
    }

    #[test]
    fn a_link_without_a_file_name_falls_back_safely() {
        assert_eq!(file_name_from_url("https://a.test/"), "download");
        assert_eq!(file_name_from_url("https://a.test"), "download");
        assert_eq!(file_name_from_url("https://a.test/con.txt"), "download");
    }

    #[test]
    fn unsafe_characters_never_reach_the_file_name() {
        assert_eq!(
            file_name_from_url("https://a.test/a%3Ab%2F..%5Cc"),
            "a_b_.._c"
        );
        assert_eq!(file_name_from_url("https://a.test/%2E%2E"), "download");
    }

    #[test]
    fn a_name_cannot_hide_its_extension() {
        // "invoice", RIGHT-TO-LEFT OVERRIDE, "fdp.exe" reads as "invoiceexe.pdf".
        assert_eq!(
            safe_file_name("invoice\u{202E}fdp.exe").as_deref(),
            Some("invoicefdp.exe")
        );
        assert_eq!(safe_file_name("\u{200B}\u{FEFF}"), None);
    }

    #[test]
    fn failures_map_to_distinct_exit_codes() {
        assert_eq!(report_failure("input.invalid_url: nope", true), EXIT_USAGE);
        assert_eq!(
            report_failure("storage.destination_conflict: x exists", true),
            EXIT_CONFLICT
        );
        assert_eq!(report_failure("storage.failed: denied", true), EXIT_STORAGE);
        assert_eq!(
            report_failure("source.transfer_failed: reset", true),
            EXIT_NETWORK
        );
    }

    #[test]
    fn options_parse_in_any_order() {
        let args: Vec<String> = ["--json", "https://a.test/x", "--sha256", "ab", "out.bin"]
            .map(String::from)
            .to_vec();
        let options = parse(&args).unwrap();
        assert!(options.json);
        assert_eq!(options.destination.as_deref(), Some("out.bin"));
        assert_eq!(options.sha256.as_deref(), Some("ab"));
        assert!(parse(&["--bogus".to_owned()]).is_err());
    }
}
