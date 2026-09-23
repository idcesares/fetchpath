use fetchpath_cache::{CacheConfig, CachedVerification, ContentCache, ContentId, Provenance};
use fetchpath_lan::{
    DeviceIdentity, FetchError, PairingCode, PairingError, PeerClient, PeerServer, PinStore,
    SessionError, UploadBudget, connect_session, host_pairing, join_pairing, read_frame,
    write_frame,
};
use std::fs;
use std::io;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

static COUNTER: AtomicU32 = AtomicU32::new(0);

struct TempDir(PathBuf);

impl TempDir {
    fn new(label: &str) -> Self {
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "fetchpath-lan-{label}-{}-{unique}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("temp dir");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn loopback() -> (TcpListener, SocketAddr) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let address = listener.local_addr().expect("address");
    (listener, address)
}

fn identity() -> Arc<DeviceIdentity> {
    Arc::new(DeviceIdentity::generate().expect("identity"))
}

// ---------------------------------------------------------------- pairing

struct PairingRun {
    host: Result<fetchpath_lan::PeerKey, PairingError>,
    joiner: Result<fetchpath_lan::PeerKey, PairingError>,
    host_pins: PinStore,
    joiner_pins: PinStore,
}

fn pair(code: PairingCode, now: Instant, typed: impl FnOnce(&PairingCode) -> String) -> PairingRun {
    let (listener, address) = loopback();
    let host_identity = identity();
    let joiner_identity = identity();
    let typed = typed(&code);
    let host_side = {
        let host_identity = Arc::clone(&host_identity);
        thread::spawn(move || {
            let mut code = code;
            let mut pins = PinStore::in_memory();
            let (mut stream, _) = listener.accept().expect("accept");
            let result = host_pairing(
                &mut stream,
                &host_identity,
                &mut code,
                now,
                &mut pins,
                "joiner",
            );
            (result, pins)
        })
    };
    let mut joiner_pins = PinStore::in_memory();
    let mut stream = TcpStream::connect(address).expect("connect");
    let joiner = join_pairing(
        &mut stream,
        &joiner_identity,
        &typed,
        &mut joiner_pins,
        "host",
    );
    drop(stream);
    let (host, host_pins) = host_side.join().expect("host thread");
    PairingRun {
        host,
        joiner,
        host_pins,
        joiner_pins,
    }
}

#[test]
fn the_right_code_pairs_both_devices_and_pins_each_key() {
    let run = pair(PairingCode::generate().unwrap(), Instant::now(), |code| {
        code.display().to_lowercase()
    });
    let joiner_key = run.host.expect("host paired");
    let host_key = run.joiner.expect("joiner paired");
    assert!(run.host_pins.is_pinned(&joiner_key));
    assert!(run.joiner_pins.is_pinned(&host_key));
    assert_eq!(run.host_pins.label(&joiner_key), Some("joiner"));
}

#[test]
fn a_wrong_code_fails_on_both_sides_and_pins_nothing() {
    let run = pair(PairingCode::generate().unwrap(), Instant::now(), |code| {
        // Same shape, one character different.
        let mut shown: Vec<char> = code.display().chars().collect();
        shown[0] = if shown[0] == '0' { '1' } else { '0' };
        shown.into_iter().collect()
    });
    assert!(matches!(run.host, Err(PairingError::ConfirmationFailed)));
    assert!(matches!(run.joiner, Err(PairingError::ConfirmationFailed)));
    assert!(run.host_pins.peers().is_empty());
    assert!(run.joiner_pins.peers().is_empty());
}

#[test]
fn an_expired_code_fails_and_pins_nothing() {
    let issued = Instant::now()
        .checked_sub(Duration::from_secs(121))
        .expect("clock has run for two minutes");
    let run = pair(
        PairingCode::generate_at(issued).unwrap(),
        Instant::now(),
        |code| code.display(),
    );
    assert!(matches!(run.host, Err(PairingError::CodeExpired)));
    assert!(matches!(run.joiner, Err(PairingError::CodeExpired)));
    assert!(run.host_pins.peers().is_empty());
    assert!(run.joiner_pins.peers().is_empty());
}

#[test]
fn a_code_is_consumed_by_a_failed_attempt_and_cannot_be_retried() {
    let (listener, address) = loopback();
    let host_identity = identity();
    let mut code = PairingCode::generate().unwrap();
    let shown = code.display();
    let host_side = thread::spawn(move || {
        let mut pins = PinStore::in_memory();
        let mut outcomes = Vec::new();
        for _ in 0..2 {
            let (mut stream, _) = listener.accept().expect("accept");
            outcomes.push(host_pairing(
                &mut stream,
                &host_identity,
                &mut code,
                Instant::now(),
                &mut pins,
                "joiner",
            ));
        }
        (outcomes, pins)
    });

    let joiner_identity = identity();
    let mut pins = PinStore::in_memory();
    let wrong = if shown.starts_with('0') { "1" } else { "0" }.to_owned() + &shown[1..];
    let first = join_pairing(
        &mut TcpStream::connect(address).unwrap(),
        &joiner_identity,
        &wrong,
        &mut pins,
        "host",
    );
    // Now the right code: too late, the first attempt spent it.
    let second = join_pairing(
        &mut TcpStream::connect(address).unwrap(),
        &joiner_identity,
        &shown,
        &mut pins,
        "host",
    );
    let (outcomes, host_pins) = host_side.join().unwrap();
    assert!(matches!(first, Err(PairingError::ConfirmationFailed)));
    assert!(matches!(second, Err(PairingError::CodeAlreadyUsed)));
    assert!(matches!(outcomes[1], Err(PairingError::CodeAlreadyUsed)));
    assert!(host_pins.peers().is_empty());
    assert!(pins.peers().is_empty());
}

#[test]
fn a_malformed_typed_code_is_rejected_before_the_host_spends_its_code() {
    let (listener, address) = loopback();
    let host_identity = identity();
    let host_side = thread::spawn(move || {
        let mut code = PairingCode::generate().unwrap();
        let mut pins = PinStore::in_memory();
        let (mut stream, _) = listener.accept().expect("accept");
        let result = host_pairing(
            &mut stream,
            &host_identity,
            &mut code,
            Instant::now(),
            &mut pins,
            "joiner",
        );
        (result.is_err(), code.is_consumed())
    });
    let mut stream = TcpStream::connect(address).unwrap();
    let result = join_pairing(
        &mut stream,
        &identity(),
        "not-a-code",
        &mut PinStore::in_memory(),
        "host",
    );
    drop(stream);
    assert!(matches!(result, Err(PairingError::InvalidCode)));
    let (host_failed, consumed) = host_side.join().unwrap();
    assert!(host_failed, "the host saw a closed connection");
    assert!(!consumed, "no hello arrived, so the code is still usable");
}

// ---------------------------------------------------------------- serving

const PUBLIC: ContentId = ContentId::FlatSha256([1; 32]);
const PRIVATE: ContentId = ContentId::FlatSha256([2; 32]);
const ABSENT: ContentId = ContentId::FlatSha256([3; 32]);

struct Fixture {
    _dir: TempDir,
    dir: PathBuf,
    server_identity: Arc<DeviceIdentity>,
    client_identity: Arc<DeviceIdentity>,
    cache: Arc<Mutex<ContentCache>>,
    pins: Arc<Mutex<PinStore>>,
    enabled: Arc<AtomicBool>,
    public_bytes: Vec<u8>,
}

impl Fixture {
    fn new(label: &str, size: usize) -> Self {
        let dir = TempDir::new(label);
        let path = dir.path().to_path_buf();
        let mut cache = ContentCache::open(&path.join("cache"), CacheConfig::new(1 << 24, 1 << 24))
            .expect("cache");
        let public_bytes: Vec<u8> = (0..size).map(|index| (index * 31 % 251) as u8).collect();
        let source = path.join("public.bin");
        fs::write(&source, &public_bytes).unwrap();
        cache
            .insert(
                &PUBLIC,
                &source,
                CachedVerification::FinalHashOnly,
                Provenance::Public,
            )
            .unwrap();
        let private = path.join("private.bin");
        fs::write(&private, b"signed-url bytes").unwrap();
        cache
            .insert(
                &PRIVATE,
                &private,
                CachedVerification::FinalHashOnly,
                Provenance::Credentialed,
            )
            .unwrap();

        let client_identity = identity();
        let mut pins = PinStore::in_memory();
        pins.pin(client_identity.public_key(), "client").unwrap();
        Self {
            _dir: dir,
            dir: path,
            server_identity: identity(),
            client_identity,
            cache: Arc::new(Mutex::new(cache)),
            pins: Arc::new(Mutex::new(pins)),
            enabled: Arc::new(AtomicBool::new(true)),
            public_bytes,
        }
    }

    fn server(&self, budget: UploadBudget) -> PeerServer {
        PeerServer::new(
            Arc::clone(&self.server_identity),
            Arc::clone(&self.pins),
            Arc::clone(&self.cache),
            Arc::clone(&self.enabled),
            budget,
        )
    }

    /// Serves exactly one connection on a background thread.
    fn serve_once(
        &self,
        budget: UploadBudget,
    ) -> (
        SocketAddr,
        thread::JoinHandle<Result<fetchpath_lan::SessionSummary, SessionError>>,
    ) {
        let (listener, address) = loopback();
        let server = self.server(budget);
        let handle = thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept");
            server.serve_connection(stream)
        });
        (address, handle)
    }

    fn client(&self, address: SocketAddr) -> PeerClient {
        PeerClient::new(
            Arc::clone(&self.client_identity),
            address,
            self.server_identity.public_key(),
        )
    }
}

fn unpaced() -> UploadBudget {
    UploadBudget {
        session_bytes: 1 << 30,
        bytes_per_second: 0,
    }
}

#[test]
fn a_paired_peer_retrieves_a_public_entry_byte_for_byte() {
    // Larger than one chunk, and not a multiple of it.
    let fixture = Fixture::new("retrieve", 200_000);
    let (address, server) = fixture.serve_once(unpaced());
    let into = fixture.dir.join("received.bin");
    let bytes = fixture
        .client(address)
        .fetch(&PUBLIC, 1 << 20, &into)
        .expect("fetched");
    assert_eq!(bytes, 200_000);
    assert_eq!(fs::read(&into).unwrap(), fixture.public_bytes);
    let summary = server.join().unwrap().expect("served");
    assert_eq!(summary.served, vec![PUBLIC]);
    assert_eq!(summary.bytes_sent, 200_000);
    assert!(
        !fixture.cache.lock().unwrap().is_pinned(&PUBLIC),
        "the upload released its pin"
    );
}

#[test]
fn a_credentialed_entry_is_refused_exactly_as_an_absent_one_is() {
    let fixture = Fixture::new("private", 1000);
    for id in [PRIVATE, ABSENT] {
        let (address, server) = fixture.serve_once(unpaced());
        let into = fixture.dir.join("never.bin");
        let result = fixture.client(address).fetch(&id, 1 << 20, &into);
        assert!(matches!(result, Err(FetchError::NotAvailable)), "{id:?}");
        assert!(!into.exists(), "no file was created");
        let summary = server.join().unwrap().expect("session ended cleanly");
        assert_eq!(summary.bytes_sent, 0);
        assert_eq!(summary.refused, 1);
    }
}

/// Reads every frame a server sends until it closes the connection.
fn drain(stream: &mut TcpStream) -> Vec<(u8, Vec<u8>)> {
    let mut frames = Vec::new();
    while let Ok(frame) = read_frame(stream) {
        frames.push(frame);
    }
    frames
}

fn raw_hello(identity: [u8; 32]) -> Vec<u8> {
    let mut hello = vec![1_u8];
    hello.extend_from_slice(&[9; 32]); // any 32 bytes are an X25519 public key
    hello.extend_from_slice(&identity);
    hello.extend_from_slice(&[7; 32]);
    hello
}

#[test]
fn an_unpaired_device_is_refused_before_a_single_byte_is_served() {
    let fixture = Fixture::new("unpaired", 1000);
    let (address, server) = fixture.serve_once(unpaced());
    let stranger = DeviceIdentity::generate().unwrap();
    let mut stream = TcpStream::connect(address).unwrap();
    write_frame(
        &mut stream,
        1,
        &raw_hello(*stranger.public_key().as_bytes()),
    )
    .unwrap();
    let frames = drain(&mut stream);
    // One refusal and nothing else: not even the server's hello.
    assert_eq!(frames, vec![(4_u8, b"not_paired".to_vec())]);
    assert!(matches!(
        server.join().unwrap(),
        Err(SessionError::NotPaired)
    ));

    // And through the client API.
    let (address, server) = fixture.serve_once(unpaced());
    let outsider = PeerClient::new(
        Arc::new(stranger),
        address,
        fixture.server_identity.public_key(),
    );
    let result = outsider.fetch(&PUBLIC, 1 << 20, &fixture.dir.join("x"));
    assert!(matches!(
        result,
        Err(FetchError::Session(SessionError::NotPaired))
    ));
    assert!(matches!(
        server.join().unwrap(),
        Err(SessionError::NotPaired)
    ));
}

#[test]
fn copying_a_pinned_public_key_without_its_private_key_fails_authentication() {
    let fixture = Fixture::new("impostor", 1000);
    let (address, server) = fixture.serve_once(unpaced());
    let mut stream = TcpStream::connect(address).unwrap();
    let pinned = *fixture.client_identity.public_key().as_bytes();
    write_frame(&mut stream, 1, &raw_hello(pinned)).unwrap();
    let (kind, _) = read_frame(&mut stream).expect("server hello");
    assert_eq!(kind, 1);
    write_frame(&mut stream, 3, &[0x55; 64]).unwrap();
    let frames = drain(&mut stream);
    assert_eq!(frames, vec![(4_u8, b"authentication_failed".to_vec())]);
    assert!(matches!(
        server.join().unwrap(),
        Err(SessionError::AuthenticationFailed)
    ));
}

#[test]
fn a_server_presenting_an_unexpected_key_is_refused_by_the_client() {
    let fixture = Fixture::new("unexpected", 1000);
    let (address, _server) = fixture.serve_once(unpaced());
    let someone_else = DeviceIdentity::generate().unwrap().public_key();
    let client = PeerClient::new(Arc::clone(&fixture.client_identity), address, someone_else);
    let result = client.fetch(&PUBLIC, 1 << 20, &fixture.dir.join("x"));
    assert!(matches!(
        result,
        Err(FetchError::Session(SessionError::UnexpectedPeer))
    ));
}

#[test]
fn with_lan_mode_off_the_server_answers_nothing() {
    let fixture = Fixture::new("disabled", 1000);
    fixture.enabled.store(false, Ordering::SeqCst);
    let (address, server) = fixture.serve_once(unpaced());
    let mut stream = TcpStream::connect(address).unwrap();
    // The server may already have closed the socket, so the write itself can
    // fail; what matters is that nothing ever comes back.
    let _ = write_frame(
        &mut stream,
        1,
        &raw_hello(*fixture.client_identity.public_key().as_bytes()),
    );
    assert!(drain(&mut stream).is_empty());
    assert!(server.join().unwrap().is_err());
}

#[test]
fn the_session_budget_refuses_a_request_before_sending_any_of_it() {
    let fixture = Fixture::new("budget", 10_000);
    let (address, server) = fixture.serve_once(UploadBudget {
        session_bytes: 9_999,
        bytes_per_second: 0,
    });
    let into = fixture.dir.join("x");
    let result = fixture.client(address).fetch(&PUBLIC, 1 << 20, &into);
    assert!(matches!(result, Err(FetchError::BudgetExhausted)));
    assert!(!into.exists());
    assert_eq!(server.join().unwrap().unwrap().bytes_sent, 0);
}

#[test]
fn the_upload_rate_is_paced() {
    let fixture = Fixture::new("paced", 256 * 1024);
    let (address, server) = fixture.serve_once(UploadBudget {
        session_bytes: 1 << 30,
        bytes_per_second: 1024 * 1024,
    });
    let started = Instant::now();
    fixture
        .client(address)
        .fetch(&PUBLIC, 1 << 20, &fixture.dir.join("x"))
        .expect("fetched");
    let elapsed = started.elapsed();
    server.join().unwrap().unwrap();
    // 256 KiB at 1 MiB/s is 250 ms. Only the lower bound is meaningful; an
    // upper bound would measure the machine rather than the pacer.
    assert!(elapsed >= Duration::from_millis(240), "{elapsed:?}");
}

#[test]
fn an_offer_larger_than_the_receivers_ceiling_is_abandoned_unread() {
    let fixture = Fixture::new("oversize", 5000);
    let (address, _server) = fixture.serve_once(unpaced());
    let into = fixture.dir.join("x");
    let result = fixture.client(address).fetch(&PUBLIC, 4999, &into);
    assert!(matches!(
        result,
        Err(FetchError::Oversize {
            offered: 5000,
            ceiling: 4999
        })
    ));
    assert!(!into.exists());
}

#[test]
fn a_tampered_frame_in_transit_fails_authentication() {
    let fixture = Fixture::new("tamper", 100_000);
    let (server_address, _server) = fixture.serve_once(unpaced());
    // A relay that flips one bit in the first content frame from the server.
    let (relay, relay_address) = loopback();
    thread::spawn(move || {
        let (mut client_side, _) = relay.accept().unwrap();
        let mut server_side = TcpStream::connect(server_address).unwrap();
        let mut upstream_client = client_side.try_clone().unwrap();
        let mut upstream_server = server_side.try_clone().unwrap();
        thread::spawn(move || {
            while let Ok((kind, payload)) = read_frame(&mut upstream_client) {
                if write_frame(&mut upstream_server, kind, &payload).is_err() {
                    break;
                }
            }
        });
        let mut flipped = false;
        while let Ok((kind, mut payload)) = read_frame(&mut server_side) {
            if kind == 12 && !flipped {
                payload[0] ^= 1;
                flipped = true;
            }
            if write_frame(&mut client_side, kind, &payload).is_err() {
                break;
            }
        }
    });
    let result = fixture
        .client(relay_address)
        .fetch(&PUBLIC, 1 << 20, &fixture.dir.join("x"));
    match result {
        Err(FetchError::Io(error)) => assert_eq!(error.kind(), io::ErrorKind::InvalidData),
        other => panic!("expected an authentication failure, got {other:?}"),
    }
}

#[test]
fn a_hostile_frame_length_is_refused_under_bounded_memory() {
    let fixture = Fixture::new("hostile", 1000);
    let (address, server) = fixture.serve_once(unpaced());
    let mut stream = TcpStream::connect(address).unwrap();
    use std::io::Write;
    stream.write_all(&[1, 0xff, 0xff, 0xff, 0xff]).unwrap();
    match server.join().unwrap() {
        Err(SessionError::Io(error)) => assert_eq!(error.kind(), io::ErrorKind::InvalidData),
        other => panic!("expected a refused frame, got {other:?}"),
    }
}

#[test]
fn turning_lan_mode_off_takes_effect_within_a_live_session() {
    let fixture = Fixture::new("toggle", 1000);
    let (address, server) = fixture.serve_once(unpaced());
    let mut channel = connect_session(
        TcpStream::connect(address).unwrap(),
        &fixture.client_identity,
        &fixture.server_identity.public_key(),
    )
    .expect("session");
    fixture.enabled.store(false, Ordering::SeqCst);
    channel.send(10, PUBLIC.render().as_bytes()).unwrap();
    let (kind, reason) = channel.receive().unwrap();
    assert_eq!((kind, reason.as_slice()), (4, b"not_available".as_slice()));
    channel.send(14, &[]).unwrap();
    assert_eq!(server.join().unwrap().unwrap().bytes_sent, 0);
}

#[test]
fn unpairing_a_device_takes_effect_within_its_live_session() {
    let fixture = Fixture::new("unpair-live", 1000);
    let (address, server) = fixture.serve_once(unpaced());
    let mut channel = connect_session(
        TcpStream::connect(address).unwrap(),
        &fixture.client_identity,
        &fixture.server_identity.public_key(),
    )
    .expect("session");
    // Served while paired.
    channel.send(10, PUBLIC.render().as_bytes()).unwrap();
    assert_eq!(channel.receive().unwrap().0, 11, "offer");
    loop {
        if channel.receive().unwrap().0 == 13 {
            break;
        }
    }
    // Unpaired mid-session: the next request is refused like any other.
    fixture
        .pins
        .lock()
        .unwrap()
        .unpin(&fixture.client_identity.public_key())
        .unwrap();
    channel.send(10, PUBLIC.render().as_bytes()).unwrap();
    let (kind, reason) = channel.receive().unwrap();
    assert_eq!((kind, reason.as_slice()), (4, b"not_available".as_slice()));
    channel.send(14, &[]).unwrap();
    let summary = server.join().unwrap().unwrap();
    assert_eq!(summary.served.len(), 1);
    assert_eq!(summary.refused, 1);
}

// ---------------------------------------------------------------- identity

#[cfg(windows)]
#[test]
fn a_sealed_identity_reloads_as_the_same_key_and_is_not_stored_in_the_clear() {
    use fetchpath_lan::Dpapi;
    let dir = TempDir::new("identity");
    let path = dir.path().join("identity");
    let first = DeviceIdentity::load_or_create(&path, &Dpapi).expect("create");
    let again = DeviceIdentity::load_or_create(&path, &Dpapi).expect("reload");
    assert_eq!(first.public_key(), again.public_key());
    assert_ne!(
        fs::read(&path).unwrap().len(),
        32,
        "a DPAPI blob, not a raw seed"
    );
}

#[cfg(windows)]
#[test]
fn an_unreadable_sealed_identity_is_an_error_not_a_silent_replacement() {
    use fetchpath_lan::Dpapi;
    let dir = TempDir::new("identity-bad");
    let path = dir.path().join("identity");
    fs::write(&path, b"not a dpapi blob").unwrap();
    assert!(DeviceIdentity::load_or_create(&path, &Dpapi).is_err());
    assert_eq!(
        fs::read(&path).unwrap(),
        b"not a dpapi blob",
        "left untouched"
    );
}

#[test]
fn a_running_server_serves_an_entry_another_process_inserted_after_it_started() {
    let fixture = Fixture::new("late-insert", 1000);
    // A second handle on the same store stands in for another process, such
    // as a download finishing while `lan serve` runs.
    let root = fixture.dir.join("cache");
    let mut other = ContentCache::open(&root, CacheConfig::new(1 << 24, 1 << 24)).unwrap();
    let late = ContentId::FlatSha256([9; 32]);
    let source = fixture.dir.join("late.bin");
    fs::write(&source, b"arrived later").unwrap();
    other
        .insert(
            &late,
            &source,
            CachedVerification::FinalHashOnly,
            Provenance::Public,
        )
        .unwrap();

    let (address, server) = fixture.serve_once(unpaced());
    let into = fixture.dir.join("late-received.bin");
    fixture
        .client(address)
        .fetch(&late, 1 << 20, &into)
        .expect("served from the refreshed view");
    assert_eq!(fs::read(&into).unwrap(), b"arrived later");
    server.join().unwrap().unwrap();
}
