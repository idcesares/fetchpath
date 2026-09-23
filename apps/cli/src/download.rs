//! `fetchpath download`: the one command most people will use.
//!
//! Output is for a person by default: progress on stderr while a terminal is
//! attached, then one plain result line. `--json` prints the machine-readable
//! record on stdout instead and suppresses progress. Exit codes are stable and
//! documented in `docs/user/CLI.md`.

use fetchpath_core::{FileJob, FileJobSnapshot, FileJobState};
use serde_json::json;
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

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
            eprintln!("fetchpath: {message}\nRun `fetchpath --help` for usage.");
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

    let mut job = FileJob::create(options.url.clone(), destination);
    if let Some(checksum) = &options.sha256 {
        job = match job.with_expected_sha256(checksum) {
            Ok(job) => job,
            Err(_) => {
                eprintln!(
                    "fetchpath: --sha256 needs a SHA-256 checksum: 64 characters, 0-9 and a-f."
                );
                return EXIT_USAGE;
            }
        };
    }
    let cancel = job.clone();
    // A second handler cannot be installed; failing to install one only means
    // Ctrl-C ends the process without the orderly cancellation path.
    let _ = ctrlc::set_handler(move || {
        cancel.cancel();
    });
    if let Err(error) = job.start() {
        return report_failure(error, options.json);
    }

    let show_progress = !options.json && !options.quiet && std::io::stderr().is_terminal();
    let started = Instant::now();
    if show_progress {
        let mut last_line_len: usize = 0;
        loop {
            let snapshot = job.snapshot();
            if is_terminal_state(snapshot.state) {
                break;
            }
            let line = progress_line(&snapshot, started.elapsed());
            let padding = last_line_len.saturating_sub(line.chars().count());
            eprint!("\r{line}{}", " ".repeat(padding));
            let _ = std::io::stderr().flush();
            last_line_len = line.chars().count();
            std::thread::sleep(Duration::from_millis(200));
        }
        eprint!("\r{}\r", " ".repeat(last_line_len));
    }
    job.join();
    let done = job.snapshot();

    match done.state {
        FileJobState::Completed => {
            report_success(&done, options.sha256.is_some(), options.json, options.quiet);
            0
        }
        FileJobState::Cancelled => {
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
        _ => report_failure(
            &done
                .error
                .unwrap_or_else(|| format!("job ended as {:?}", done.state)),
            options.json,
        ),
    }
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
        Some(value) => return Ok(PathBuf::from(value)),
    };
    Ok(folder.join(file_name_from_url(url)))
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
    let decoded = percent_decode(segment);
    let cleaned: String = decoded
        .chars()
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
        return "download".into();
    }
    cleaned.chars().take(200).collect()
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

fn is_terminal_state(state: FileJobState) -> bool {
    matches!(
        state,
        FileJobState::Completed | FileJobState::Cancelled | FileJobState::Failed
    )
}

fn progress_line(snapshot: &FileJobSnapshot, elapsed: Duration) -> String {
    let received = snapshot.bytes_received;
    let seconds = elapsed.as_secs_f64();
    let rate = if seconds > 0.5 {
        Some(received as f64 / seconds)
    } else {
        None
    };
    let rate_text = rate.map_or(String::new(), |r| format!("  {}/s", bytes(r as u64)));
    match snapshot.total_bytes {
        // Percent and remaining time only from a length the source stated.
        Some(total) if total > 0 => {
            let percent = (received as f64 / total as f64 * 100.0).min(100.0);
            let eta = rate.filter(|r| *r > 0.0).map_or(String::new(), |r| {
                format!(
                    "  {} left",
                    duration(((total - received.min(total)) as f64 / r) as u64)
                )
            });
            format!(
                "{percent:5.1}%  {} of {}{rate_text}{eta}",
                bytes(received),
                bytes(total)
            )
        }
        _ => format!("{} received{rate_text}", bytes(received)),
    }
}

fn report_success(done: &FileJobSnapshot, checked: bool, as_json: bool, quiet: bool) {
    let destination = done
        .destination
        .as_ref()
        .map(|path| path.display().to_string())
        .unwrap_or_default();
    let sha256 = done.observed_sha256.clone().unwrap_or_default();
    if as_json {
        println!(
            "{}",
            json!({
                "result": "downloaded_observed",
                "job_id": done.job_id,
                "destination": destination,
                "bytes": done.bytes_received,
                "observed_sha256": sha256,
                "checksum_matched": checked,
                "staging_cleanup_pending": done
                    .staging_cleanup_pending
                    .as_ref()
                    .map(|path| path.display().to_string()),
            })
        );
        return;
    }
    println!("{destination}");
    if quiet {
        return;
    }
    eprintln!("Saved {} to {destination}", bytes(done.bytes_received));
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

fn bytes(value: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut size = value as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{value} B")
    } else {
        format!("{size:.1} {}", UNITS[unit])
    }
}

fn duration(seconds: u64) -> String {
    if seconds >= 3600 {
        format!(
            "{}:{:02}:{:02}",
            seconds / 3600,
            seconds / 60 % 60,
            seconds % 60
        )
    } else {
        format!("{}:{:02}", seconds / 60, seconds % 60)
    }
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
