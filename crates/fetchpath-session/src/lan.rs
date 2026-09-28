//! Paired devices and LAN sharing, owned by the engine (FP-033).
//!
//! Sharing is off until the person turns it on. While it is on the engine
//! answers paired devices from the content cache, and only with entries
//! recorded `Public`; `fetchpath-lan` refuses everything else exactly as it
//! refuses an absent entry. Pairing shows a single-use code for two minutes
//! and pins the one device that proves it holds it. State lives beside the
//! cache: the `enabled` flag, the sealed identity and the pinned devices.

use crate::wire;
use fetchpath_core::fetchpath_cache::{CacheConfig, ContentCache};
use fetchpath_lan::{
    DeviceIdentity, Dpapi, PairingCode, PairingError, PeerKey, PeerServer, PinStore, UploadBudget,
    host_pairing, join_pairing,
};
use fetchpath_protocol::model::{LanView, PairedDevice, PairingState, PairingView};
use std::fs;
use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

/// Where paired devices ask for files.
pub const SERVE_PORT: u16 = 47631;
/// Where a device showing a code waits for the other one.
pub const PAIRING_PORT: u16 = 47632;
const MAX_SESSIONS: usize = 4;
const MAX_LABEL: usize = 64;

pub struct Lan {
    dir: PathBuf,
    cache_root: PathBuf,
    /// `0.0.0.0` unless `FETCHPATH_LAN_BIND` names another address, which
    /// tests use to stay on loopback.
    bind: IpAddr,
    identity: OnceLock<Result<Arc<DeviceIdentity>, String>>,
    pins: OnceLock<Arc<Mutex<PinStore>>>,
    enabled: Arc<AtomicBool>,
    server: Mutex<Option<Server>>,
    problem: Mutex<Option<String>>,
    pairing: Arc<Mutex<Option<Pairing>>>,
}

struct Server {
    address: SocketAddr,
    stop: Arc<AtomicBool>,
}

struct Pairing {
    code: Option<String>,
    address: String,
    expires_at: SystemTime,
    state: PairingState,
    device: Option<PairedDevice>,
    problem: Option<String>,
    cancel: Arc<AtomicBool>,
}

impl Lan {
    pub fn new(dir: PathBuf, cache_root: PathBuf) -> Self {
        let bind = std::env::var("FETCHPATH_LAN_BIND")
            .ok()
            .and_then(|text| text.parse().ok())
            .unwrap_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED));
        let lan = Self {
            dir,
            cache_root,
            bind,
            identity: OnceLock::new(),
            pins: OnceLock::new(),
            enabled: Arc::new(AtomicBool::new(false)),
            server: Mutex::new(None),
            problem: Mutex::new(None),
            pairing: Arc::new(Mutex::new(None)),
        };
        lan.enabled.store(lan.flag_on(), Ordering::SeqCst);
        lan
    }

    fn flag_path(&self) -> PathBuf {
        self.dir.join("enabled")
    }

    /// On only when the flag file says exactly `on`.
    fn flag_on(&self) -> bool {
        fs::read_to_string(self.flag_path()).is_ok_and(|text| text.trim() == "on")
    }

    fn identity(&self) -> Result<Arc<DeviceIdentity>, String> {
        self.identity
            .get_or_init(|| {
                DeviceIdentity::load_or_create(&self.dir.join("identity"), &Dpapi)
                    .map(Arc::new)
                    .map_err(|error| format!("This computer's pairing key cannot be read: {error}"))
            })
            .clone()
    }

    fn pins(&self) -> Result<Arc<Mutex<PinStore>>, String> {
        if let Some(pins) = self.pins.get() {
            return Ok(Arc::clone(pins));
        }
        let store = PinStore::open(&self.dir.join("peers"))
            .map_err(|error| format!("The list of paired devices cannot be read: {error}"))?;
        Ok(Arc::clone(
            self.pins.get_or_init(|| Arc::new(Mutex::new(store))),
        ))
    }

    /// Starts serving if sharing was left on.
    pub fn resume(&self) {
        if self.enabled.load(Ordering::SeqCst) {
            self.start_serving();
        }
    }

    /// True while the engine should stay up for other devices: sharing is
    /// served, or a code is waiting.
    pub fn busy(&self) -> bool {
        self.server.lock().expect("lan poisoned").is_some()
            || self
                .pairing
                .lock()
                .expect("lan poisoned")
                .as_ref()
                .is_some_and(|pairing| pairing.state == PairingState::Waiting)
    }

    pub fn status(&self) -> Result<LanView, String> {
        let identity = self.identity()?;
        let devices = self
            .pins()?
            .lock()
            .expect("pins poisoned")
            .peers()
            .into_iter()
            .map(|(key, label)| device(&key, &label))
            .collect();
        let serving = self
            .server
            .lock()
            .expect("lan poisoned")
            .as_ref()
            .map(|server| shown_address(server.address));
        let pairing = self
            .pairing
            .lock()
            .expect("lan poisoned")
            .as_mut()
            .map(|pairing| {
                if pairing.state == PairingState::Waiting && SystemTime::now() >= pairing.expires_at
                {
                    pairing.state = PairingState::Expired;
                    pairing.code = None;
                }
                PairingView {
                    state: pairing.state,
                    code: pairing.code.clone(),
                    address: pairing.address.clone(),
                    expires_at: wire::timestamp(
                        pairing
                            .expires_at
                            .duration_since(SystemTime::UNIX_EPOCH)
                            .map_or(0, |elapsed| elapsed.as_millis() as u64),
                    ),
                    device: pairing.device.clone(),
                    problem: pairing.problem.clone(),
                }
            });
        Ok(LanView {
            sharing: self.enabled.load(Ordering::SeqCst),
            serving,
            problem: self.problem.lock().expect("lan poisoned").clone(),
            fingerprint: identity.fingerprint().to_string(),
            devices,
            pairing,
        })
    }

    pub fn set_sharing(&self, on: bool) -> Result<LanView, String> {
        fs::create_dir_all(&self.dir)
            .and_then(|()| fs::write(self.flag_path(), if on { "on" } else { "off" }))
            .map_err(|error| format!("The sharing setting cannot be saved: {error}"))?;
        // The server checks the flag on every request, so off takes effect
        // for a device already connected too.
        self.enabled.store(on, Ordering::SeqCst);
        if on {
            self.start_serving();
        } else {
            self.stop_serving();
        }
        self.status()
    }

    fn start_serving(&self) {
        let mut server = self.server.lock().expect("lan poisoned");
        if server.is_some() {
            return;
        }
        let started = (|| {
            let identity = self.identity()?;
            let pins = self.pins()?;
            let cache = ContentCache::open(&self.cache_root, CacheConfig::new(u64::MAX, u64::MAX))
                .map_err(|error| format!("The cache cannot be read: {error}"))?;
            let listener = TcpListener::bind(SocketAddr::new(self.bind, SERVE_PORT))
                .map_err(|error| format!("Port {SERVE_PORT} could not be opened: {error}"))?;
            let address = listener
                .local_addr()
                .map_err(|error| format!("Port {SERVE_PORT} could not be opened: {error}"))?;
            let stop = Arc::new(AtomicBool::new(false));
            let peer_server = Arc::new(PeerServer::new(
                identity,
                pins,
                Arc::new(Mutex::new(cache)),
                Arc::clone(&self.enabled),
                UploadBudget::default(),
            ));
            let stopping = Arc::clone(&stop);
            std::thread::spawn(move || peer_server.run(listener, stopping, MAX_SESSIONS));
            Ok::<_, String>(Server { address, stop })
        })();
        match started {
            Ok(started) => {
                *server = Some(started);
                *self.problem.lock().expect("lan poisoned") = None;
            }
            Err(problem) => *self.problem.lock().expect("lan poisoned") = Some(problem),
        }
    }

    /// Stops accepting. A transfer in flight is refused at its next request,
    /// because the flag is already off or the engine is leaving.
    pub fn stop_serving(&self) {
        *self.problem.lock().expect("lan poisoned") = None;
        if let Some(server) = self.server.lock().expect("lan poisoned").take() {
            server.stop.store(true, Ordering::SeqCst);
            // The accept loop checks the flag when a connection arrives.
            let _ =
                TcpStream::connect_timeout(&wake_address(server.address), Duration::from_secs(1));
        }
    }

    /// Shows a new code and waits for one device, replacing any code shown.
    pub fn start_pairing(&self) -> Result<LanView, String> {
        self.cancel_pairing();
        let identity = self.identity()?;
        let pins = self.pins()?;
        // A code withdrawn a moment ago releases the port within 100 ms.
        let deadline = Instant::now() + Duration::from_secs(1);
        let listener = loop {
            match TcpListener::bind(SocketAddr::new(self.bind, PAIRING_PORT)) {
                Ok(listener) => break listener,
                Err(_) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(error) => {
                    return Err(format!("Port {PAIRING_PORT} could not be opened: {error}"));
                }
            }
        };
        let local = listener
            .local_addr()
            .and_then(|address| listener.set_nonblocking(true).map(|()| address))
            .map_err(|error| format!("Port {PAIRING_PORT} could not be opened: {error}"))?;
        let mut code = PairingCode::generate()
            .map_err(|error| format!("A pairing code could not be made: {error}"))?;
        let expires_at = SystemTime::now() + fetchpath_lan::CODE_LIFETIME;
        let cancel = Arc::new(AtomicBool::new(false));
        *self.pairing.lock().expect("lan poisoned") = Some(Pairing {
            code: Some(code.display()),
            address: shown_address(local),
            expires_at,
            state: PairingState::Waiting,
            device: None,
            problem: None,
            cancel: Arc::clone(&cancel),
        });
        let shared = Arc::clone(&self.pairing);
        std::thread::spawn(move || {
            let outcome = host_one(&listener, &identity, &mut code, &pins, &cancel);
            let mut guard = shared.lock().expect("lan poisoned");
            let Some(pairing) = guard.as_mut().filter(|p| Arc::ptr_eq(&p.cancel, &cancel)) else {
                return;
            };
            pairing.code = None;
            match outcome {
                Hosted::Paired(device) => {
                    pairing.state = PairingState::Paired;
                    pairing.device = Some(device);
                }
                Hosted::Expired => pairing.state = PairingState::Expired,
                Hosted::Cancelled => pairing.state = PairingState::Cancelled,
                Hosted::Failed(problem) => {
                    pairing.state = PairingState::Failed;
                    pairing.problem = Some(problem);
                }
            }
        });
        self.status()
    }

    pub fn cancel_pairing(&self) {
        if let Some(pairing) = self.pairing.lock().expect("lan poisoned").as_mut() {
            pairing.cancel.store(true, Ordering::SeqCst);
            if pairing.state == PairingState::Waiting {
                pairing.state = PairingState::Cancelled;
                pairing.code = None;
            }
        }
    }

    /// Pairs with the device showing `code` at `address`.
    pub fn join(
        &self,
        address: &str,
        code: &str,
        label: Option<&str>,
    ) -> Result<PairedDevice, String> {
        let target = parse_address(address)?;
        let label = clean_label(label.unwrap_or("paired device"));
        let identity = self.identity()?;
        let pins = self.pins()?;
        let mut stream = TcpStream::connect_timeout(&target, Duration::from_secs(5))
            .map_err(|error| format!("{address} could not be reached: {error}. Check the address and that the other computer is showing a code."))?;
        let mut pins = pins.lock().expect("pins poisoned");
        let key = join_pairing(&mut stream, &identity, code, &mut pins, &label)
            .map_err(|error| plain(&error))?;
        Ok(device(&key, &label))
    }

    pub fn unpair(&self, key: &str) -> Result<LanView, String> {
        let key = PeerKey::parse_hex(key.trim())
            .ok_or_else(|| "That is not a paired device's key.".to_string())?;
        self.pins()?
            .lock()
            .expect("pins poisoned")
            .unpin(&key)
            .map_err(|error| format!("The device could not be removed: {error}"))?;
        self.status()
    }
}

enum Hosted {
    Paired(PairedDevice),
    Expired,
    Cancelled,
    Failed(String),
}

/// Waits for one connection until the code expires, then runs the host side.
/// The first attempt spends the code whatever its outcome.
fn host_one(
    listener: &TcpListener,
    identity: &DeviceIdentity,
    code: &mut PairingCode,
    pins: &Mutex<PinStore>,
    cancel: &AtomicBool,
) -> Hosted {
    let mut stream = loop {
        if cancel.load(Ordering::SeqCst) {
            return Hosted::Cancelled;
        }
        match listener.accept() {
            Ok((stream, _)) => break stream,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                if Instant::now() >= code.expires_at() {
                    return Hosted::Expired;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(error) => return Hosted::Failed(format!("The connection failed: {error}")),
        }
    };
    if stream.set_nonblocking(false).is_err() {
        return Hosted::Failed("The connection failed.".into());
    }
    let label = "paired device";
    let mut pins = pins.lock().expect("pins poisoned");
    match host_pairing(
        &mut stream,
        identity,
        code,
        Instant::now(),
        &mut pins,
        label,
    ) {
        Ok(key) => Hosted::Paired(device(&key, label)),
        Err(PairingError::CodeExpired) => Hosted::Expired,
        Err(error) => Hosted::Failed(plain(&error)),
    }
}

fn device(key: &PeerKey, label: &str) -> PairedDevice {
    PairedDevice {
        key: key.hex(),
        fingerprint: key.fingerprint().to_string(),
        label: label.to_owned(),
    }
}

fn plain(error: &PairingError) -> String {
    match error {
        PairingError::InvalidCode => {
            "That code is not ten letters and digits from the other computer's pairing code.".into()
        }
        PairingError::CodeExpired => {
            "The code expired. Show a new one on the other computer.".into()
        }
        PairingError::CodeAlreadyUsed => {
            "That code was already used. Show a new one on the other computer.".into()
        }
        PairingError::ConfirmationFailed => {
            "The codes did not match, so nothing was paired. Show a new code and try again.".into()
        }
        PairingError::Refused(_) => {
            "The other computer refused. Show a new code there and try again.".into()
        }
        PairingError::Io(error) => format!("The connection failed: {error}"),
    }
}

fn clean_label(label: &str) -> String {
    let label: String = label
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_LABEL)
        .collect();
    let label = label.trim();
    if label.is_empty() {
        "paired device".into()
    } else {
        label.into()
    }
}

/// `host:port`, or a bare host on the pairing port.
fn parse_address(text: &str) -> Result<SocketAddr, String> {
    use std::net::ToSocketAddrs;
    let text = text.trim();
    let with_port = if text.parse::<SocketAddr>().is_ok()
        || text
            .rsplit_once(':')
            .is_some_and(|(_, port)| port.parse::<u16>().is_ok())
    {
        text.to_owned()
    } else {
        format!("{text}:{PAIRING_PORT}")
    };
    with_port
        .to_socket_addrs()
        .ok()
        .and_then(|mut addresses| addresses.next())
        .ok_or_else(|| format!("{text} is not an address. Type what the other computer shows, such as 192.168.1.20:{PAIRING_PORT}."))
}

/// The address to show: a wildcard bind becomes this computer's address on
/// its default route, found without sending anything.
fn shown_address(address: SocketAddr) -> String {
    if !address.ip().is_unspecified() {
        return address.to_string();
    }
    UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))
        .and_then(|socket| {
            socket
                .connect((Ipv4Addr::new(192, 0, 2, 1), 9))
                .map(|()| socket)
        })
        .and_then(|socket| socket.local_addr())
        .map(|local| SocketAddr::new(local.ip(), address.port()).to_string())
        .unwrap_or_else(|_| format!("this computer's address, port {}", address.port()))
}

fn wake_address(address: SocketAddr) -> SocketAddr {
    if address.ip().is_unspecified() {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), address.port())
    } else {
        address
    }
}
