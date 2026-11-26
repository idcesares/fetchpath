mod agent_setup;
mod agents;
mod cache;
mod cli_help;
mod client;
mod download;
mod engine;
mod inbox;
mod lan;
mod mcp;
mod queue;
mod remote;
mod reveal;
mod rules;
mod setup_cmd;
mod share;
mod sharing;
mod tools;
mod tui;
mod wait;
mod web;
mod when;

pub(crate) const VERSION: &str = env!("CARGO_PKG_VERSION");

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match args.first().map(String::as_str) {
        None => tui::run(&[], cli_help::HELP),
        Some("--plain" | "--json") => tui::run(&args, cli_help::HELP),
        Some("-h" | "--help") => {
            println!("{}", cli_help::HELP);
            0
        }
        Some("help") => cli_help::run_help(&args[1..]),
        Some("-V" | "--version" | "version") => {
            println!("fetchpath {VERSION}");
            0
        }
        Some(cmd)
            if args.len() == 2
                && matches!(args[1].as_str(), "-h" | "--help")
                && cli_help::command_help(cmd).is_some() =>
        {
            println!("{}", cli_help::command_help(cmd).expect("checked"));
            0
        }
        Some("download") => download::run(&args[1..]),
        Some("add") => queue::add(&args[1..]),
        Some("torrent") => queue::torrent(&args[1..]),
        Some("batch") => queue::batch(&args[1..]),
        Some("status") => queue::status(&args[1..]),
        Some("ls" | "list") => queue::ls(&args[1..]),
        Some("show") => queue::show(&args[1..]),
        Some("pause") => queue::control(queue::Control::Pause, &args[1..]),
        Some("resume") => queue::control(queue::Control::Resume, &args[1..]),
        Some("cancel") => queue::control(queue::Control::Cancel, &args[1..]),
        Some("retry") => queue::control(queue::Control::Retry, &args[1..]),
        Some("rm" | "remove") => queue::control(queue::Control::Remove, &args[1..]),
        Some("clean") => queue::clean(&args[1..]),
        Some("approve") => queue::control(queue::Control::Approve, &args[1..]),
        Some("deny") => queue::control(queue::Control::Deny, &args[1..]),
        Some("watch") => queue::watch(&args[1..]),
        Some("inspect") => queue::inspect(&args[1..]),
        Some("history") => queue::history(&args[1..]),
        Some("settings") => queue::settings(&args[1..]),
        Some("engine") => engine::run(&args[1..]),
        Some("hub") => queue::hub(&args[1..]),
        Some("web") => web::run(&args[1..]),
        Some("tools") => tools::run(&args[1..]),
        Some("setup" | "doctor") => setup_cmd::run(&args[1..]),
        Some("completions") => cli_help::completions(&args[1..]),
        Some("folder") => reveal::run(&args[1..]),
        Some("rules") => rules::run(&args[1..]),
        Some("mcp") => mcp::run(&args[1..]),
        Some("agents") => agents::run(&args[1..]),
        Some("inbox") => inbox::run(&args[1..]),
        Some("share") => sharing::run(&args[1..]),
        Some("agent-setup") => agent_setup::run(&args[1..]),
        Some("approvals") => agents::approvals(&args[1..]),
        Some("cache") => cache::run(&args[1..]),
        Some("lan") => lan::run(&args[1..]),
        Some("fetch-verified") => fetch_verified(&args[1..]),
        Some(other) if cli_help::looks_like_direct_link(other) => queue::add(&args),
        Some(other) => {
            if let Some(suggestion) = cli_help::suggest(other, cli_help::COMMANDS.iter().copied()) {
                eprintln!(
                    "fetchpath: unknown command {other}. Did you mean `{suggestion}`?\nRun `fetchpath --help` for usage."
                );
            } else {
                eprintln!("fetchpath: unknown command {other}\nRun `fetchpath --help` for usage.");
            }
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
