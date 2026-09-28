//! `fetchpath lan …` and `fetchpath fetch-verified`.
//!
//! State lives under `%LOCALAPPDATA%\Fetchpath`, or `FETCHPATH_DATA_DIR` when
//! set; the cache is the engine's, at [`cache_root`]. LAN mode is off until `lan enable` is run. Nothing here prints a
//! signing key, and the pairing code is printed only by `pair-host`, whose
//! whole purpose is to show it.

use fetchpath_core::fetchpath_cache::{CacheConfig, ContentCache, ContentId};
use fetchpath_core::{
    DeliverySource, MirrorSource, PeerOutcome, PeerSource, VerifiedDownloadRequest,
    download_verified_shared,
};
use fetchpath_lan::{
    DeviceIdentity, Dpapi, PairingCode, PeerClient, PeerKey, PeerServer, PinStore, UploadBudget,
    host_pairing, join_pairing,
};
use serde_json::{Value, json};
use std::fs;
use std::io;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const DEFAULT_PORT: u16 = 47631;
const CACHE_QUOTA_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const CACHE_MAX_ENTRY_BYTES: u64 = 1024 * 1024 * 1024;
const MAX_SESSIONS: usize = 4;

pub const USAGE: &str = "usage:
  fetchpath download URL DESTINATION
  fetchpath fetch-verified --sha256 HEX --size BYTES [--peer ADDRESS=KEY]... URL DESTINATION
  fetchpath lan id | enable | disable | peers
  fetchpath lan unpair KEY
  fetchpath lan pair-host [BIND]
  fetchpath lan pair-join ADDRESS CODE [LABEL]
  fetchpath lan serve [BIND]";

fn data_dir() -> Result<PathBuf, String> {
    if let Some(dir) = std::env::var_os("FETCHPATH_DATA_DIR") {
        return Ok(PathBuf::from(dir));
    }
    std::env::var_os("LOCALAPPDATA")
        .map(|base| PathBuf::from(base).join("Fetchpath"))
        .ok_or_else(|| "cli.no_data_dir".to_owned())
}

/// The content cache the engine fills and `lan serve` shares (FP-032):
/// `FETCHPATH_DATA_DIR\cache` when set, the engine's own folder when it was
/// moved with `FETCHPATH_APP_DATA_DIR`, and otherwise
/// `%LOCALAPPDATA%\app.fetchpath.desktop\cache`, local rather than roaming
/// and removed with the rest of Fetchpath's data when the person asks.
pub(crate) fn cache_root() -> Result<PathBuf, String> {
    if let Some(dir) = std::env::var_os("FETCHPATH_DATA_DIR") {
        return Ok(PathBuf::from(dir).join("cache"));
    }
    if let Ok(home) = fetchpath_protocol::launch::EngineHome::from_env()
        && !home.is_default()
    {
        return Ok(home.dir().join("cache"));
    }
    std::env::var_os("LOCALAPPDATA")
        .map(|base| {
            PathBuf::from(base)
                .join("app.fetchpath.desktop")
                .join("cache")
        })
        .ok_or_else(|| "cli.no_data_dir".to_owned())
}

fn lan_dir() -> Result<PathBuf, String> {
    Ok(data_dir()?.join("lan"))
}

fn flag_path() -> Result<PathBuf, String> {
    Ok(lan_dir()?.join("enabled"))
}

/// LAN mode is on only when the flag file says exactly `on`.
fn lan_enabled() -> bool {
    flag_path()
        .ok()
        .and_then(|path| fs::read_to_string(path).ok())
        .is_some_and(|text| text.trim() == "on")
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

fn bind_address(argument: Option<&String>) -> Result<SocketAddr, String> {
    let text = argument
        .cloned()
        .unwrap_or_else(|| format!("0.0.0.0:{DEFAULT_PORT}"));
    text.parse()
        .map_err(|_| format!("cli.invalid_address:{text}"))
}

fn io_error(prefix: &str) -> impl Fn(io::Error) -> String + '_ {
    move |error| format!("{prefix}:{error}")
}

pub fn run_lan(args: &[String]) -> Result<Value, String> {
    match args.first().map(String::as_str) {
        Some("id") => {
            let identity = identity()?;
            Ok(json!({
                "fingerprint": identity.fingerprint().to_string(),
                "key": identity.public_key().hex(),
                "lan_enabled": lan_enabled(),
            }))
        }
        Some(state @ ("enable" | "disable")) => {
            let path = flag_path()?;
            fs::create_dir_all(path.parent().expect("flag has a parent"))
                .map_err(io_error("lan.flag_unwritable"))?;
            let on = state == "enable";
            fs::write(&path, if on { "on" } else { "off" })
                .map_err(io_error("lan.flag_unwritable"))?;
            Ok(json!({ "lan_enabled": on }))
        }
        Some("peers") => {
            let peers: Vec<Value> = pins()?
                .peers()
                .into_iter()
                .map(|(key, label)| {
                    json!({
                        "fingerprint": key.fingerprint().to_string(),
                        "key": key.hex(),
                        "label": label,
                    })
                })
                .collect();
            Ok(json!({ "peers": peers }))
        }
        Some("unpair") => {
            let key = args
                .get(1)
                .and_then(|text| PeerKey::parse_hex(text))
                .ok_or_else(|| "cli.invalid_key".to_owned())?;
            let removed = pins()?
                .unpin(&key)
                .map_err(io_error("lan.peers_unwritable"))?;
            Ok(json!({ "unpaired": removed }))
        }
        Some("pair-host") => pair_host(bind_address(args.get(1))?),
        Some("pair-join") => {
            let (Some(address), Some(code)) = (args.get(1), args.get(2)) else {
                return Err(USAGE.to_owned());
            };
            let address: SocketAddr = address
                .parse()
                .map_err(|_| format!("cli.invalid_address:{address}"))?;
            let label = args.get(3).map(String::as_str).unwrap_or("paired device");
            let identity = identity()?;
            let mut pins = pins()?;
            let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(5))
                .map_err(io_error("lan.unreachable"))?;
            let host = join_pairing(&mut stream, &identity, code, &mut pins, label)
                .map_err(|error| error.to_string())?;
            Ok(json!({
                "paired": true,
                "fingerprint": host.fingerprint().to_string(),
                "key": host.hex(),
            }))
        }
        Some("serve") => serve(bind_address(args.get(1))?),
        _ => Err(USAGE.to_owned()),
    }
}

/// Shows a code and waits for one pairing attempt, at most until the code
/// expires. The first attempt spends the code whatever its outcome.
fn pair_host(bind: SocketAddr) -> Result<Value, String> {
    let identity = identity()?;
    let mut pins = pins()?;
    let listener = TcpListener::bind(bind).map_err(io_error("lan.bind_failed"))?;
    listener
        .set_nonblocking(true)
        .map_err(io_error("lan.bind_failed"))?;
    let mut code = PairingCode::generate().map_err(io_error("lan.random_unavailable"))?;
    println!(
        "{}",
        json!({
            "code": code.display(),
            "fingerprint": identity.fingerprint().to_string(),
            "listening": listener.local_addr().map(|a| a.to_string()).unwrap_or_default(),
            "expires_in_secs": fetchpath_lan::CODE_LIFETIME.as_secs(),
        })
    );
    let mut stream = loop {
        match listener.accept() {
            Ok((stream, _)) => break stream,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                if Instant::now() >= code.expires_at() {
                    return Err("pairing.code_expired".to_owned());
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(error) => return Err(format!("lan.accept_failed:{error}")),
        }
    };
    stream
        .set_nonblocking(false)
        .map_err(io_error("lan.accept_failed"))?;
    let joiner = host_pairing(
        &mut stream,
        &identity,
        &mut code,
        Instant::now(),
        &mut pins,
        "paired device",
    )
    .map_err(|error| error.to_string())?;
    Ok(json!({
        "paired": true,
        "fingerprint": joiner.fingerprint().to_string(),
        "key": joiner.hex(),
    }))
}

/// Serves paired peers until the process ends. Turning LAN mode off with
/// `lan disable` takes effect within two seconds, on the next request.
fn serve(bind: SocketAddr) -> Result<Value, String> {
    if !lan_enabled() {
        return Err("lan.disabled: run `fetchpath lan enable` first".to_owned());
    }
    let identity = identity()?;
    let enabled = Arc::new(AtomicBool::new(true));
    let pins = Arc::new(Mutex::new(pins()?));
    // `lan disable`, `lan unpair` and new pairings happen in other processes.
    // Re-reading both every two seconds bounds how long a revoked device keeps
    // access; the server also re-checks both on every request.
    let watched_flag = Arc::clone(&enabled);
    let watched_pins = Arc::clone(&pins);
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(Duration::from_secs(2));
            watched_flag.store(lan_enabled(), Ordering::SeqCst);
            if let Ok(mut pins) = watched_pins.lock() {
                // Fails closed: an unreadable list pins nobody.
                let _ = pins.reload();
            }
        }
    });
    let listener = TcpListener::bind(bind).map_err(io_error("lan.bind_failed"))?;
    eprintln!(
        "{}",
        json!({
            "serving": listener.local_addr().map(|a| a.to_string()).unwrap_or_default(),
            "fingerprint": identity.fingerprint().to_string(),
        })
    );
    let server = Arc::new(PeerServer::new(
        identity,
        pins,
        Arc::new(Mutex::new(cache()?)),
        enabled,
        UploadBudget::default(),
    ));
    server.run(listener, Arc::new(AtomicBool::new(false)), MAX_SESSIONS);
    Ok(json!({ "serving": false }))
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
