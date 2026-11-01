//! `fetchpath inbox` (FP-115): files an agent offered you. Nothing is copied
//! until you accept. The engine checks the copy against the offered hash and
//! never replaces a file.

use crate::client::{self, Engine};
use crate::download::EXIT_USAGE;
use fetchpath_protocol::command::Command;
use fetchpath_protocol::message::CommandResult;
use fetchpath_protocol::send::{HASH_NOTICE, OfferState, OfferView, SENDER_NOTICE};
use fetchpath_protocol::ProtocolError;
use std::time::{Duration, Instant};

const USAGE: &str = "usage: fetchpath inbox [list]
       fetchpath inbox accept OFFER [--to FOLDER]   copy the file into FOLDER (default: your download folder)
       fetchpath inbox decline OFFER
OFFER is the start of an offer's id from `fetchpath inbox`. The sender is a
label the program declared, not proof of who it is.";

/// How long `accept` waits for the copy to finish before saying it goes on.
const COPY_WAIT: Duration = Duration::from_secs(60);

pub fn run(args: &[String]) -> i32 {
    let json = args.iter().any(|arg| arg == "--json");
    let words: Vec<&str> = args
        .iter()
        .map(String::as_str)
        .filter(|arg| *arg != "--json")
        .collect();
    let result = match words.as_slice() {
        [] | ["list"] => Engine::connect().and_then(|engine| list(&engine, json)),
        ["accept", id] => Engine::connect().and_then(|engine| accept(&engine, id, None, json)),
        ["accept", id, "--to", folder] => {
            Engine::connect().and_then(|engine| accept(&engine, id, Some(folder), json))
        }
        ["decline", id] => Engine::connect().and_then(|engine| decline(&engine, id, json)),
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

fn inbox(engine: &Engine) -> Result<Vec<OfferView>, ProtocolError> {
    match engine.send(Command::ListInbox)? {
        CommandResult::Inbox { offers } => Ok(offers),
        other => Err(client::unexpected(&other)),
    }
}

fn find(engine: &Engine, reference: &str) -> Result<OfferView, ProtocolError> {
    let reference = reference.trim();
    if reference.len() < 4 {
        return Err(client::input_error(
            "Give the offer's id (at least its first 4 characters).",
        ));
    }
    let mut found: Vec<OfferView> = inbox(engine)?
        .into_iter()
        .filter(|offer| offer.job_id.as_str().starts_with(reference))
        .collect();
    match found.len() {
        1 => Ok(found.remove(0)),
        0 => Err(client::input_error("There is no offer with that id.")),
        _ => Err(client::input_error(
            "More than one offer starts with that; give more of the id.",
        )),
    }
}

fn list(engine: &Engine, json: bool) -> Result<(), ProtocolError> {
    let offers = inbox(engine)?;
    if json {
        println!("{}", serde_json::json!({ "offers": offers }));
        return Ok(());
    }
    if offers.is_empty() {
        println!("Nothing has been offered to you.");
        return Ok(());
    }
    for offer in &offers {
        print_offer(offer);
    }
    println!("\n{HASH_NOTICE}\n{SENDER_NOTICE}");
    Ok(())
}

fn print_offer(offer: &OfferView) {
    let id = offer.job_id.as_str();
    println!(
        "{}  {:?}  {}  {} bytes  from {}",
        &id[..id.len().min(8)],
        offer.state,
        offer.manifest.file_name,
        offer.manifest.size,
        offer.manifest.sender_label
    );
    println!("    sha256 {}", offer.manifest.sha256);
    if let Some(origin) = &offer.manifest.origin {
        println!("    link of the download it came from: {origin}");
    }
    if let Some(path) = &offer.saved_to {
        println!("    saved to {path}");
    }
    if let Some(reason) = &offer.reason {
        println!("    not copied: {reason}");
    }
}

fn accept(
    engine: &Engine,
    reference: &str,
    folder: Option<&str>,
    json: bool,
) -> Result<(), ProtocolError> {
    let found = find(engine, reference)?;
    engine.send(Command::AcceptOffer {
        offer_id: found.job_id.clone(),
        destination: folder.map(str::to_owned),
    })?;
    // The copy runs in the engine; wait briefly so the answer is the result.
    let deadline = Instant::now() + COPY_WAIT;
    let mut current = found;
    loop {
        current = inbox(engine)?
            .into_iter()
            .find(|offer| offer.job_id == current.job_id)
            .unwrap_or(current);
        if current.state != OfferState::Copying || Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    if json {
        println!("{}", serde_json::json!({ "offer": current }));
        return Ok(());
    }
    print_offer(&current);
    println!("{HASH_NOTICE}
{SENDER_NOTICE}");
    if current.state == OfferState::Copying {
        println!("Still copying; check `fetchpath inbox`.");
    }
    Ok(())
}

fn decline(engine: &Engine, reference: &str, json: bool) -> Result<(), ProtocolError> {
    let found = find(engine, reference)?;
    match engine.send(Command::DeclineOffer {
        offer_id: found.job_id,
    })? {
        CommandResult::Offer { offer } => {
            if json {
                println!("{}", serde_json::json!({ "offer": offer }));
            } else {
                print_offer(&offer);
                println!("{HASH_NOTICE}
{SENDER_NOTICE}");
            }
            Ok(())
        }
        other => Err(client::unexpected(&other)),
    }
}
