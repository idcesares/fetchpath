//! The share listener over real sockets (FP-117, gates X1 to X4): one route
//! on the stored port, the Host check, Range resume, a ticket used up only by
//! a complete in-order delivery, and nothing identifying in a response.

use super::Share;
use fetchpath_protocol::command::Command;
use fetchpath_protocol::message::CommandResult;
use fetchpath_protocol::model::ShareView;
use fetchpath_protocol::{ClientId, EngineClient};
use fetchpath_session::Session;
use fetchpath_session::engine::{Engine, InProcessClient};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::time::{Duration, Instant};

const BASE: &str = "https://share.example.com";
const HOST: &str = "share.example.com";

struct Up {
    _dir: tempfile::TempDir,
    user: InProcessClient,
    share: Share,
    port: u16,
}

impl Drop for Up {
    fn drop(&mut self) {
        self.share.shutdown();
    }
}

fn free_port() -> u16 {
    TcpListener::bind(("127.0.0.1", 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn status(user: &InProcessClient) -> ShareView {
    match user
        .send(&ClientId::random(), Command::ShareStatus)
        .unwrap()
    {
        CommandResult::Sharing { sharing } => sharing,
        other => panic!("not a status: {other:?}"),
    }
}

fn configure(user: &InProcessClient, port: u16) {
    user.send(
        &ClientId::random(),
        Command::ConfigureSharing {
            base_url: Some(BASE.into()),
            port: Some(port),
        },
    )
    .unwrap();
}

/// An engine with the address stored and the listener running on it.
fn up() -> Up {
    let dir = tempfile::tempdir().unwrap();
    let session = Session::load_with_browser(dir.path().join("queue-v1.json"), 3, None).unwrap();
    let engine = Engine::new(Arc::new(session));
    let user = InProcessClient::manual(Arc::clone(&engine));
    let port = free_port();
    configure(&user, port);
    let share = Share::new(Arc::clone(&engine));
    share.reconcile();
    assert_eq!(share.port(), Some(port), "{:?}", status(&user).problem);
    Up {
        _dir: dir,
        user,
        share,
        port,
    }
}

/// The path and fragment key of a fresh setup test link.
fn test_link(up: &Up) -> (String, String) {
    let result = up
        .user
        .send(&ClientId::random(), Command::StartShareTest)
        .unwrap();
    let CommandResult::ShareLink { link, .. } = result else {
        panic!("not a link");
    };
    let rest = link.expose().strip_prefix(BASE).unwrap().to_owned();
    let (path, key) = rest.split_once('#').unwrap();
    (path.to_owned(), key.to_owned())
}

struct Answer {
    status: u16,
    head: String,
    body: Vec<u8>,
}

impl Answer {
    fn header(&self, name: &str) -> Option<&str> {
        self.head.lines().skip(1).find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.trim().eq_ignore_ascii_case(name).then(|| value.trim())
        })
    }
}

fn request(port: u16, method: &str, target: &str, headers: &[(&str, &str)]) -> Answer {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let mut head = format!("{method} {target} HTTP/1.1\r\n");
    for (name, value) in headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("Connection: close\r\n\r\n");
    stream.write_all(head.as_bytes()).unwrap();
    let mut raw = Vec::new();
    let _ = stream.read_to_end(&mut raw);
    let split = raw
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .expect("a response head");
    let head = String::from_utf8_lossy(&raw[..split]).into_owned();
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .expect("a status line");
    Answer {
        status,
        head,
        body: raw[split + 4..].to_vec(),
    }
}

fn get(port: u16, target: &str) -> Answer {
    request(port, "GET", target, &[("Host", HOST)])
}

fn ranged(port: u16, target: &str, range: &str) -> Answer {
    request(port, "GET", target, &[("Host", HOST), ("Range", range)])
}

fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
    let until = Instant::now() + Duration::from_secs(5);
    while !done() {
        assert!(Instant::now() < until, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn only_get_of_a_link_is_served() {
    let up = up();
    let (path, _) = test_link(&up);
    for target in [
        "/",
        "/index.html",
        "/api/ws",
        "/s",
        "/s/",
        "/s/short",
        &format!("{path}/"),
        &format!("{path}?x=1"),
        &format!("{path}?d=2"),
        &format!("{path}?d=1&x=1"),
    ] {
        let answer = get(up.port, target);
        assert_eq!(answer.status, 404, "{target}");
        assert!(answer.body.is_empty(), "{target}");
    }
    for method in ["POST", "PUT", "DELETE", "HEAD", "OPTIONS", "PATCH"] {
        let answer = request(up.port, method, &path, &[("Host", HOST)]);
        assert_eq!(answer.status, 405, "{method}");
        assert_eq!(answer.header("allow"), Some("GET"));
    }
    // None of that used the link up.
    assert_eq!(get(up.port, &path).status, 200);
}

#[test]
fn another_host_name_learns_nothing_and_uses_nothing_up() {
    let up = up();
    let (path, _) = test_link(&up);
    let stream = format!("{path}?d=1");
    for target in [&path, &stream] {
        for host in [
            "evil.example.net",
            "127.0.0.1",
            "share.example.com:8080",
            "",
        ] {
            let answer = request(up.port, "GET", target, &[("Host", host)]);
            assert_eq!(answer.status, 421, "{host:?} for {target}");
            assert!(answer.body.is_empty());
        }
        let twice = request(
            up.port,
            "GET",
            target,
            &[("Host", HOST), ("Host", "evil.example.net")],
        );
        assert_eq!(twice.status, 421);
    }
    // The name is compared in lower case, and the link still works.
    let upper = request(up.port, "GET", &path, &[("Host", "SHARE.Example.COM")]);
    assert_eq!(upper.status, 200);
    assert_eq!(get(up.port, &stream).status, 200);
}

#[test]
fn the_page_and_the_stream_say_nothing_about_the_file() {
    let up = up();
    let (path, key) = test_link(&up);
    let name = status(&up.user).shares[0].name.clone();
    let page = get(up.port, &path);
    assert_eq!(page.status, 200);
    let policy = page.header("content-security-policy").unwrap();
    assert!(policy.contains("script-src 'sha256-"), "{policy}");
    assert!(!policy.contains("unsafe-"), "{policy}");
    assert_eq!(page.header("referrer-policy"), Some("no-referrer"));
    assert_eq!(page.header("cache-control"), Some("no-store"));
    assert_eq!(page.header("x-content-type-options"), Some("nosniff"));
    let stream = get(up.port, &format!("{path}?d=1"));
    assert_eq!(stream.status, 200);
    assert_eq!(
        stream.header("content-type"),
        Some("application/octet-stream")
    );
    assert_eq!(stream.header("accept-ranges"), Some("bytes"));
    assert_eq!(&stream.body[..4], b"FPS1");
    for answer in [&page, &stream] {
        assert!(answer.header("content-disposition").is_none());
        assert!(answer.header("access-control-allow-origin").is_none());
        let everything = [answer.head.as_bytes(), &answer.body].concat();
        let text = String::from_utf8_lossy(&everything).to_lowercase();
        assert!(!text.contains(&key.to_lowercase()));
        assert!(!text.contains(&name.to_lowercase()));
    }
}

#[test]
fn a_link_is_used_up_by_a_complete_in_order_delivery_only() {
    let up = up();
    let (path, _) = test_link(&up);
    let stream = format!("{path}?d=1");
    let whole = get(up.port, &stream);
    assert_eq!(whole.status, 200);
    let total = whole.body.len();
    assert_eq!(whole.header("content-length"), Some(&*total.to_string()));
    wait_for("the test to pass", || status(&up.user).tested);
    assert_eq!(get(up.port, &stream).status, 410);
    assert_eq!(get(up.port, &path).status, 410);
}

#[test]
fn a_resumed_download_completes_the_link_and_a_bad_range_does_not() {
    let up = up();
    let (path, _) = test_link(&up);
    let stream = format!("{path}?d=1");
    let first = get(up.port, &stream);
    let total = first.body.len();
    // `first` completed it; make a new one for the range checks.
    wait_for("the first link to end", || status(&up.user).tested);
    let (path, _) = test_link(&up);
    let stream = format!("{path}?d=1");

    // A resume that skips the start does not complete anything.
    let skipped = ranged(up.port, &stream, "bytes=22-");
    assert_eq!(skipped.status, 206);
    assert_eq!(
        skipped.header("content-range"),
        Some(&*format!("bytes 22-{}/{total}", total - 1))
    );
    assert_eq!(skipped.body.len(), total - 22);
    std::thread::sleep(Duration::from_millis(100));
    assert!(!status(&up.user).tested, "a tail alone is not a download");

    for refused in [
        "bytes=-5",
        "bytes=0-1,5-6",
        "bytes=5-4",
        "bytes=x-",
        "chunks=0-",
    ] {
        let answer = ranged(up.port, &stream, refused);
        assert_eq!(answer.status, 416, "{refused}");
        assert_eq!(
            answer.header("content-range"),
            Some(&*format!("bytes */{total}"))
        );
    }
    let past = ranged(up.port, &stream, &format!("bytes={total}-"));
    assert_eq!(past.status, 416);

    // The start, then the rest: in order, so the link is used up.
    let head = ranged(up.port, &stream, "bytes=0-29");
    assert_eq!(head.status, 206);
    assert_eq!(head.body.len(), 30);
    assert_eq!(&head.body[..4], b"FPS1");
    let tail = ranged(up.port, &stream, "bytes=30-");
    assert_eq!(tail.status, 206);
    let joined = [head.body, tail.body].concat();
    assert_eq!(joined.len(), total);
    assert_eq!(
        joined[22..],
        skipped.body[..],
        "the same link gives the same bytes"
    );
    wait_for("the resumed download to complete", || {
        status(&up.user).shares[0].delivered == total as u64
    });
    assert_eq!(get(up.port, &stream).status, 410);
}

#[test]
fn a_taken_port_is_a_visible_failure_and_never_another_port() {
    let dir = tempfile::tempdir().unwrap();
    let session = Session::load_with_browser(dir.path().join("queue-v1.json"), 3, None).unwrap();
    let engine = Engine::new(Arc::new(session));
    let user = InProcessClient::manual(Arc::clone(&engine));
    let taken = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = taken.local_addr().unwrap().port();
    configure(&user, port);
    let share = Share::new(Arc::clone(&engine));
    share.reconcile();
    assert_eq!(share.port(), None);
    let view = status(&user);
    assert!(view.serving.is_none());
    let problem = view.problem.expect("the reason is shown");
    assert!(problem.contains(&port.to_string()), "{problem}");
    // Nothing can be linked or tested through a port that is not ours.
    assert!(
        user.send(&ClientId::random(), Command::StartShareTest)
            .is_err()
    );
    drop(taken);
    share.shutdown();
}

#[test]
fn the_listener_follows_the_stored_port_and_closes_with_the_engine() {
    let up = up();
    let old = up.port;
    assert_eq!(get(old, "/").status, 404);
    let new = free_port();
    configure(&up.user, new);
    up.share.reconcile();
    assert_eq!(up.share.port(), Some(new));
    assert!(TcpStream::connect(("127.0.0.1", old)).is_err());
    assert_eq!(get(new, "/").status, 404);
    assert_eq!(status(&up.user).serving, Some(format!("127.0.0.1:{new}")));
    up.share.shutdown();
    assert!(TcpStream::connect(("127.0.0.1", new)).is_err());
}

// Relay mode (FP-118): the same handler, reached through a relay.

mod relayed {
    use super::*;
    use crate::share::relay_key::{KeyFile, TestProtector};
    use fetchpath_protocol::RelayKey;
    use fetchpath_relay::client::Connector;
    use fetchpath_relay::server::{Conn, Limits, Relay};
    use fetchpath_relay::wire::EnrollmentKey;
    use tokio::sync::Notify;

    /// A relay on loopback sockets, run on a thread of its own.
    struct Rig {
        visitors: u16,
        engines: u16,
        key: EnrollmentKey,
        stop: Arc<Notify>,
    }

    impl Drop for Rig {
        fn drop(&mut self) {
            self.stop.notify_one();
        }
    }

    fn rig() -> Rig {
        let key = EnrollmentKey::generate().unwrap();
        let relay = Relay::new(key.clone(), Limits::default(), |_| {});
        let stop = Arc::new(Notify::new());
        let (sent, ports) = std::sync::mpsc::channel();
        let waiting = Arc::clone(&stop);
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async move {
                let engines = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                let visitors = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                sent.send((
                    visitors.local_addr().unwrap().port(),
                    engines.local_addr().unwrap().port(),
                ))
                .unwrap();
                let for_engines = Arc::clone(&relay);
                tokio::spawn(async move {
                    while let Ok((stream, _)) = engines.accept().await {
                        tokio::spawn(Arc::clone(&for_engines).serve_engine(stream));
                    }
                });
                tokio::spawn(async move {
                    while let Ok((stream, _)) = visitors.accept().await {
                        tokio::spawn(Arc::clone(&relay).serve_visitor(stream));
                    }
                });
                waiting.notified().await;
            });
        });
        let (visitors, engines) = ports.recv().unwrap();
        Rig {
            visitors,
            engines,
            key,
            stop,
        }
    }

    fn dial(engines: u16) -> crate::share::relay::Dial {
        Arc::new(move |_plan| {
            let connector: Connector = Arc::new(move || {
                Box::pin(async move {
                    let stream = tokio::net::TcpStream::connect(("127.0.0.1", engines)).await?;
                    Ok(Box::new(stream) as Conn)
                })
            });
            Ok(connector)
        })
    }

    struct Up {
        _dir: tempfile::TempDir,
        key_path: std::path::PathBuf,
        user: InProcessClient,
        share: Share,
        rig: Rig,
    }

    impl Drop for Up {
        fn drop(&mut self) {
            self.share.shutdown();
        }
    }

    fn send(user: &InProcessClient, command: Command) -> Result<CommandResult, String> {
        user.send(&ClientId::random(), command)
            .map_err(|error| error.message)
    }

    fn enroll(user: &InProcessClient, key: &EnrollmentKey) {
        send(
            user,
            Command::ConfigureRelay {
                relay: true,
                key: Some(RelayKey::try_from(key.to_hex()).unwrap()),
            },
        )
        .unwrap();
    }

    /// Relay mode chosen with `key`; `rig` is the relay the engine dials.
    fn up_with(rig: Rig, key: &EnrollmentKey) -> Up {
        let dir = tempfile::tempdir().unwrap();
        let session =
            Session::load_with_browser(dir.path().join("queue-v1.json"), 3, None).unwrap();
        let engine = Engine::new(Arc::new(session));
        let key_path = dir.path().join("relay-key");
        let keys = Arc::new(KeyFile::new(key_path.clone(), Box::new(TestProtector)));
        engine.set_relay_keys(keys.clone());
        let user = InProcessClient::manual(Arc::clone(&engine));
        send(
            &user,
            Command::ConfigureSharing {
                base_url: Some(BASE.into()),
                port: Some(free_port()),
            },
        )
        .unwrap();
        let share = Share::with_relay(Arc::clone(&engine), keys, dial(rig.engines));
        enroll(&user, key);
        share.reconcile();
        Up {
            _dir: dir,
            key_path,
            user,
            share,
            rig,
        }
    }

    fn up() -> Up {
        let rig = rig();
        let key = rig.key.clone();
        let up = up_with(rig, &key);
        wait_for("the relay to enroll the engine", || {
            status(&up.user).serving.is_some()
        });
        up
    }

    fn test_link_for(user: &InProcessClient) -> (String, String) {
        let CommandResult::ShareLink { link, .. } = send(user, Command::StartShareTest).unwrap()
        else {
            panic!("not a link");
        };
        let rest = link.expose().strip_prefix(BASE).unwrap().to_owned();
        let (path, key) = rest.split_once('#').unwrap();
        (path.to_owned(), key.to_owned())
    }

    /// A fresh test link, once the relay routes it.
    fn routed(up: &Up) -> (String, String) {
        let (path, key) = test_link_for(&up.user);
        wait_for("the relay to route the link", || {
            get(up.rig.visitors, &path).status != 502
        });
        (path, key)
    }

    #[test]
    fn a_relayed_test_link_is_delivered_and_sharing_can_turn_on() {
        let up = up();
        let view = status(&up.user);
        assert!(view.relay && view.relay_key_stored);
        assert_eq!(up.share.port(), None, "no local port in relay mode");
        let (path, key) = routed(&up);
        let page = get(up.rig.visitors, &path);
        assert_eq!(page.status, 200);
        assert!(
            page.header("content-security-policy")
                .unwrap()
                .contains("script-src 'sha256-")
        );
        let stream = format!("{path}?d=1");
        // A resume through the relay, then the rest, completes the link.
        let head = ranged(up.rig.visitors, &stream, "bytes=0-99");
        assert_eq!(head.status, 206);
        assert_eq!(&head.body[..4], b"FPS1");
        let tail = ranged(up.rig.visitors, &stream, "bytes=100-");
        assert_eq!(tail.status, 206);
        wait_for("the test link to be delivered", || status(&up.user).tested);
        send(&up.user, Command::SetSharingEnabled { enabled: true }).unwrap();
        assert!(status(&up.user).enabled);
        // The key reached the sealed file only, and nothing public shows it.
        let on_disk = std::fs::read(&up.key_path).unwrap();
        assert!(!String::from_utf8_lossy(&on_disk).contains(&up.rig.key.to_hex()));
        let everything = [head.head.as_bytes(), &head.body, &tail.body].concat();
        let text = String::from_utf8_lossy(&everything);
        assert!(!text.contains(&key) && !text.contains(&up.rig.key.to_hex()));
    }

    #[test]
    fn unknown_ids_are_refused_and_another_host_is_misdirected() {
        let up = up();
        let (path, _) = routed(&up);
        let unknown = format!("/s/{}", "A".repeat(43));
        // The relay knows no engine for it.
        assert_eq!(get(up.rig.visitors, &unknown).status, 502);
        assert_eq!(get(up.rig.visitors, "/").status, 404);
        let evil = request(
            up.rig.visitors,
            "GET",
            &format!("{path}?d=1"),
            &[("Host", "evil.example.net")],
        );
        assert_eq!(evil.status, 421);
        // Neither used the link up.
        assert_eq!(get(up.rig.visitors, &path).status, 200);
    }

    #[test]
    fn a_wrong_key_is_a_visible_problem_and_nothing_can_be_tested() {
        let rig = rig();
        let wrong = EnrollmentKey::generate().unwrap();
        let up = up_with(rig, &wrong);
        wait_for("the refusal to show", || {
            status(&up.user)
                .problem
                .is_some_and(|problem| problem.contains("refused"))
        });
        assert!(status(&up.user).serving.is_none());
        assert!(send(&up.user, Command::StartShareTest).is_err());
    }

    #[test]
    fn leaving_relay_mode_hangs_up_and_serves_locally_again() {
        let up = up();
        let (path, _) = routed(&up);
        send(
            &up.user,
            Command::ConfigureRelay {
                relay: false,
                key: None,
            },
        )
        .unwrap();
        up.share.reconcile();
        assert!(up.share.port().is_some());
        // The old link ended with the mode change, and the relay lost its id.
        wait_for("the relay to forget the engine", || {
            get(up.rig.visitors, &path).status == 502
        });
    }
}
