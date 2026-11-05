//! `fetchpath share` (FP-117): public links to your files through a tunnel you
//! run, and the guided setup that proves the tunnel works first.
//!
//! The engine keeps links in memory only. A link is printed once, when it is
//! made; nothing here can show it again.

use crate::client::{self, Engine};
use crate::download::EXIT_USAGE;
use fetchpath_protocol::ProtocolError;
use fetchpath_protocol::command::Command;
use fetchpath_protocol::message::CommandResult;
use fetchpath_protocol::model::{ShareItem, ShareState, ShareView};
use fetchpath_protocol::send::OfferSource;
use std::time::{Duration, Instant};

const USAGE: &str = "usage: fetchpath share [status]
       fetchpath share setup URL [--port N]   store the tunnel's https address and prove it with a test link
       fetchpath share test                   make a new test link
       fetchpath share on | off               turn file links on (after a passed test) or off
       fetchpath share create FILE [--hours N]   a single-use link to a file (shown once)
       fetchpath share revoke HANDLE|all
URL is the public address your tunnel (Cloudflare Tunnel, Tailscale Funnel) gives,
forwarding to this computer's share port. Links last a day by default (at most 30 days).";

/// How long `setup` waits for the listener to start.
const START_WAIT: Duration = Duration::from_secs(10);
/// How long `setup` waits for the test link to be downloaded.
const TEST_WAIT: Duration = Duration::from_secs(15 * 60);

pub fn run(args: &[String]) -> i32 {
    let json = args.iter().any(|arg| arg == "--json");
    let words: Vec<&str> = args
        .iter()
        .map(String::as_str)
        .filter(|arg| *arg != "--json")
        .collect();
    let result = match words.as_slice() {
        [] | ["status"] => Engine::connect().and_then(|engine| status(&engine, json)),
        ["setup", url] => Engine::connect().and_then(|engine| setup(&engine, url, None, json)),
        ["setup", url, "--port", port] => match port.parse::<u16>() {
            Ok(port) if port > 0 => {
                Engine::connect().and_then(|engine| setup(&engine, url, Some(port), json))
            }
            _ => {
                eprintln!("{USAGE}");
                return EXIT_USAGE;
            }
        },
        ["test"] => Engine::connect().and_then(|engine| test(&engine, json)),
        ["on"] => Engine::connect().and_then(|engine| enable(&engine, true, json)),
        ["off"] => Engine::connect().and_then(|engine| enable(&engine, false, json)),
        ["create", file] => Engine::connect().and_then(|engine| create(&engine, file, None, json)),
        ["create", file, "--hours", hours] => match hours.parse::<u64>() {
            Ok(hours) if (1..=24 * 30).contains(&hours) => {
                Engine::connect().and_then(|engine| create(&engine, file, Some(hours), json))
            }
            _ => {
                eprintln!("{USAGE}");
                return EXIT_USAGE;
            }
        },
        ["revoke", handle] => Engine::connect().and_then(|engine| revoke(&engine, handle, json)),
        _ => {
            eprintln!("{USAGE}");
            return EXIT_USAGE;
        }
    };
    match result {
        Ok(()) => 0,
        Err(error) => client::fail(&error, json),
    }
}

fn view(engine: &Engine) -> Result<ShareView, ProtocolError> {
    sharing(engine.send(Command::ShareStatus)?)
}

fn sharing(result: CommandResult) -> Result<ShareView, ProtocolError> {
    match result {
        CommandResult::Sharing { sharing } => Ok(sharing),
        other => Err(client::unexpected(&other)),
    }
}

fn status(engine: &Engine, json: bool) -> Result<(), ProtocolError> {
    let sharing = view(engine)?;
    if json {
        client::print_json(&sharing);
    } else {
        print_view(&sharing);
    }
    Ok(())
}

fn print_view(sharing: &ShareView) {
    match &sharing.base_url {
        None => println!("No tunnel address is stored. Run `fetchpath share setup URL`."),
        Some(url) => {
            println!("Tunnel address: {url}");
            match (&sharing.serving, &sharing.problem) {
                (Some(bound), _) => println!("Listening on {bound} (your tunnel forwards here)."),
                (None, Some(problem)) => println!("Not listening: {problem}"),
                (None, None) => println!("Not listening yet."),
            }
        }
    }
    println!("Share port: {}", sharing.port);
    println!(
        "Test link: {}",
        if sharing.tested {
            "delivered"
        } else {
            "not delivered yet"
        }
    );
    println!(
        "File links: {}",
        if sharing.enabled {
            "on"
        } else if sharing.tested {
            "off (run `fetchpath share on`)"
        } else {
            "off (they turn on only after a delivered test link)"
        }
    );
    let files: Vec<&ShareItem> = sharing.shares.iter().filter(|item| !item.test).collect();
    if files.is_empty() {
        println!("No file links.");
    }
    for item in files {
        print_item(item);
    }
}

fn print_item(item: &ShareItem) {
    println!(
        "{}  {}  {}  {} bytes  opened {}x  {} of {} bytes sent  expires in {}",
        &item.handle[..item.handle.len().min(8)],
        state_label(item.state),
        item.name,
        item.size,
        item.opened,
        item.delivered,
        item.total,
        client::duration(item.expires_in_seconds),
    );
}

fn state_label(state: ShareState) -> &'static str {
    match state {
        ShareState::Live => "live",
        ShareState::Completed => "used up",
        ShareState::Revoked => "revoked",
        ShareState::Expired => "expired",
        ShareState::Changed => "file changed",
        ShareState::OverBudget => "stopped (too many requests)",
        ShareState::Unknown => "unknown",
    }
}

/// The text of a QR code for `text`, drawn with block characters for a dark
/// terminal. `None` when it does not fit in a code.
pub(crate) fn qr(text: &str) -> Option<String> {
    use qrcode::render::unicode::Dense1x2;
    let code = qrcode::QrCode::with_error_correction_level(text, qrcode::EcLevel::L).ok()?;
    Some(
        code.render::<Dense1x2>()
            .dark_color(Dense1x2::Light)
            .light_color(Dense1x2::Dark)
            .quiet_zone(true)
            .build(),
    )
}

/// Stores the address, waits for the listener, then walks the person through
/// the test link. Turning sharing on stays a separate step.
fn setup(engine: &Engine, url: &str, port: Option<u16>, json: bool) -> Result<(), ProtocolError> {
    let configured = sharing(engine.send(Command::ConfigureSharing {
        base_url: Some(url.trim().to_owned()),
        port,
    })?)?;
    let deadline = Instant::now() + START_WAIT;
    let mut current = configured;
    while current.serving.is_none() && current.problem.is_none() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(200));
        current = view(engine)?;
    }
    if current.serving.is_none() {
        let problem = current
            .problem
            .unwrap_or_else(|| "The share listener did not start.".to_owned());
        return Err(client::input_error(&problem));
    }
    if !json {
        println!("Stored {url}.");
        println!(
            "Your tunnel must forward that address to 127.0.0.1:{} on this computer.",
            current.port
        );
    }
    test_and_wait(engine, json)
}

fn test(engine: &Engine, json: bool) -> Result<(), ProtocolError> {
    test_and_wait(engine, json)
}

fn test_and_wait(engine: &Engine, json: bool) -> Result<(), ProtocolError> {
    let link = match engine.send(Command::StartShareTest)? {
        CommandResult::ShareLink { link, .. } => link,
        other => return Err(client::unexpected(&other)),
    };
    if !json {
        println!("\nOpen this test link on a phone with Wi-Fi off (mobile data):\n");
        println!("  {}\n", link.expose());
        if let Some(code) = qr(link.expose()) {
            println!("{code}");
        }
        println!(
            "Press Download on the page. Waiting up to 15 minutes (Ctrl+C to stop waiting)..."
        );
    }
    client::catch_interrupt();
    let deadline = Instant::now() + TEST_WAIT;
    let mut current = view(engine)?;
    while !current.tested && !client::interrupted() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(500));
        current = view(engine)?;
    }
    if json {
        client::print_json(&serde_json::json!({
            "tested": current.tested,
            "link": link.expose(),
            "sharing": current,
        }));
        return Ok(());
    }
    if current.tested {
        println!(
            "The test link was delivered. File links are still off: run `fetchpath share on` to turn them on."
        );
    } else {
        println!(
            "The test link was not downloaded yet. It stays valid for a while; run `fetchpath share test` for a new one."
        );
    }
    Ok(())
}

fn enable(engine: &Engine, enabled: bool, json: bool) -> Result<(), ProtocolError> {
    let sharing = sharing(engine.send(Command::SetSharingEnabled { enabled })?)?;
    if json {
        client::print_json(&sharing);
    } else if sharing.enabled {
        println!(
            "File links are on. Revoke them with `fetchpath share revoke all`, or turn them off with `fetchpath share off`."
        );
    } else {
        println!("File links are off.");
    }
    Ok(())
}

fn create(
    engine: &Engine,
    file: &str,
    hours: Option<u64>,
    json: bool,
) -> Result<(), ProtocolError> {
    let path = std::path::absolute(file)
        .map_err(|error| client::input_error(&format!("That path cannot be used: {error}")))?;
    let (link, item) = match engine.send(Command::CreateShare {
        source: OfferSource::Path {
            path: path.to_string_lossy().into_owned(),
        },
        expires_in_seconds: hours.map(|hours| hours * 3600),
    })? {
        CommandResult::ShareLink { link, share } => (link, share),
        other => return Err(client::unexpected(&other)),
    };
    if json {
        client::print_json(&serde_json::json!({ "link": link.expose(), "share": item }));
        return Ok(());
    }
    println!("{}\n", link.expose());
    if let Some(code) = qr(link.expose()) {
        println!("{code}");
    }
    println!(
        "{} bytes. Works once, for {}. Shown only now: it cannot be listed again.",
        item.size,
        client::duration(item.expires_in_seconds)
    );
    println!(
        "Anyone who has the whole link can download the file once. The file stays locked (it cannot be changed, renamed or deleted) while the link lives."
    );
    Ok(())
}

fn revoke(engine: &Engine, reference: &str, json: bool) -> Result<(), ProtocolError> {
    let sharing = if reference == "all" {
        sharing(engine.send(Command::RevokeAllShares)?)?
    } else {
        let reference = reference.trim();
        if reference.len() < 4 {
            return Err(client::input_error(
                "Give the link's handle (at least its first 4 characters) or `all`.",
            ));
        }
        let matches: Vec<String> = view(engine)?
            .shares
            .into_iter()
            .filter(|item| item.handle.starts_with(reference))
            .map(|item| item.handle)
            .collect();
        let handle = match matches.as_slice() {
            [one] => one.clone(),
            [] => return Err(client::input_error("There is no link with that handle.")),
            _ => {
                return Err(client::input_error(
                    "More than one link starts with that; give more of the handle.",
                ));
            }
        };
        sharing(engine.send(Command::RevokeShare { handle })?)?
    };
    if json {
        client::print_json(&sharing);
    } else {
        println!("Revoked.");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::qr;

    #[test]
    fn a_link_fits_in_a_qr_code_drawn_in_block_characters() {
        let link = format!(
            "https://share.example.com/s/{}#{}",
            "A".repeat(43),
            "B".repeat(43)
        );
        let drawn = qr(&link).expect("a link of this size fits");
        let lines: Vec<&str> = drawn.lines().collect();
        assert!(lines.len() > 10, "{} lines", lines.len());
        let width = lines[0].chars().count();
        assert!(lines.iter().all(|line| line.chars().count() == width));
        assert!(drawn.chars().any(|c| matches!(c, '█' | '▀' | '▄')));
    }
}
