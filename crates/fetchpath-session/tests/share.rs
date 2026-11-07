//! Share links (FP-117, gates X1 to X6): nothing is shared before the setup
//! test passed, only the person makes links, the link is shown once, and a
//! file with a login behind it is never linked.

use fetchpath_protocol::command::{
    Command, ConflictPolicy, DestinationIntent, JobInput, JobRequest,
};
use fetchpath_protocol::message::CommandResult;
use fetchpath_protocol::model::{ShareItem, ShareState, ShareView};
use fetchpath_protocol::principal::{AgentName, Principal};
use fetchpath_protocol::send::OfferSource;
use fetchpath_protocol::{ClientId, EngineClient, ProtocolError, RelayKey, SensitiveUrl};
use fetchpath_session::Session;
use fetchpath_session::engine::{Engine, InProcessClient};
use fetchpath_session::share::{MAX_READ, RelayKeyStore};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

const BASE: &str = "https://share.example.com";
const HOST: &str = "share.example.com";
const KEY_HEX: &str = "9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f";

struct Setup {
    _dir: tempfile::TempDir,
    data: PathBuf,
    files: PathBuf,
    engine: Arc<Engine>,
    user: InProcessClient,
}

fn engine_in(data: &Path) -> Arc<Engine> {
    let session = Session::load_with_browser(data.join("queue-v1.json"), 3, None).unwrap();
    let mut settings = session.settings();
    settings.disk_reserve_bytes = 1;
    session.update_settings(settings).unwrap();
    Engine::new(Arc::new(session))
}

fn setup() -> Setup {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    let files = dir.path().join("files");
    for folder in [&data, &files] {
        std::fs::create_dir_all(folder).unwrap();
    }
    let engine = engine_in(&data);
    let user = InProcessClient::manual(Arc::clone(&engine));
    Setup {
        _dir: dir,
        data,
        files,
        engine,
        user,
    }
}

fn send(client: &InProcessClient, command: Command) -> Result<CommandResult, ProtocolError> {
    client.send(&ClientId::random(), command)
}

fn code<T: std::fmt::Debug>(result: Result<T, ProtocolError>) -> String {
    result
        .expect_err("expected a refusal")
        .code
        .as_str()
        .to_owned()
}

fn status(client: &InProcessClient) -> ShareView {
    match send(client, Command::ShareStatus).unwrap() {
        CommandResult::Sharing { sharing } => sharing,
        other => panic!("not a status: {other:?}"),
    }
}

fn configure(client: &InProcessClient, base_url: &str) -> ShareView {
    match send(
        client,
        Command::ConfigureSharing {
            base_url: Some(base_url.into()),
            port: None,
        },
    )
    .unwrap()
    {
        CommandResult::Sharing { sharing } => sharing,
        other => panic!("not a status: {other:?}"),
    }
}

/// The link and its item, as the one create answer carries them.
fn link(result: Result<CommandResult, ProtocolError>) -> (String, ShareItem) {
    match result.unwrap() {
        CommandResult::ShareLink { link, share } => (link.expose().to_owned(), share),
        other => panic!("not a link: {other:?}"),
    }
}

fn id_and_key(link: &str) -> (String, String) {
    let rest = link
        .strip_prefix(BASE)
        .unwrap()
        .strip_prefix("/s/")
        .unwrap();
    let (id, key) = rest.split_once('#').unwrap();
    (id.to_owned(), key.to_owned())
}

/// What the listener does: read the whole stream in order through `host`.
fn deliver(engine: &Engine, link: &str, host: &str) {
    let (id, _) = id_and_key(link);
    let transfer = engine.share_store().open(&id, host).unwrap();
    let mut at = 0_u64;
    while at < transfer.cipher_len() {
        let piece = transfer.read(at, MAX_READ).unwrap();
        let end = at + piece.bytes.len() as u64;
        transfer.delivered(at, end);
        at = end;
    }
}

fn start_test(s: &Setup) -> String {
    link(send(&s.user, Command::StartShareTest)).0
}

/// Address stored, listener up, test delivered, sharing on.
fn enabled(s: &Setup) {
    configure(&s.user, BASE);
    s.engine
        .set_share_listener(s.engine.share_port().unwrap(), Ok("127.0.0.1:47650".into()));
    let test = start_test(s);
    deliver(&s.engine, &test, HOST);
    send(&s.user, Command::SetSharingEnabled { enabled: true }).unwrap();
}

fn create(s: &Setup, path: &Path) -> Result<CommandResult, ProtocolError> {
    send(
        &s.user,
        Command::CreateShare {
            source: OfferSource::Path {
                path: path.display().to_string(),
            },
            expires_in_seconds: None,
        },
    )
}

#[test]
fn nothing_is_shared_before_the_test_passes() {
    let s = setup();
    let file = s.files.join("report.txt");
    std::fs::write(&file, b"hello").unwrap();

    // X1: a fresh engine listens on nothing and shares nothing.
    let fresh = status(&s.user);
    assert!(!fresh.enabled && fresh.base_url.is_none() && !fresh.tested);
    assert_eq!(s.engine.share_port(), None);
    assert_eq!(code(create(&s, &file)), "share.not_ready");
    assert_eq!(
        code(send(&s.user, Command::SetSharingEnabled { enabled: true })),
        "share.not_ready"
    );

    // An address alone does not enable sharing, and it does not start a
    // listener that is not asked for: the port is known, the bind is the CLI's.
    configure(&s.user, BASE);
    assert!(s.engine.share_port().is_some());
    assert_eq!(
        code(send(&s.user, Command::StartShareTest)),
        "share.not_ready"
    );
    s.engine
        .set_share_listener(s.engine.share_port().unwrap(), Ok("127.0.0.1:47650".into()));
    assert_eq!(
        code(send(&s.user, Command::SetSharingEnabled { enabled: true })),
        "share.not_ready",
        "no test yet"
    );

    // X2: a delivery through any other Host is refused and passes nothing.
    let test = start_test(&s);
    let (id, _) = id_and_key(&test);
    for host in ["127.0.0.1:47650", "localhost", "evil.example.com"] {
        assert!(s.engine.share_store().open(&id, host).is_err(), "{host}");
    }
    assert!(!status(&s.user).tested);
    deliver(&s.engine, &test, HOST);
    assert!(status(&s.user).tested);
    send(&s.user, Command::SetSharingEnabled { enabled: true }).unwrap();
    assert!(status(&s.user).enabled);

    // X3: a different address or port is a different tunnel; the test, the
    // switch and every link go back to the start.
    let (live, _) = link(create(&s, &file));
    configure(&s.user, "https://other.example.com");
    let after = status(&s.user);
    assert!(!after.enabled && !after.tested);
    assert!(
        after
            .shares
            .iter()
            .all(|item| item.state != ShareState::Live)
    );
    assert!(
        s.engine
            .share_store()
            .open(&id_and_key(&live).0, HOST)
            .is_err()
    );
    assert_eq!(code(create(&s, &file)), "share.not_ready");
    send(
        &s.user,
        Command::ConfigureSharing {
            base_url: None,
            port: Some(47651),
        },
    )
    .unwrap();
    assert_eq!(s.engine.share_port(), Some(47651));

    // The address survives a restart; the switch does not.
    let restarted = setup_again(&s);
    let view = status(&restarted.user);
    assert_eq!(view.base_url.as_deref(), Some("https://other.example.com"));
    assert_eq!(view.port, 47651);
    assert!(!view.enabled && !view.tested);
}

/// A second handle on the same data folder and files, for restarts.
fn setup_again(s: &Setup) -> Setup {
    let engine = engine_in(&s.data);
    Setup {
        _dir: tempfile::tempdir().unwrap(),
        data: s.data.clone(),
        files: s.files.clone(),
        user: InProcessClient::manual(Arc::clone(&engine)),
        engine,
    }
}

#[test]
fn the_link_is_shown_once_and_nowhere_else() {
    let s = setup();
    enabled(&s);
    let file = s.files.join("private name.txt");
    std::fs::write(&file, b"contents").unwrap();
    let (link, item) = link(create(&s, &file));
    let (id, key) = id_and_key(&link);
    assert_eq!((item.name.as_str(), item.size), ("private name.txt", 8));

    // X4: the id and the key are in the create answer only. Not in a status,
    // not in a revoke or list answer, not in a debug print of the answer.
    let create_again = create(&s, &file).unwrap();
    assert!(!format!("{create_again:?}").contains(&key));
    let answers = [
        serde_json::to_string(&status(&s.user)).unwrap(),
        serde_json::to_string(&send(&s.user, Command::ShareStatus).unwrap()).unwrap(),
        format!("{:?}", status(&s.user)),
    ];
    for answer in answers {
        assert!(!answer.contains(&id) && !answer.contains(&key), "{answer}");
    }

    // The handle revokes; the id is not what names a link to the person.
    let handle = status(&s.user)
        .shares
        .iter()
        .find(|item| !item.test)
        .unwrap()
        .handle
        .clone();
    assert_ne!(handle, id);
    send(
        &s.user,
        Command::RevokeShare {
            handle: handle.clone(),
        },
    )
    .unwrap();
    assert_eq!(
        code(send(&s.user, Command::RevokeShare { handle })),
        "share.unknown_link"
    );
    assert!(s.engine.share_store().open(&id, HOST).is_err());
    send(&s.user, Command::RevokeAllShares).unwrap();
    assert!(!s.engine.has_live_shares());
}

#[test]
fn only_the_person_makes_links() {
    let s = setup();
    enabled(&s);
    let file = s.files.join("a.txt");
    std::fs::write(&file, b"a").unwrap();
    let agent = InProcessClient::manual(Arc::clone(&s.engine))
        .with_principal(Principal::Agent(AgentName::try_from("helper").unwrap()));
    for command in [
        Command::ShareStatus,
        Command::ConfigureSharing {
            base_url: Some("https://evil.example.com".into()),
            port: None,
        },
        Command::ConfigureRelay {
            relay: true,
            key: Some(relay_key()),
        },
        Command::SetSharingEnabled { enabled: false },
        Command::StartShareTest,
        Command::CreateShare {
            source: OfferSource::Path {
                path: file.display().to_string(),
            },
            expires_in_seconds: None,
        },
        Command::RevokeShare { handle: "x".into() },
        Command::RevokeAllShares,
    ] {
        let name = command.name();
        let refused = send(&agent, command);
        assert!(refused.is_err(), "{name} was allowed for an agent");
    }
    assert_eq!(status(&s.user).base_url.as_deref(), Some(BASE));
    assert!(status(&s.user).enabled);
}

#[test]
fn no_other_principal_can_change_the_relay_or_its_key() {
    let s = setup();
    let keys = with_keys(&s);
    configure(&s.user, BASE);
    for principal in [
        Principal::Device(
            fetchpath_protocol::principal::DeviceId::try_from("abc123".to_owned()).unwrap(),
        ),
        Principal::Browser,
        Principal::Agent(AgentName::try_from("helper").unwrap()),
    ] {
        let client = InProcessClient::manual(Arc::clone(&s.engine)).with_principal(principal);
        let refused = send(
            &client,
            Command::ConfigureRelay {
                relay: true,
                key: Some(relay_key()),
            },
        );
        assert!(refused.is_err());
    }
    assert!(keys.0.lock().unwrap().is_none(), "a key was stored");
    assert!(!status(&s.user).relay);
}

#[test]
fn a_file_is_frozen_while_its_link_lives_and_unsafe_sources_are_refused() {
    let s = setup();
    enabled(&s);
    let file = s.files.join("report.txt");
    std::fs::write(&file, b"hello").unwrap();

    for refused in [
        PathBuf::from("report.txt"),
        PathBuf::from(r"\\server\share\report.txt"),
        s.files.join("missing.txt"),
        s.files.join("report.txt.part"),
    ] {
        assert_eq!(code(create(&s, &refused)), "share.not_shareable");
    }
    let engine_file = s.data.join("queue-v1.json");
    assert!(engine_file.exists());
    assert_eq!(code(create(&s, &engine_file)), "share.not_shareable");

    // X5: what the link serves is what was read; a writer, a rename or a
    // delete is refused by the system until the link ends.
    let (link, _) = link(create(&s, &file));
    assert!(std::fs::write(&file, b"changed").is_err());
    assert!(std::fs::remove_file(&file).is_err());
    assert!(std::fs::rename(&file, s.files.join("renamed.txt")).is_err());
    let (id, _) = id_and_key(&link);
    assert!(s.engine.share_store().probe(&id, HOST).is_ok());
    send(&s.user, Command::RevokeAllShares).unwrap();
    std::fs::write(&file, b"changed").unwrap();
}

fn serve(body: &'static [u8]) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            thread::spawn(move || {
                let mut request = [0_u8; 4096];
                let _ = stream.read(&mut request);
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(body);
            });
        }
    });
    address.to_string()
}

#[test]
fn a_credentialed_download_is_never_linked() {
    let address = serve(b"private bytes");
    let s = setup();
    enabled(&s);
    let secret = s.files.join("secret.bin");
    let CommandResult::Job { job } = send(
        &s.user,
        Command::CreateJob {
            request: JobRequest::File {
                input: JobInput::Url {
                    url: SensitiveUrl::try_from(format!("http://{address}/secret.bin")).unwrap(),
                },
                destination: DestinationIntent {
                    path: secret.display().to_string(),
                    conflict: ConflictPolicy::Ask,
                },
                not_before: None,
                expected_sha256: None,
            },
        },
    )
    .unwrap() else {
        panic!("not a job")
    };
    let deadline = Instant::now() + Duration::from_secs(20);
    while !secret.exists() || std::fs::metadata(&secret).map(|m| m.len()).unwrap_or(0) == 0 {
        s.engine.tick();
        assert!(Instant::now() < deadline, "the download never finished");
        thread::sleep(Duration::from_millis(10));
    }
    thread::sleep(Duration::from_millis(200));
    s.engine.tick();

    // A plain download is linkable by path; the same output with its public
    // flag gone (as a login-bearing download's record is) is not, by job or
    // by path, and neither is a copy of its bytes.
    let queue = s.data.join("queue-v1.json");
    let saved = std::fs::read_to_string(&queue).unwrap();
    let stripped = saved
        .replace("\"publicOrigin\": true,", "")
        .replace("\"publicOrigin\": true", "\"publicOrigin\": false")
        .replace("\"publicOrigin\":true,", "")
        .replace("\"publicOrigin\":true", "\"publicOrigin\":false");
    assert_ne!(saved, stripped, "the flag is persisted");
    std::fs::write(&queue, stripped).unwrap();
    let restarted = setup_again(&s);
    restarted.engine.tick();
    enabled(&restarted);

    assert_eq!(
        code(send(
            &restarted.user,
            Command::CreateShare {
                source: OfferSource::Job { job_id: job.job_id },
                expires_in_seconds: None,
            },
        )),
        "policy.credentialed_not_allowed"
    );
    assert_eq!(
        code(create(&restarted, &secret)),
        "policy.credentialed_not_allowed"
    );
    let copy = s.files.join("renamed.dat");
    std::fs::write(&copy, std::fs::read(&secret).unwrap()).unwrap();
    assert_eq!(
        code(create(&restarted, &copy)),
        "policy.credentialed_not_allowed"
    );
}

/// The host's sealed key file, reduced to what the engine sees of it.
#[derive(Default)]
struct Keys(Mutex<Option<String>>);

impl RelayKeyStore for Keys {
    fn present(&self) -> bool {
        self.0.lock().unwrap().is_some()
    }

    fn store(&self, key: &str) -> Result<(), String> {
        *self.0.lock().unwrap() = Some(key.to_owned());
        Ok(())
    }
}

fn relay_key() -> RelayKey {
    RelayKey::try_from("9f".repeat(32)).unwrap()
}

fn with_keys(s: &Setup) -> Arc<Keys> {
    let keys = Arc::new(Keys::default());
    s.engine.set_relay_keys(keys.clone());
    keys
}

fn relay(s: &Setup, on: bool, key: Option<RelayKey>) -> Result<ShareView, ProtocolError> {
    match send(&s.user, Command::ConfigureRelay { relay: on, key })? {
        CommandResult::Sharing { sharing } => Ok(sharing),
        other => panic!("not a status: {other:?}"),
    }
}

#[test]
fn relay_mode_needs_an_address_and_a_key_and_binds_no_local_port() {
    let s = setup();
    with_keys(&s);
    // Nothing stored yet.
    assert_eq!(code(relay(&s, true, Some(relay_key()))), "share.not_ready");
    configure(&s.user, BASE);
    assert_eq!(code(relay(&s, true, None)), "share.not_ready");
    let view = relay(&s, true, Some(relay_key())).unwrap();
    assert!(view.relay && view.relay_key_stored);
    assert_eq!(s.engine.share_port(), None);
    let plan = s.engine.share_relay().unwrap();
    assert_eq!((plan.host.as_str(), plan.port), ("share.example.com", 443));
    let view = relay(&s, false, None).unwrap();
    assert!(!view.relay && view.relay_key_stored);
    assert!(s.engine.share_port().is_some());
    assert!(s.engine.share_relay().is_none());
}

#[test]
fn without_a_key_store_no_key_is_accepted() {
    let s = setup();
    configure(&s.user, BASE);
    assert_eq!(
        code(relay(&s, true, Some(relay_key()))),
        "share.unsupported"
    );
}

#[test]
fn the_key_is_write_only() {
    let s = setup();
    let keys = with_keys(&s);
    configure(&s.user, BASE);
    let view = relay(&s, true, Some(relay_key())).unwrap();
    assert_eq!(keys.0.lock().unwrap().as_deref(), Some(KEY_HEX));
    let shown = serde_json::to_string(&view).unwrap();
    let debug = format!("{view:?}");
    let again = serde_json::to_string(&status(&s.user)).unwrap();
    for text in [shown, debug, again] {
        assert!(!text.contains(&KEY_HEX[..16]), "{text}");
    }
    for entry in std::fs::read_dir(&s.data).unwrap() {
        let bytes = std::fs::read(entry.unwrap().path()).unwrap_or_default();
        assert!(!String::from_utf8_lossy(&bytes).contains(&KEY_HEX[..16]));
    }
}

#[test]
fn a_failed_seal_leaves_the_setup_running() {
    struct Broken;
    impl RelayKeyStore for Broken {
        fn present(&self) -> bool {
            false
        }
        fn store(&self, _: &str) -> Result<(), String> {
            Err("sealing failed".into())
        }
    }
    let s = setup();
    s.engine.set_relay_keys(Arc::new(Broken));
    configure(&s.user, BASE);
    assert!(relay(&s, true, Some(relay_key())).is_err());
    assert!(!status(&s.user).relay);
}

#[test]
fn changing_the_mode_or_the_key_ends_the_test_and_every_link() {
    let s = setup();
    with_keys(&s);
    enabled(&s);
    assert!(status(&s.user).enabled && status(&s.user).tested);
    let first = relay(&s, true, Some(relay_key())).unwrap();
    assert!(
        !first.enabled
            && !first.tested
            && first
                .shares
                .iter()
                .all(|item| item.state != ShareState::Live)
    );
    // Report relay as up, deliver a test, enable.
    let plan = s.engine.share_relay().unwrap();
    s.engine
        .set_share_relay(plan.generation, Ok("relay connected".into()));
    let test = start_test(&s);
    deliver(&s.engine, &test, HOST);
    send(&s.user, Command::SetSharingEnabled { enabled: true }).unwrap();
    assert!(status(&s.user).enabled);
    // A new key is a new setup.
    let other = RelayKey::try_from("11".repeat(32)).unwrap();
    let view = relay(&s, true, Some(other)).unwrap();
    assert!(
        !view.enabled
            && !view.tested
            && view
                .shares
                .iter()
                .all(|item| item.state != ShareState::Live)
    );
    assert_ne!(s.engine.share_relay().unwrap().generation, plan.generation);
    // The same request again changes nothing.
    let generation = s.engine.share_relay().unwrap().generation;
    relay(&s, true, None).unwrap();
    assert_eq!(s.engine.share_relay().unwrap().generation, generation);
}

#[test]
fn a_late_report_from_an_old_relay_task_unlocks_nothing() {
    let s = setup();
    with_keys(&s);
    configure(&s.user, BASE);
    relay(&s, true, Some(relay_key())).unwrap();
    let old = s.engine.share_relay().unwrap().generation;
    relay(&s, true, Some(RelayKey::try_from("22".repeat(32)).unwrap())).unwrap();
    s.engine.set_share_relay(old, Ok("relay connected".into()));
    assert_eq!(
        code(send(&s.user, Command::StartShareTest)),
        "share.not_ready"
    );
    // Nor does a local listener report on the old port.
    s.engine
        .set_share_listener(47650, Ok("127.0.0.1:47650".into()));
    assert_eq!(
        code(send(&s.user, Command::StartShareTest)),
        "share.not_ready"
    );
    let now = s.engine.share_relay().unwrap().generation;
    s.engine.set_share_relay(now, Err("refused".into()));
    assert_eq!(status(&s.user).problem.as_deref(), Some("refused"));
    s.engine.set_share_relay(now, Ok("relay connected".into()));
    assert!(status(&s.user).serving.is_some());
    assert!(send(&s.user, Command::StartShareTest).is_ok());
}

#[test]
fn live_link_ids_are_listed_for_the_relay() {
    let s = setup();
    with_keys(&s);
    configure(&s.user, BASE);
    relay(&s, true, Some(relay_key())).unwrap();
    s.engine.set_share_relay(
        s.engine.share_relay().unwrap().generation,
        Ok("relay connected".into()),
    );
    assert!(s.engine.share_store().live_ids().is_empty());
    let test = start_test(&s);
    assert_eq!(s.engine.share_store().live_ids(), vec![id_and_key(&test).0]);
    send(&s.user, Command::RevokeAllShares).unwrap();
    assert!(s.engine.share_store().live_ids().is_empty());
}

#[test]
fn relay_mode_survives_a_restart_with_sharing_off() {
    let s = setup();
    with_keys(&s);
    configure(&s.user, BASE);
    relay(&s, true, Some(relay_key())).unwrap();
    let again = setup_again(&s);
    let view = status(&again.user);
    assert!(view.relay && !view.enabled);
    assert_eq!(again.engine.share_port(), None);
    assert!(again.engine.share_relay().is_some());
}
