mod agents;
mod cache;
mod client;
mod download;
mod engine;
mod lan;
mod mcp;
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
  fetchpath torrent MAGNET|HTTPS_TORRENT|TORRENT_FILE [--to NEW_FOLDER] [--upload] [--wait]
  fetchpath batch FILE|- [--to FOLDER] [--at TIME] [--wait]
  fetchpath ls [--active | --failed]          fetchpath show JOB...
  fetchpath pause | resume | cancel | retry | rm JOB...
  fetchpath approvals                         agents' requests that wait for you
  fetchpath approve | deny JOB...             answer an agent's request that waits for you
  fetchpath agents [grant NAME FOLDER... | revoke NAME [FOLDER...] | limit NAME --size S --per-hour N]
  fetchpath watch [JOB]                       fetchpath inspect LINK
  fetchpath folder JOB                        show its file in File Explorer
  fetchpath history [TEXT] [--limit N]        fetchpath settings [NAME [VALUE]]
  fetchpath engine status | stop
  fetchpath rules [list | add ... | rm ID | test LINK]   where downloads go, by site, type or size
  fetchpath cache [status | clear]            verified copies kept for downloads with a checksum
  fetchpath mcp [--agent NAME]               serve Fetchpath to an AI agent host (MCP, stdio)

Video and audio need two free programs, yt-dlp and ffmpeg, that are not
part of Fetchpath:
  fetchpath tools                             whether they are set up
  fetchpath tools install [--yes]             download and set them up (asks first)
  fetchpath tools use FOLDER                  use copies you already have

JOB is a download's number in `fetchpath ls`, or the start of its id.
Every queue command takes --json to print the engine's own records.


Paired devices (advanced; sharing is off until you run `fetchpath lan on`):
  fetchpath lan [status]                      this computer and its paired devices
  fetchpath lan on | off                      share checksum-verified public files with them
  fetchpath lan pair                          show a code; then on the other device:
  fetchpath lan join ADDRESS CODE [NAME]      fetchpath lan unpair KEY
  fetchpath fetch-verified --sha256 HEX --size BYTES [--peer ADDRESS=KEY]... LINK DESTINATION

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
        Some("torrent") => queue::torrent(&args[1..]),
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
        Some("mcp") => mcp::run(&args[1..]),
        Some("agents") => agents::run(&args[1..]),
        Some("approvals") => agents::approvals(&args[1..]),
        Some("cache") => cache::run(&args[1..]),
        Some("lan") => lan::run(&args[1..]),
        Some("fetch-verified") => fetch_verified(&args[1..]),
        Some(other) => {
            eprintln!("fetchpath: unknown command {other}\nRun `fetchpath --help` for usage.");
            download::EXIT_USAGE
        }
    };
    std::process::exit(code);
}

/// Asks paired devices, then the link, and prints JSON for scripts.
fn fetch_verified(args: &[String]) -> i32 {
    match lan::fetch_verified(args) {
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
