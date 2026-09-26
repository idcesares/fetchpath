mod client;
mod download;
mod engine;
mod lan;
mod queue;
mod reveal;
mod rules;
mod tools;
mod tui;
mod wait;
mod when;

pub(crate) const VERSION: &str = env!("CARGO_PKG_VERSION");

const HELP: &str = "Fetchpath — download files from the command line.

Usage:
  fetchpath [--plain]        Open the interactive terminal (in a terminal only)
  fetchpath download LINK [DESTINATION] [--sha256 HEX] [--json] [--quiet]
  fetchpath --version
  fetchpath --help

The download queue, kept by the Fetchpath engine:
  fetchpath add LINK... [--to FOLDER|FILE] [--sha256 HEX] [--quality Q] [--at TIME] [--wait]
  fetchpath batch FILE|- [--to FOLDER] [--at TIME] [--wait]
  fetchpath ls [--active | --failed]          fetchpath show JOB...
  fetchpath pause | resume | cancel | retry | rm JOB...
  fetchpath approve | deny JOB...             answer an agent's request that waits for you
  fetchpath watch [JOB]                       fetchpath inspect LINK
  fetchpath folder JOB                        show its file in File Explorer
  fetchpath history [TEXT] [--limit N]        fetchpath settings [NAME [VALUE]]
  fetchpath engine status | stop
  fetchpath rules [list | add ... | rm ID | test LINK]   where downloads go, by site, type or size

Video and audio need two free programs, yt-dlp and ffmpeg, that are not
part of Fetchpath:
  fetchpath tools                             whether they are set up
  fetchpath tools install [--yes]             download and set them up (asks first)
  fetchpath tools use FOLDER                  use copies you already have

JOB is a download's number in `fetchpath ls`, or the start of its id.
Every queue command takes --json to print the engine's own records.


Paired devices (advanced; off until you run `fetchpath lan enable`):
  fetchpath lan id | enable | disable | peers | unpair KEY
  fetchpath lan pair-host [BIND]      then on the other device:
  fetchpath lan pair-join ADDRESS CODE [LABEL]
  fetchpath lan serve [BIND]
  fetchpath fetch-verified --sha256 HEX --size BYTES [--peer ADDRESS=KEY]... LINK DESTINATION
  fetchpath cache status

DESTINATION may be a file name or a folder. Without one, the file goes where
a matching rule says, or to your Downloads folder, under the name the link
suggests. Fetchpath never
overwrites an existing file.

Options:
  --sha256 HEX   Save the file only if it matches this SHA-256 checksum.
  --json         Print one JSON result on standard output, and no progress.
  -q, --quiet    Print only the saved path.

Exit codes:
  0 saved   1 engine unavailable   2 bad input   3 file already exists
  4 network or server   5 checksum did not match   6 could not write
  130 cancelled or interrupted

The full guide is docs\\CLI.md in the Fetchpath install folder.";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match args.first().map(String::as_str) {
        None => tui::run(&[], HELP),
        Some("--plain" | "--json") => tui::run(&args, HELP),
        Some("-h" | "--help" | "help") => {
            println!("{HELP}");
            0
        }
        Some("-V" | "--version" | "version") => {
            println!("fetchpath {VERSION}");
            0
        }
        Some("download") => download::run(&args[1..]),
        Some("add") => queue::add(&args[1..]),
        Some("batch") => queue::batch(&args[1..]),
        Some("ls" | "list") => queue::ls(&args[1..]),
        Some("show") => queue::show(&args[1..]),
        Some("pause") => queue::control(queue::Control::Pause, &args[1..]),
        Some("resume") => queue::control(queue::Control::Resume, &args[1..]),
        Some("cancel") => queue::control(queue::Control::Cancel, &args[1..]),
        Some("retry") => queue::control(queue::Control::Retry, &args[1..]),
        Some("rm" | "remove") => queue::control(queue::Control::Remove, &args[1..]),
        Some("approve") => queue::control(queue::Control::Approve, &args[1..]),
        Some("deny") => queue::control(queue::Control::Deny, &args[1..]),
        Some("watch") => queue::watch(&args[1..]),
        Some("inspect") => queue::inspect(&args[1..]),
        Some("history") => queue::history(&args[1..]),
        Some("settings") => queue::settings(&args[1..]),
        Some("engine") => engine::run(&args[1..]),
        Some("tools") => tools::run(&args[1..]),
        Some("folder") => reveal::run(&args[1..]),
        Some("rules") => rules::run(&args[1..]),
        Some("lan" | "cache" | "fetch-verified") => paired_devices(&args),
        Some(other) => {
            eprintln!("fetchpath: unknown command {other}\nRun `fetchpath --help` for usage.");
            download::EXIT_USAGE
        }
    };
    std::process::exit(code);
}

/// Cache and paired-device commands, which print JSON for scripts.
fn paired_devices(args: &[String]) -> i32 {
    let outcome = match args[0].as_str() {
        "lan" => lan::run_lan(&args[1..]),
        "cache" if args.get(1).map(String::as_str) == Some("status") => lan::cache_status(),
        "cache" => Err(lan::USAGE.to_owned()),
        _ => lan::fetch_verified(&args[1..]),
    };
    match outcome {
        Ok(value) => {
            println!("{value}");
            0
        }
        Err(error) => {
            eprintln!("{error}");
            if error.starts_with("usage:") {
                download::EXIT_USAGE
            } else {
                1
            }
        }
    }
}
