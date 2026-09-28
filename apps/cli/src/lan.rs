//! `fetchpath lan …`, through the engine (FP-033), and `fetchpath
//! fetch-verified`, which asks paired devices directly.
//!
//! The engine keeps paired devices and the sharing switch in [`lan_dir`] and
//! serves paired devices while sharing is on. Nothing here prints a signing
//! key; a pairing code is printed only by `pair`, whose whole purpose is to
//! show it.

use crate::client::{self, Engine};
use crate::download::EXIT_USAGE;
use fetchpath_core::fetchpath_cache::{CacheConfig, ContentCache, ContentId};
use fetchpath_core::{
    DeliverySource, MirrorSource, PeerOutcome, PeerSource, VerifiedDownloadRequest,
    download_verified_shared,
};
use fetchpath_lan::{DeviceIdentity, Dpapi, PeerClient, PeerKey, PinStore};
use fetchpath_protocol::ProtocolError;
use fetchpath_protocol::command::Command;
use fetchpath_protocol::message::CommandResult;
use fetchpath_protocol::model::{LanView, PairingState};
use serde_json::{Value, json};
use std::io;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

const CACHE_QUOTA_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const CACHE_MAX_ENTRY_BYTES: u64 = 1024 * 1024 * 1024;

pub const USAGE: &str = "usage: fetchpath lan [status] [--json]
  fetchpath lan on | off                  share with paired devices, or stop
  fetchpath lan pair                      show a code for another device to join
  fetchpath lan join ADDRESS CODE [NAME]  pair with a device showing a code
  fetchpath lan unpair KEY                remove a paired device
  fetchpath fetch-verified --sha256 HEX --size BYTES [--peer ADDRESS=KEY]... LINK DESTINATION";

/// Fetchpath's local data: `FETCHPATH_DATA_DIR` when set, the engine's own
/// folder when it was moved with `FETCHPATH_APP_DATA_DIR`, and otherwise
/// `%LOCALAPPDATA%\app.fetchpath.desktop`, local rather than roaming and
/// removed with the rest of the data when the person asks at uninstall.
pub(crate) fn data_dir() -> Result<PathBuf, String> {
    if let Some(dir) = std::env::var_os("FETCHPATH_DATA_DIR") {
        return Ok(PathBuf::from(dir));
    }
    if let Ok(home) = fetchpath_protocol::launch::EngineHome::from_env()
        && !home.is_default()
    {
        return Ok(home.dir().to_path_buf());
    }
    std::env::var_os("LOCALAPPDATA")
        .map(|base| PathBuf::from(base).join("app.fetchpath.desktop"))
        .ok_or_else(|| "cli.no_data_dir".to_owned())
}

/// The content cache the engine fills and shares (FP-032).
pub(crate) fn cache_root() -> Result<PathBuf, String> {
    Ok(data_dir()?.join("cache"))
}

/// Paired devices, this device's identity and the sharing switch.
pub(crate) fn lan_dir() -> Result<PathBuf, String> {
    Ok(data_dir()?.join("lan"))
}

fn identity() -> Result<Arc<DeviceIdentity>, String> {
    DeviceIdentity::load_or_create(&lan_dir()?.join("identity"), &Dpapi)
        .map(Arc::new)
        .map_err(|error| format!("lan.identity_unavailable:{error}"))
}

fn pins() -> Result<PinStore, String> {
    PinStore::open(&lan_dir()?.join("peers"))
        .map_err(|error| format!("lan.peers_unavailable:{error}"))
}

fn cache() -> Result<ContentCache, String> {
    ContentCache::open(
        &cache_root()?,
        CacheConfig::new(CACHE_QUOTA_BYTES, CACHE_MAX_ENTRY_BYTES),
    )
    .map_err(|error| format!("cache.unavailable:{error}"))
}

pub fn run(args: &[String]) -> i32 {
    let json = args.iter().any(|arg| arg == "--json");
    let words: Vec<&str> = args
        .iter()
        .map(String::as_str)
        .filter(|arg| *arg != "--json")
        .collect();
    let command = match words.as_slice() {
        [] | ["status"] => Command::LanStatus,
        ["on"] => Command::SetLanSharing { enabled: true },
        ["off"] => Command::SetLanSharing { enabled: false },
        ["pair"] => Command::StartPairing,
        ["join", address, code] | ["join", address, code, _] => Command::JoinPairing {
            address: (*address).to_owned(),
            code: (*code).to_owned(),
            label: words.get(3).map(|label| (*label).to_owned()),
        },
        ["unpair", key] => Command::Unpair {
            key: (*key).to_owned(),
        },
        _ => {
            eprintln!("{USAGE}");
            return EXIT_USAGE;
        }
    };
    let pairing = matches!(command, Command::StartPairing);
    match send(command, pairing, json) {
        Ok(()) => 0,
        Err(error) => client::fail(&error, json),
    }
}

fn send(command: Command, pairing: bool, json: bool) -> Result<(), ProtocolError> {
    let engine = Engine::connect()?;
    let result = engine.send(command)?;
    match result {
        CommandResult::Joined { device } => {
            if json {
                client::print_json(&CommandResult::Joined { device });
            } else {
                println!(
                    "Paired with {} ({}). Check that the other computer shows this computer's fingerprint.",
                    device.label, device.fingerprint
                );
            }
            Ok(())
        }
        CommandResult::Lan { lan } if pairing => wait_for_pairing(&engine, lan, json),
        CommandResult::Lan { lan } => {
            if json {
                client::print_json(&CommandResult::Lan { lan });
            } else {
                print_status(&lan);
            }
            Ok(())
        }
        other => Err(client::unexpected(&other)),
    }
}

fn print_status(lan: &LanView) {
    println!("This computer: {}", lan.fingerprint);
    match (&lan.serving, lan.sharing) {
        (Some(address), true) => println!("Sharing with paired devices at {address}."),
        (None, true) => println!(
            "Sharing is on but not running: {}",
            lan.problem
                .as_deref()
                .unwrap_or("the engine is not serving.")
        ),
        _ => println!("Sharing is off."),
    }
    if lan.devices.is_empty() {
        println!("No paired devices.");
    }
    for device in &lan.devices {
        println!("  {}  {}  {}", device.fingerprint, device.label, device.key);
    }
}

/// Shows the code, then waits for the other device or the code's end. With
/// `--json`, the code is printed as one line first so a script can pass it on.
fn wait_for_pairing(engine: &Engine, mut lan: LanView, json: bool) -> Result<(), ProtocolError> {
    if json {
        client::print_json(&CommandResult::Lan { lan: lan.clone() });
    } else if let Some(pairing) = &lan.pairing
        && let Some(code) = &pairing.code
    {
        println!("On the other computer, run:");
        println!("  fetchpath lan join {} {code}", pairing.address);
        println!("or enter that address and code in Settings, Paired devices.");
        println!("This computer's fingerprint: {}", lan.fingerprint);
        println!("The code works once, for two minutes. Waiting...");
    }
    loop {
        let Some(pairing) = lan.pairing.as_ref() else {
            return Ok(());
        };
        match pairing.state {
            PairingState::Waiting => {}
            PairingState::Paired => {
                if json {
                    client::print_json(&CommandResult::Lan { lan: lan.clone() });
                } else if let Some(device) = &pairing.device {
                    println!(
                        "Paired with {}. Check that the other computer shows {}.",
                        device.fingerprint, lan.fingerprint
                    );
                }
                return Ok(());
            }
            state => {
                let message = pairing.problem.clone().unwrap_or_else(|| match state {
                    PairingState::Expired => {
                        "The code expired before another device used it.".into()
                    }
                    _ => "Pairing stopped.".into(),
                });
                return Err(client::input_error(&message));
            }
        }
        std::thread::sleep(Duration::from_millis(500));
        lan = match engine.send(Command::LanStatus)? {
            CommandResult::Lan { lan } => lan,
            other => return Err(client::unexpected(&other)),
        };
    }
}

/// Adapts the LAN client to the core's peer interface. Every failure is
/// "unavailable": the core falls through to the next source either way.
struct LanPeer(PeerClient);

impl PeerSource for LanPeer {
    fn fingerprint(&self) -> [u8; 32] {
        self.0.peer().fingerprint().0
    }

    fn fetch(&self, id: &ContentId, ceiling: u64, into: &Path) -> io::Result<bool> {
        Ok(self.0.fetch(id, ceiling, into).is_ok())
    }
}

pub fn fetch_verified(args: &[String]) -> Result<Value, String> {
    let mut sha256 = None;
    let mut size = None;
    let mut peer_args = Vec::new();
    let mut positional = Vec::new();
    let mut rest = args.iter();
    while let Some(argument) = rest.next() {
        match argument.as_str() {
            "--sha256" => sha256 = rest.next().cloned(),
            "--size" => size = rest.next().and_then(|text| text.parse::<u64>().ok()),
            "--peer" => peer_args.push(rest.next().cloned().ok_or_else(|| USAGE.to_owned())?),
            _ => positional.push(argument.clone()),
        }
    }
    let (Some(sha256), Some(size), [url, destination]) = (sha256, size, positional.as_slice())
    else {
        return Err(USAGE.to_owned());
    };
    if ContentId::from_expected_sha256(&sha256).is_none() {
        return Err("cli.invalid_sha256".to_owned());
    }

    let mut peers = Vec::new();
    if !peer_args.is_empty() {
        let identity = identity()?;
        let pins = pins()?;
        for argument in &peer_args {
            let (address, key) = argument
                .split_once('=')
                .ok_or_else(|| format!("cli.invalid_peer:{argument}"))?;
            let address: SocketAddr = address
                .parse()
                .map_err(|_| format!("cli.invalid_address:{address}"))?;
            let key = PeerKey::parse_hex(key).ok_or_else(|| "cli.invalid_key".to_owned())?;
            // Only an explicitly paired device may be asked.
            if !pins.is_pinned(&key) {
                return Err(format!("lan.not_paired:{}", key.fingerprint()));
            }
            peers.push(LanPeer(PeerClient::new(
                Arc::clone(&identity),
                address,
                key,
            )));
        }
    }
    let peers: Vec<&dyn PeerSource> = peers.iter().map(|peer| peer as &dyn PeerSource).collect();

    let request = VerifiedDownloadRequest {
        expected_sha256: Some(sha256.to_ascii_lowercase()),
        expected_bytes: Some(size),
        ..VerifiedDownloadRequest::new(
            vec![MirrorSource::new(url.clone())],
            PathBuf::from(destination),
        )
    };
    let mut cache = cache()?;
    let done =
        download_verified_shared(request, &mut cache, &peers).map_err(|error| error.to_string())?;

    let (source, peer) = match done.source {
        DeliverySource::Network => ("network", None),
        DeliverySource::LocalCache => ("local_cache", None),
        DeliverySource::Peer { fingerprint } => (
            "peer",
            Some(fetchpath_lan::Fingerprint(fingerprint).to_string()),
        ),
    };
    let peer_outcomes: Vec<Value> = done
        .peers
        .iter()
        .map(|report| {
            json!({
                "fingerprint": fetchpath_lan::Fingerprint(report.fingerprint).to_string(),
                "outcome": match report.outcome {
                    PeerOutcome::Unused => "unused",
                    PeerOutcome::Delivered => "delivered",
                    PeerOutcome::Unavailable => "unavailable",
                    PeerOutcome::Corrupt => "corrupt",
                },
            })
        })
        .collect();
    Ok(json!({
        "result": "downloaded_verified",
        "destination": done.destination.display().to_string(),
        "bytes": done.bytes,
        "observed_sha256": done.observed_sha256,
        "verification": format!("{:?}", done.verification),
        "source": source,
        "peer": peer,
        "peers": peer_outcomes,
    }))
}
