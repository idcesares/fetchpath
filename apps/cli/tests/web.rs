//! The web UI listener of a real engine process (FP-104): who may ask for a
//! sign-in link, and what keeps the engine alive.

use fetchpath_protocol::command::Command;
use fetchpath_protocol::launch::{self, EngineHome};
use fetchpath_protocol::message::CommandResult;
use fetchpath_protocol::pipe::{Limits, PipeEngineClient};
use fetchpath_protocol::principal::Principal;
use fetchpath_protocol::{ClientId, EngineClient};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::process::{Child, Command as Process, Stdio};
use std::thread;
use std::time::{Duration, Instant};
use tokio_tungstenite::tungstenite::{self, Message, client::IntoClientRequest};

const EXE: &str = env!("CARGO_BIN_EXE_fetchpath");

fn engine(home: &EngineHome, grace_ms: u64) -> Child {
    Process::new(EXE)
        .args(["engine", "--idle-grace-ms", &grace_ms.to_string()])
        .env("FETCHPATH_APP_DATA_DIR", home.dir())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap()
}

fn attached(home: &EngineHome) -> PipeEngineClient {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        match launch::attach(home, Limits::default()) {
            Ok(client) => return client,
            Err(error) => {
                assert!(
                    Instant::now() < deadline,
                    "the engine never answered: {error}"
                );
                thread::sleep(Duration::from_millis(50));
            }
        }
    }
}

fn exited_within(child: &mut Child, wait: Duration) -> Option<i32> {
    let deadline = Instant::now() + wait;
    while Instant::now() < deadline {
        if let Some(status) = child.try_wait().unwrap() {
            return Some(status.code().unwrap_or(-1));
        }
        thread::sleep(Duration::from_millis(50));
    }
    None
}

fn turn_web_ui_on(person: &PipeEngineClient) {
    let CommandResult::Settings { view } = person
        .send(&ClientId::random(), Command::GetSettings)
        .unwrap()
    else {
        panic!("no settings");
    };
    let mut settings = view.settings;
    settings.web_ui = Some(true);
    person
        .send(&ClientId::random(), Command::UpdateSettings { settings })
        .unwrap();
}

/// The `host:port` and path of a fresh sign-in link.
fn link(person: &PipeEngineClient) -> (String, String) {
    let CommandResult::WebUiLink { url } = person
        .send(&ClientId::random(), Command::OpenWebUi)
        .unwrap()
    else {
        panic!("no link");
    };
    let url = url.expose();
    let rest = url.strip_prefix("http://").expect("a plain http link");
    let (host, path) = rest.split_at(rest.find('/').unwrap());
    (host.to_owned(), path.to_owned())
}

fn sign_in(host: &str, path: &str) -> String {
    let port: u16 = host.rsplit(':').next().unwrap().parse().unwrap();
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .write_all(
            format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n").as_bytes(),
        )
        .unwrap();
    let mut reply = String::new();
    stream.read_to_string(&mut reply).unwrap();
    assert!(
        reply.starts_with("HTTP/1.1 303"),
        "{}",
        reply.lines().next().unwrap_or("")
    );
    let cookie = reply
        .lines()
        .find_map(|line| {
            line.to_ascii_lowercase()
                .starts_with("set-cookie:")
                .then(|| line.to_owned())
        })
        .expect("a cookie");
    cookie
        .split_once(':')
        .unwrap()
        .1
        .trim()
        .split(';')
        .next()
        .unwrap()
        .to_owned()
}

#[test]
fn only_the_person_can_open_the_web_ui_and_only_a_live_socket_keeps_the_engine() {
    let dir = tempfile::tempdir().unwrap();
    let home = EngineHome::at(dir.path().join("data"));
    let mut running = engine(&home, 1_500);
    let person = attached(&home);

    // Off until turned on.
    let off = person
        .send(&ClientId::random(), Command::OpenWebUi)
        .unwrap_err();
    assert_eq!(off.code.as_str(), "contract.unsupported");
    turn_web_ui_on(&person);

    for principal in ["agent:helper", "browser"] {
        let other = attached(&home).with_principal(Principal::try_from(principal).unwrap());
        for command in [Command::OpenWebUi, Command::SignOutBrowsers] {
            let refused = other.send(&ClientId::random(), command).unwrap_err();
            assert_eq!(refused.code.as_str(), "policy.not_permitted", "{principal}");
        }
    }

    // The listener alone does not keep the engine alive; a signed-in socket
    // does, like an open window.
    let (host, path) = link(&person);
    let cookie = sign_in(&host, &path);
    let mut request = format!("ws://{host}/ui/socket")
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert("origin", format!("http://{host}").parse().unwrap());
    request
        .headers_mut()
        .insert("cookie", cookie.parse().unwrap());
    let port: u16 = host.rsplit(':').next().unwrap().parse().unwrap();
    let (mut socket, _) =
        tungstenite::client(request, TcpStream::connect(("127.0.0.1", port)).unwrap()).unwrap();
    // The person's own pipe connection would count too: let it go first.
    drop(person);
    assert_eq!(
        exited_within(&mut running, Duration::from_secs(4)),
        None,
        "a signed-in socket let the engine go"
    );
    socket.close(None).ok();
    while !matches!(socket.read(), Err(_) | Ok(Message::Close(_))) {}
    drop(socket);
    assert_eq!(
        exited_within(&mut running, Duration::from_secs(15)),
        Some(0),
        "the engine stayed for a closed socket or its listener"
    );
}

#[test]
fn the_listener_alone_does_not_keep_the_engine_running() {
    let dir = tempfile::tempdir().unwrap();
    let home = EngineHome::at(dir.path().join("data"));
    let mut running = engine(&home, 1_000);
    turn_web_ui_on(&attached(&home));
    assert_eq!(
        exited_within(&mut running, Duration::from_secs(15)),
        Some(0),
        "the engine stayed for its listener"
    );
}
