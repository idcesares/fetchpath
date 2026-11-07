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
use fetchpath_protocol::model::{RelayKey, ShareItem, ShareState, ShareView};
use fetchpath_protocol::qr_image;
use fetchpath_protocol::send::OfferSource;
use std::time::{Duration, Instant};

const USAGE: &str = "usage: fetchpath share [status]
       fetchpath share setup URL [--port N]   store the tunnel's https address and prove it with a test link
       fetchpath share relay URL [--stored-key] [--qr IMAGE]   use your relay at URL; its key is read from stdin
       fetchpath share relay off              go back to a tunnel (the relay key stays stored)
       fetchpath share test [--qr IMAGE]      make a new test link
       fetchpath share on | off               turn file links on (after a passed test) or off
       fetchpath share create FILE [--hours N] [--qr IMAGE]   a single-use link to a file (shown once)
       fetchpath share revoke HANDLE|all
URL is the public address your tunnel (Cloudflare Tunnel, Tailscale Funnel) gives,
forwarding to this computer's share port. A relay is a fetchpath-relay you run on a
server (docs/user/RELAY.md): pipe its 64-character key in, never pass it as an argument.
Links last a day by default (at most 30 days).
--qr IMAGE (setup, test, create) also saves the link's QR code as IMAGE, a new .svg or .png
file. The image IS the link: anyone who has it can download the file once.";

/// How long `setup` waits for the listener to start.
const START_WAIT: Duration = Duration::from_secs(10);
/// How long `setup` waits for the test link to be downloaded.
const TEST_WAIT: Duration = Duration::from_secs(15 * 60);

pub fn run(args: &[String]) -> i32 {
    let json = args.iter().any(|arg| arg == "--json");
    let mut words: Vec<&str> = args
        .iter()
        .map(String::as_str)
        .filter(|arg| *arg != "--json")
        .collect();
    let Ok(image) = take_image_path(&mut words) else {
        eprintln!("{USAGE}");
        return EXIT_USAGE;
    };
    let image = image.as_deref();
    // `--qr` belongs to the commands that make a link.
    let makes_link = matches!(
        words.first(),
        Some(&("setup" | "test" | "create" | "relay"))
    );
    if image.is_some() && !makes_link {
        eprintln!("{USAGE}");
        return EXIT_USAGE;
    }
    let result = match words.as_slice() {
        [] | ["status"] => Engine::connect().and_then(|engine| status(&engine, json)),
        ["setup", url] => {
            Engine::connect().and_then(|engine| setup(&engine, url, None, image, json))
        }
        ["setup", url, "--port", port] => match port.parse::<u16>() {
            Ok(port) if port > 0 => {
                Engine::connect().and_then(|engine| setup(&engine, url, Some(port), image, json))
            }
            _ => {
                eprintln!("{USAGE}");
                return EXIT_USAGE;
            }
        },
        ["relay", "off"] => Engine::connect().and_then(|engine| relay_off(&engine, json)),
        ["relay", url] => {
            Engine::connect().and_then(|engine| relay(&engine, url, false, image, json))
        }
        ["relay", url, "--stored-key"] => {
            Engine::connect().and_then(|engine| relay(&engine, url, true, image, json))
        }
        ["test"] => Engine::connect().and_then(|engine| test(&engine, image, json)),
        ["on"] => Engine::connect().and_then(|engine| enable(&engine, true, json)),
        ["off"] => Engine::connect().and_then(|engine| enable(&engine, false, json)),
        ["create", file] => {
            Engine::connect().and_then(|engine| create(&engine, file, None, image, json))
        }
        ["create", file, "--hours", hours] => match hours.parse::<u64>() {
            Ok(hours) if (1..=24 * 30).contains(&hours) => {
                Engine::connect().and_then(|engine| create(&engine, file, Some(hours), image, json))
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
        None => println!(
            "No address is stored. Run `fetchpath share setup URL` (tunnel) or `fetchpath share relay URL`."
        ),
        Some(url) if sharing.relay => {
            println!("Relay address: {url}");
            match (&sharing.serving, &sharing.problem) {
                (Some(_), _) => println!("Enrolled with the relay."),
                (None, Some(problem)) => println!("Not enrolled: {problem}"),
                (None, None) => println!("Not enrolled yet."),
            }
        }
        Some(url) => {
            println!("Tunnel address: {url}");
            match (&sharing.serving, &sharing.problem) {
                (Some(bound), _) => println!("Listening on {bound} (your tunnel forwards here)."),
                (None, Some(problem)) => println!("Not listening: {problem}"),
                (None, None) => println!("Not listening yet."),
            }
        }
    }
    if sharing.relay_key_stored {
        println!("A relay key is stored (it cannot be shown again).");
    }
    if !sharing.relay {
        println!("Share port: {}", sharing.port);
    }
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

/// Removes `--qr IMAGE` from `words`. `Err` when it has no usable file name:
/// the name must end in `.svg` or `.png`.
fn take_image_path(words: &mut Vec<&str>) -> Result<Option<String>, ()> {
    let Some(at) = words.iter().position(|word| *word == "--qr") else {
        return Ok(None);
    };
    let path = words.get(at + 1).map(|path| (*path).to_owned()).ok_or(())?;
    words.drain(at..at + 2);
    if words.contains(&"--qr") || qr_image::Format::from_path(&path).is_none() {
        return Err(());
    }
    Ok(Some(path))
}

/// The modules of a QR code for `text`, row by row: `1` dark, `0` light. The
/// same shape the desktop draws and saves from.
fn qr_rows(text: &str) -> Option<Vec<String>> {
    let code = qrcode::QrCode::with_error_correction_level(text, qrcode::EcLevel::L).ok()?;
    let width = code.width();
    Some(
        code.to_colors()
            .chunks(width)
            .map(|row| {
                row.iter()
                    .map(|color| {
                        if *color == qrcode::Color::Dark {
                            '1'
                        } else {
                            '0'
                        }
                    })
                    .collect()
            })
            .collect(),
    )
}

/// Saves the link's QR code to `path` when the person asked for an image. The
/// file IS the link, so it is written only here, only to the path they gave,
/// and never over a file that exists.
fn save_image(link: &str, path: Option<&str>) -> Option<Result<(), String>> {
    let path = path?;
    let write = || -> Result<(), String> {
        let format = qr_image::Format::from_path(path).ok_or("Use a .svg or .png file name.")?;
        let rows = qr_rows(link).ok_or("The link does not fit in a QR code.")?;
        let bytes = qr_image::render(&rows, format).ok_or("The QR code could not be drawn.")?;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(|error| format!("Could not save the QR image: {error}"))?;
        std::io::Write::write_all(&mut file, &bytes).map_err(|error| {
            // Nothing half-written is left behind: the file was ours (new).
            drop(file);
            let _ = std::fs::remove_file(path);
            format!("Could not save the QR image: {error}")
        })
    };
    Some(write())
}

/// Tells the person what happened to the image they asked for. A failure is
/// reported but does not hide the link: it was made and is shown once.
fn report_image(saved: &Option<Result<(), String>>, path: Option<&str>) {
    match (saved, path) {
        (Some(Ok(())), Some(path)) => println!(
            "Saved the QR image to {path}. It is the link itself: anyone who has the file can download once. Delete it when you are done."
        ),
        (Some(Err(problem)), _) => eprintln!("{problem}"),
        _ => {}
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
fn setup(
    engine: &Engine,
    url: &str,
    port: Option<u16>,
    image: Option<&str>,
    json: bool,
) -> Result<(), ProtocolError> {
    // A tunnel address is not a relay's: leave relay mode first.
    engine.send(Command::ConfigureRelay {
        relay: false,
        key: None,
    })?;
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
    test_and_wait(engine, image, json)
}

fn test(engine: &Engine, image: Option<&str>, json: bool) -> Result<(), ProtocolError> {
    test_and_wait(engine, image, json)
}

fn test_and_wait(engine: &Engine, image: Option<&str>, json: bool) -> Result<(), ProtocolError> {
    let link = match engine.send(Command::StartShareTest)? {
        CommandResult::ShareLink { link, .. } => link,
        other => return Err(client::unexpected(&other)),
    };
    let saved = save_image(link.expose(), image);
    if !json {
        println!("\nOpen this test link on a phone with Wi-Fi off (mobile data):\n");
        println!("  {}\n", link.expose());
        if let Some(code) = qr(link.expose()) {
            println!("{code}");
        }
        report_image(&saved, image);
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
            "qr_image": image.filter(|_| matches!(saved, Some(Ok(())))),
            "sharing": current,
        }));
        report_image(&saved, None);
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

/// One line of the relay key from stdin. It is never an argument (shell
/// history, process lists) and never echoed back.
fn read_key() -> Result<RelayKey, ProtocolError> {
    eprintln!("Paste the relay's key (64 hex characters) and press Enter:");
    let mut line = zeroize::Zeroizing::new(String::new());
    std::io::stdin()
        .read_line(&mut line)
        .map_err(|_| client::input_error("The key could not be read."))?;
    RelayKey::try_from(std::mem::take(&mut *line))
        .map_err(|_| client::input_error("That is not a relay key (64 hex characters)."))
}

/// Points the engine at a relay, enrolls it and walks the person through the
/// test link, as `setup` does for a tunnel.
fn relay(
    engine: &Engine,
    url: &str,
    stored_key: bool,
    image: Option<&str>,
    json: bool,
) -> Result<(), ProtocolError> {
    let key = if stored_key { None } else { Some(read_key()?) };
    engine.send(Command::ConfigureSharing {
        base_url: Some(url.trim().to_owned()),
        port: None,
    })?;
    let mut current = sharing(engine.send(Command::ConfigureRelay { relay: true, key })?)?;
    let deadline = Instant::now() + START_WAIT;
    while current.serving.is_none() && current.problem.is_none() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(200));
        current = view(engine)?;
    }
    if current.serving.is_none() {
        let problem = current
            .problem
            .unwrap_or_else(|| "The relay did not answer.".to_owned());
        return Err(client::input_error(&problem));
    }
    if !json {
        println!("Enrolled with the relay at {url}.");
    }
    test_and_wait(engine, image, json)
}

fn relay_off(engine: &Engine, json: bool) -> Result<(), ProtocolError> {
    let sharing = sharing(engine.send(Command::ConfigureRelay {
        relay: false,
        key: None,
    })?)?;
    if json {
        client::print_json(&sharing);
    } else {
        println!("Relay mode is off. Run `fetchpath share setup URL` to use a tunnel.");
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
    image: Option<&str>,
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
    let saved = save_image(link.expose(), image);
    if json {
        client::print_json(&serde_json::json!({
            "link": link.expose(),
            "share": item,
            "qr_image": image.filter(|_| matches!(saved, Some(Ok(())))),
        }));
        report_image(&saved, None);
        return Ok(());
    }
    println!("{}\n", link.expose());
    if let Some(code) = qr(link.expose()) {
        println!("{code}");
    }
    report_image(&saved, image);
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
    use super::{qr, save_image, take_image_path};

    fn link() -> String {
        format!(
            "https://share.example.com/s/{}#{}",
            "A".repeat(43),
            "B".repeat(43)
        )
    }

    #[test]
    fn the_image_option_needs_a_svg_or_png_name_and_leaves_the_rest() {
        let mut words = vec!["create", "a.bin", "--qr", "code.PNG", "--hours", "2"];
        assert_eq!(take_image_path(&mut words), Ok(Some("code.PNG".to_owned())));
        assert_eq!(words, ["create", "a.bin", "--hours", "2"]);
        let mut none = vec!["test"];
        assert_eq!(take_image_path(&mut none), Ok(None));
        for bad in [
            vec!["test", "--qr"],
            vec!["test", "--qr", "code.jpg"],
            vec!["test", "--qr", "a.png", "--qr", "b.png"],
        ] {
            assert!(take_image_path(&mut bad.clone()).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn the_image_is_written_only_to_the_named_new_file() {
        let dir = tempfile::tempdir().unwrap();
        let svg = dir.path().join("code.svg");
        let png = dir.path().join("code.png");
        for path in [&svg, &png] {
            let name = path.to_str().unwrap();
            assert_eq!(save_image(&link(), Some(name)), Some(Ok(())));
            // A second save never replaces what is there.
            assert!(matches!(save_image(&link(), Some(name)), Some(Err(_))));
        }
        assert!(std::fs::read_to_string(&svg).unwrap().starts_with("<svg "));
        assert_eq!(&std::fs::read(&png).unwrap()[1..4], b"PNG");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
        assert_eq!(save_image(&link(), None), None);
    }

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
