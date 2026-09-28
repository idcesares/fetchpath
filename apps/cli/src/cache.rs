//! `fetchpath cache`: the engine's content cache (FP-032). A download with a
//! checksum leaves a verified copy there, and the same file downloaded again
//! finishes from it without a transfer.

use crate::client::{self, Engine};
use crate::download::EXIT_USAGE;
use fetchpath_protocol::ProtocolError;
use fetchpath_protocol::command::Command;
use fetchpath_protocol::describe::bytes;
use fetchpath_protocol::message::CommandResult;
use fetchpath_protocol::model::CacheView;

pub const USAGE: &str = "usage: fetchpath cache [status | clear] [--json]
change its size with: fetchpath settings cache-quota-bytes BYTES";

pub fn run(args: &[String]) -> i32 {
    let json = args.iter().any(|arg| arg == "--json");
    let command = match args
        .iter()
        .map(String::as_str)
        .filter(|arg| *arg != "--json")
        .collect::<Vec<_>>()
        .as_slice()
    {
        [] | ["status"] => Command::CacheStatus,
        ["clear"] => Command::ClearCache,
        _ => {
            eprintln!("{USAGE}");
            return EXIT_USAGE;
        }
    };
    let cleared = matches!(command, Command::ClearCache);
    match fetch(command) {
        Ok(cache) if json => {
            client::print_json(&CommandResult::Cache { cache });
            0
        }
        Ok(cache) => {
            if cleared {
                println!("Cache cleared. Saved downloads are not affected.");
            }
            println!("{}", describe(&cache));
            0
        }
        Err(error) => client::fail(&error, json),
    }
}

fn fetch(command: Command) -> Result<CacheView, ProtocolError> {
    match Engine::connect()?.send(command)? {
        CommandResult::Cache { cache } => Ok(cache),
        other => Err(client::unexpected(&other)),
    }
}

pub(crate) fn describe(cache: &CacheView) -> String {
    let held = match cache.entries {
        0 => "The cache is empty".to_owned(),
        1 => format!("The cache holds {} in 1 file", bytes(cache.bytes)),
        n => format!("The cache holds {} in {n} files", bytes(cache.bytes)),
    };
    format!(
        "{held}; it keeps up to {} (between {} and {}).",
        bytes(cache.quota_bytes),
        bytes(cache.min_quota_bytes),
        bytes(cache.max_quota_bytes)
    )
}
