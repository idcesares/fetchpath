mod download;
mod lan;

const VERSION: &str = env!("CARGO_PKG_VERSION");

const HELP: &str = "Fetchpath — download files from the command line.

Usage:
  fetchpath download LINK [DESTINATION] [--sha256 HEX] [--json] [--quiet]
  fetchpath --version
  fetchpath --help

DESTINATION may be a file name or a folder. Without one, the file goes to
your Downloads folder under the name the link suggests. Fetchpath never
overwrites an existing file.

Options:
  --sha256 HEX   Save the file only if it matches this SHA-256 checksum.
  --json         Print one JSON result on standard output, and no progress.
  -q, --quiet    Print only the saved path.

Exit codes:
  0 saved   2 bad input   3 file already exists   4 network or server
  5 checksum did not match   6 could not write   130 cancelled

The full guide is docs\\CLI.md in the Fetchpath install folder.";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match args.first().map(String::as_str) {
        None => {
            eprintln!("{HELP}");
            download::EXIT_USAGE
        }
        Some("-h" | "--help" | "help") => {
            println!("{HELP}");
            0
        }
        Some("-V" | "--version" | "version") => {
            println!("fetchpath {VERSION}");
            0
        }
        Some("download") => download::run(&args[1..]),
        // Cache and paired-LAN commands are experimental and unlisted until
        // FP-020's independent review is complete.
        Some("lan" | "cache" | "fetch-verified") => experimental(&args),
        Some(other) => {
            eprintln!("fetchpath: unknown command {other}\nRun `fetchpath --help` for usage.");
            download::EXIT_USAGE
        }
    };
    std::process::exit(code);
}

fn experimental(args: &[String]) -> i32 {
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
