//! The web UI listener over real loopback sockets (FP-104).

use super::*;
use fetchpath_protocol::message::CommandResult;
use fetchpath_session::Session;
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Instant;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::{self, Message, WebSocket};

const COOKIE: &str = sessions::COOKIE_NAME;

struct Fixture {
    _dir: tempfile::TempDir,
    engine: Arc<Engine>,
    connections: Arc<AtomicUsize>,
    web: Web,
}

fn fixture(ticket_ttl: Option<Duration>) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let session = Session::load_with_browser(dir.path().join("queue.json"), 2, None).unwrap();
    let engine = Engine::new(Arc::new(session));
    let connections = Arc::new(AtomicUsize::new(0));
    let mut web = Web::new(
        Arc::clone(&engine),
        dir.path().to_path_buf(),
        Arc::clone(&connections),
    );
    if let Some(ttl) = ticket_ttl {
        web.ticket_ttl = ttl;
    }
    web.apply(true);
    Fixture {
        _dir: dir,
        engine,
        connections,
        web,
    }
}

impl Fixture {
    fn port(&self) -> u16 {
        self.web.addresses()[0].port()
    }

    fn host(&self) -> String {
        format!("{CANONICAL_HOST}:{}", self.port())
    }

    /// The path and query of a fresh sign-in link.
    fn link(&self) -> String {
        let Ok(CommandResult::WebUiLink { url }) = self
            .web
            .answer(&Principal::User, &Command::OpenWebUi)
            .expect("answered here")
        else {
            panic!("no link");
        };
        let link = url.expose().to_owned();
        link[link.find("/open").unwrap()..].to_owned()
    }

    /// Signs a browser in and returns its cookie.
    fn sign_in(&self) -> String {
        let reply = get(self.port(), &self.host(), &self.link(), &[]);
        assert_eq!(reply.status, 303);
        reply.cookie().expect("a cookie")
    }

    fn socket(&self, cookie: &str) -> WebSocket<TcpStream> {
        let host = self.host();
        let mut request = format!("ws://{host}/ui/socket")
            .into_client_request()
            .unwrap();
        let headers = request.headers_mut();
        headers.insert("origin", format!("http://{host}").parse().unwrap());
        headers.insert("cookie", format!("{COOKIE}={cookie}").parse().unwrap());
        let stream = TcpStream::connect(("127.0.0.1", self.port())).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let (socket, _) = tungstenite::client(request, stream).expect("the upgrade");
        socket
    }
}

struct Reply {
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
}

impl Reply {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    fn cookie(&self) -> Option<String> {
        let set = self.header("set-cookie")?;
        let pair = set.split(';').next()?;
        pair.strip_prefix(&format!("{COOKIE}=")).map(str::to_owned)
    }
}

fn get(port: u16, host: &str, path: &str, extra: &[(&str, &str)]) -> Reply {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut request = format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n");
    for (name, value) in extra {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    request.push_str("\r\n");
    stream.write_all(request.as_bytes()).unwrap();
    let mut raw = Vec::new();
    let _ = stream.read_to_end(&mut raw);
    let text = String::from_utf8_lossy(&raw).into_owned();
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    let mut lines = head.lines();
    let status = lines
        .next()
        .and_then(|line| line.split(' ').nth(1))
        .and_then(|code| code.parse().ok())
        .expect("a status line");
    let headers = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.to_ascii_lowercase(), value.trim().to_owned()))
        .collect();
    Reply {
        status,
        headers,
        body: body.to_owned(),
    }
}

/// A WebSocket upgrade request that the listener may refuse.
fn upgrade(f: &Fixture, host: &str, origin: Option<&str>, cookie: Option<&str>) -> Reply {
    let mut extra = vec![
        ("Upgrade", "websocket"),
        ("Connection", "Upgrade"),
        ("Sec-WebSocket-Version", "13"),
        // RFC 6455 sample nonce, not a secret.
        ("Sec-WebSocket-Key", "dGhlIHNhbXBsZSBub25jZQ=="), // gitleaks:allow
    ];
    if let Some(origin) = origin {
        extra.push(("Origin", origin));
    }
    let cookie = cookie.map(|cookie| format!("{COOKIE}={cookie}"));
    if let Some(cookie) = &cookie {
        extra.push(("Cookie", cookie));
    }
    get(f.port(), host, "/ui/socket", &extra)
}

/// The next answer, skipping notices; `None` once the socket is closed.
fn next_answer(socket: &mut WebSocket<TcpStream>) -> Option<Value> {
    loop {
        match socket.read() {
            Ok(Message::Text(text)) => {
                let value: Value = serde_json::from_str(text.as_str()).unwrap();
                if value.get("notice").is_none() {
                    return Some(value);
                }
            }
            Ok(Message::Close(_)) | Err(_) => return None,
            Ok(_) => {}
        }
    }
}

fn ask(socket: &mut WebSocket<TcpStream>, call: &str, args: Value) -> Option<Value> {
    let frame = json!({ "id": 1, "call": call, "args": args }).to_string();
    socket.send(Message::text(frame)).ok()?;
    next_answer(socket)
}

fn wait_for(mut done: impl FnMut() -> bool) -> bool {
    let until = Instant::now() + Duration::from_secs(5);
    while Instant::now() < until {
        if done() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

fn device() -> DeviceId {
    DeviceId::try_from("0123456789abcdef0123".to_owned()).unwrap()
}

#[test]
fn a_ticket_signs_one_browser_in_once() {
    let f = fixture(None);
    let (port, host) = (f.port(), f.host());
    let signed_out = get(port, &host, "/", &[]);
    assert_eq!(signed_out.status, 200);
    assert_eq!(get(port, &host, "/app.js", &[]).status, 404);

    let link = f.link();
    let first = get(port, &host, &link, &[]);
    assert_eq!(first.status, 303);
    let cookie = first.cookie().expect("a cookie");
    assert_eq!(cookie.len(), 64);
    let attributes = first.header("set-cookie").unwrap();
    assert!(attributes.contains("HttpOnly") && attributes.contains("SameSite=Strict"));
    assert!(
        first
            .header("content-security-policy")
            .unwrap()
            .contains("frame-ancestors")
    );

    let again = get(port, &host, &link, &[]);
    assert!(again.cookie().is_none(), "a ticket worked twice");
    let forged = get(
        port,
        &host,
        &format!("/open?ticket={}", "0".repeat(64)),
        &[],
    );
    assert!(forged.cookie().is_none());

    let cookie_header = format!("{COOKIE}={cookie}");
    let signed_in = get(port, &host, "/", &[("Cookie", &cookie_header)]);
    assert_eq!(signed_in.status, 200);
    assert_ne!(signed_in.body, signed_out.body);
}

#[test]
fn an_expired_ticket_signs_nobody_in() {
    let f = fixture(Some(Duration::from_millis(30)));
    let link = f.link();
    std::thread::sleep(Duration::from_millis(100));
    let reply = get(f.port(), &f.host(), &link, &[]);
    assert!(reply.cookie().is_none());
}

#[test]
fn a_ticket_is_not_spent_or_honored_off_the_canonical_host() {
    let f = fixture(None);
    let link = f.link();
    let other = format!("127.0.0.1:{}", f.port());
    assert!(get(f.port(), &other, &link, &[]).cookie().is_none());
    assert!(get(f.port(), &f.host(), &link, &[]).cookie().is_some());
}

#[test]
fn another_host_name_is_refused() {
    let f = fixture(None);
    for host in ["evil.example", "evil.example:80", "fetchpath.localhost"] {
        let reply = get(f.port(), host, "/", &[]);
        assert_eq!(reply.status, 421, "{host}");
        assert!(reply.body.is_empty());
    }
    let wrong_port = format!("localhost:{}", f.port() ^ 1);
    assert_eq!(get(f.port(), &wrong_port, "/", &[]).status, 421);
}

#[test]
fn the_socket_needs_a_session_and_the_pages_own_origin() {
    let f = fixture(None);
    let cookie = f.sign_in();
    let host = f.host();
    let own = format!("http://{host}");
    assert_eq!(upgrade(&f, &host, Some(&own), None).status, 403);
    assert_eq!(
        upgrade(&f, &host, Some(&own), Some(&"0".repeat(64))).status,
        403
    );
    assert_eq!(
        upgrade(&f, &host, Some("http://evil.example"), Some(&cookie)).status,
        403
    );
    assert_eq!(upgrade(&f, &host, None, Some(&cookie)).status, 403);
    assert_eq!(upgrade(&f, &host, Some(&own), Some(&cookie)).status, 101);
}

#[test]
fn the_socket_answers_calls_and_closes_on_an_unknown_one() {
    let f = fixture(None);
    let mut socket = f.socket(&f.sign_in());
    let stats = ask(&mut socket, "queueStats", json!({})).expect("an answer");
    assert!(stats.get("ok").is_some(), "{stats}");
    let connection = ask(&mut socket, "engineConnection", json!({})).unwrap();
    assert_eq!(connection["ok"]["connected"], true);
    assert!(ask(&mut socket, "shutdownEngine", json!({})).is_none());
}

#[test]
fn a_browser_submits_only_into_the_folder_choices_and_never_retargets_a_retry() {
    let f = fixture(None);
    let mut socket = f.socket(&f.sign_in());
    let batch = json!({ "drafts": [{
        "url": "http://127.0.0.1:9/a.bin",
        "destination": "C:\\Windows\\a.bin",
    }] });
    let answer = ask(&mut socket, "startBatch", batch).expect("an answer");
    assert!(answer.get("error").is_some(), "{answer}");

    let retry = json!({ "jobId": "job_0", "url": "http://127.0.0.1:9/b.bin" });
    let answer = ask(&mut socket, "retryDownload", retry).expect("an answer");
    assert_eq!(answer["error"]["code"], "policy.not_permitted");
    assert_eq!(
        answer["error"]["message"],
        "Do this in Fetchpath on this PC"
    );
}

#[test]
fn a_device_cannot_change_settings() {
    let f = fixture(None);
    let envelope = |command| CommandEnvelope::new(ClientId::random(), command);
    let Ok(CommandResult::Settings { view }) = f
        .engine
        .execute_as(&Principal::User, &envelope(Command::GetSettings))
    else {
        panic!("no settings");
    };
    let update = Command::UpdateSettings {
        settings: view.settings,
    };
    let result = f
        .engine
        .execute_as(&Principal::Device(device()), &envelope(update));
    assert!(result.is_err());
}

#[test]
fn only_the_person_gets_a_link_or_signs_browsers_out() {
    let f = fixture(None);
    for principal in [
        Principal::Browser,
        Principal::Agent("claude".try_into().unwrap()),
        Principal::Device(device()),
    ] {
        for command in [Command::OpenWebUi, Command::SignOutBrowsers] {
            let answer = f.web.answer(&principal, &command).expect("answered here");
            assert_eq!(answer.unwrap_err().code.as_str(), "policy.not_permitted");
        }
    }
    let guard = lock(&f.web.running);
    let started = guard.as_ref().unwrap();
    assert!(lock(&started.shared.tickets).is_empty());
    drop(guard);
    assert!(
        f.web
            .answer(&Principal::User, &Command::GetSettings)
            .is_none()
    );
}

#[test]
fn the_link_is_refused_while_the_web_ui_is_off() {
    let f = fixture(None);
    f.web.apply(false);
    let answer = f.web.answer(&Principal::User, &Command::OpenWebUi).unwrap();
    assert_eq!(answer.unwrap_err().code.as_str(), "contract.unsupported");
}

#[test]
fn signing_out_closes_sockets_and_ends_the_cookie() {
    let f = fixture(None);
    let cookie = f.sign_in();
    let mut socket = f.socket(&cookie);
    assert!(ask(&mut socket, "queueStats", json!({})).is_some());
    let out = f
        .web
        .answer(&Principal::User, &Command::SignOutBrowsers)
        .unwrap();
    assert!(matches!(out, Ok(CommandResult::BrowsersSignedOut)));
    assert!(next_answer(&mut socket).is_none());
    let header = format!("{COOKIE}={cookie}");
    let host = f.host();
    let own = format!("http://{host}");
    assert_eq!(upgrade(&f, &host, Some(&own), Some(&cookie)).status, 403);
    assert_eq!(
        get(f.port(), &host, "/", &[("Cookie", &header)]).body,
        get(f.port(), &host, "/", &[]).body
    );
    // The listener stays up for a new sign-in.
    assert!(
        f.web
            .answer(&Principal::User, &Command::OpenWebUi)
            .unwrap()
            .is_ok()
    );
}

#[test]
fn turning_it_off_closes_sockets_and_the_port() {
    let f = fixture(None);
    let mut socket = f.socket(&f.sign_in());
    let port = f.port();
    f.web.apply(false);
    assert!(next_answer(&mut socket).is_none());
    assert!(TcpStream::connect(("127.0.0.1", port)).is_err());
}

#[test]
fn the_listener_is_loopback_only() {
    let f = fixture(None);
    let addresses = f.web.addresses();
    assert!(!addresses.is_empty());
    assert!(addresses.iter().all(|address| address.ip().is_loopback()));
}

#[test]
fn only_a_live_socket_counts_as_a_client() {
    let f = fixture(None);
    assert_eq!(f.connections.load(Ordering::SeqCst), 0);
    let mut socket = f.socket(&f.sign_in());
    assert!(ask(&mut socket, "queueStats", json!({})).is_some());
    assert_eq!(f.connections.load(Ordering::SeqCst), 1);
    drop(socket);
    assert!(wait_for(|| f.connections.load(Ordering::SeqCst) == 0));
}

#[test]
fn a_sign_out_between_the_check_and_the_socket_leaves_no_socket() {
    let f = fixture(None);
    let cookie = f.sign_in();
    let (shared, id) = {
        let guard = lock(&f.web.running);
        let shared = Arc::clone(&guard.as_ref().unwrap().shared);
        let id = lock(&shared.sessions)
            .check(&cookie, sessions::now_ms())
            .expect("a live session");
        (shared, id)
    };
    assert!(shared.register(&id).is_some());
    shared.sign_out();
    assert!(
        shared.register(&id).is_none(),
        "a socket was admitted after the sign-out"
    );
}

#[test]
fn signing_out_also_voids_links_not_yet_used() {
    let f = fixture(None);
    let link = f.link();
    f.web
        .answer(&Principal::User, &Command::SignOutBrowsers)
        .unwrap()
        .unwrap();
    assert!(get(f.port(), &f.host(), &link, &[]).cookie().is_none());
}

#[test]
fn every_session_cookie_is_tried_and_the_cookie_ends_with_the_browser() {
    let f = fixture(None);
    let (port, host) = (f.port(), f.host());
    let reply = get(port, &host, &f.link(), &[]);
    assert!(!reply.header("set-cookie").unwrap().contains("Max-Age"));
    let real = format!("{COOKIE}={}", reply.cookie().unwrap());
    let planted = format!("{COOKIE}={}", "0".repeat(64));
    let both = format!("{planted}; {real}");
    let signed_out = get(port, &host, "/", &[]).body;
    assert_ne!(get(port, &host, "/", &[("Cookie", &both)]).body, signed_out);
    assert_ne!(
        get(port, &host, "/", &[("Cookie", &planted), ("Cookie", &real)]).body,
        signed_out
    );
}

#[test]
fn a_foreign_origin_is_refused_on_any_request_and_refusals_close_the_connection() {
    let f = fixture(None);
    let (port, host) = (f.port(), f.host());
    let own = format!("http://{host}");
    assert_eq!(get(port, &host, "/", &[("Origin", &own)]).status, 200);
    let foreign = get(port, &host, "/", &[("Origin", "http://evil.example")]);
    assert_eq!(foreign.status, 403);
    assert_eq!(foreign.header("connection"), Some("close"));
    assert_eq!(get(port, &host, "/", &[("Origin", "null")]).status, 403);
    assert_eq!(
        get(port, "evil.example", "/", &[]).header("connection"),
        Some("close")
    );
    assert_eq!(
        get(port, &host, "/nothing", &[]).header("connection"),
        Some("close")
    );
}

#[test]
fn a_new_listener_does_not_know_the_old_listeners_cookie() {
    let f = fixture(None);
    let cookie = f.sign_in();
    f.web.apply(false);
    f.web.apply(true);
    let header = format!("{COOKIE}={cookie}");
    let host = f.host();
    let own = format!("http://{host}");
    assert_eq!(
        get(f.port(), &host, "/", &[("Cookie", &header)]).body,
        get(f.port(), &host, "/", &[]).body
    );
    assert_eq!(upgrade(&f, &host, Some(&own), Some(&cookie)).status, 403);
}

#[test]
fn a_sessions_file_from_an_earlier_build_is_deleted() {
    let dir = tempfile::tempdir().unwrap();
    let legacy = dir.path().join("web-sessions.json");
    std::fs::write(&legacy, "{}").unwrap();
    let session = Session::load_with_browser(dir.path().join("queue.json"), 2, None).unwrap();
    let _web = Web::new(
        Engine::new(Arc::new(session)),
        dir.path().to_path_buf(),
        Arc::new(AtomicUsize::new(0)),
    );
    assert!(!legacy.exists());
}
