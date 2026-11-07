//! Relay behavior on loopback sockets, with a tiny origin standing in for the
//! engine's share handler: it echoes the request head it received and then
//! streams a body.

use fetchpath_relay::client::{self, Connector, Handler, Settings, Status};
use fetchpath_relay::server::{Event, Limits, Relay};
use fetchpath_relay::wire::EnrollmentKey;
use std::io;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;

const ID: &str = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQ";
const MARKER: &str = "PLAINTEXT-MARKER-7f3a";

struct Rig {
    engines: std::net::SocketAddr,
    visitors: std::net::SocketAddr,
    events: Arc<Mutex<Vec<Event>>>,
    key: EnrollmentKey,
}

async fn rig() -> Rig {
    let key = EnrollmentKey::generate().unwrap();
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&events);
    let relay = Relay::new(key.clone(), Limits::default(), move |event| {
        sink.lock().unwrap().push(event);
    });
    let engine_side = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let visitor_side = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (engines, visitors) = (
        engine_side.local_addr().unwrap(),
        visitor_side.local_addr().unwrap(),
    );
    let for_engines = Arc::clone(&relay);
    tokio::spawn(async move {
        while let Ok((stream, _)) = engine_side.accept().await {
            tokio::spawn(Arc::clone(&for_engines).serve_engine(stream));
        }
    });
    tokio::spawn(async move {
        while let Ok((stream, _)) = visitor_side.accept().await {
            tokio::spawn(Arc::clone(&relay).serve_visitor(stream));
        }
    });
    Rig {
        engines,
        visitors,
        events,
        key,
    }
}

fn connector(address: std::net::SocketAddr) -> Connector {
    Arc::new(move || {
        Box::pin(async move {
            let stream = TcpStream::connect(address).await?;
            Ok(Box::new(stream) as fetchpath_relay::server::Conn)
        })
    })
}

/// Reads one request head, then answers with the head it saw (as the body's
/// first part) followed by `extra` bytes, and closes.
fn origin(extra: usize) -> Handler {
    Arc::new(move |mut conn| {
        Box::pin(async move {
            let mut head = Vec::new();
            let mut chunk = [0u8; 512];
            while !head.windows(4).any(|window| window == b"\r\n\r\n") {
                match conn.read(&mut chunk).await {
                    Ok(0) | Err(_) => return,
                    Ok(read) => head.extend_from_slice(&chunk[..read]),
                }
            }
            let length = head.len() + extra;
            let response =
                format!("HTTP/1.1 200 OK\r\nContent-Length: {length}\r\nConnection: close\r\n\r\n");
            let _ = conn.write_all(response.as_bytes()).await;
            let _ = conn.write_all(&head).await;
            let filler = vec![b'x'; 64 * 1024];
            let mut left = extra;
            while left > 0 {
                let part = left.min(filler.len());
                if conn.write_all(&filler[..part]).await.is_err() {
                    return;
                }
                left -= part;
            }
            let _ = conn.shutdown().await;
        })
    })
}

struct Engine {
    status: watch::Receiver<Status>,
    ids: watch::Sender<Vec<String>>,
    task: tokio::task::JoinHandle<()>,
}

fn start_engine(rig: &Rig, key: EnrollmentKey, extra: usize, ids: &[&str]) -> Engine {
    let (ids_tx, ids_rx) = watch::channel(ids.iter().map(|id| (*id).to_owned()).collect());
    let (status_tx, status_rx) = watch::channel(Status::Connecting);
    let task = tokio::spawn(client::run(
        Settings::new(key, "laptop"),
        connector(rig.engines),
        ids_rx,
        origin(extra),
        status_tx,
    ));
    Engine {
        status: status_rx,
        ids: ids_tx,
        task,
    }
}

async fn enrolled(engine: &mut Engine) {
    tokio::time::timeout(
        Duration::from_secs(5),
        engine.status.wait_for(|status| *status == Status::Enrolled),
    )
    .await
    .expect("enrolled in time")
    .unwrap();
    // The announce and the first work connections reach the relay.
    tokio::time::sleep(Duration::from_millis(300)).await;
}

async fn get(address: std::net::SocketAddr, request: &str) -> io::Result<Vec<u8>> {
    let mut stream = TcpStream::connect(address).await?;
    stream.write_all(request.as_bytes()).await?;
    let mut body = Vec::new();
    tokio::time::timeout(Duration::from_secs(10), stream.read_to_end(&mut body))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "no end"))??;
    Ok(body)
}

fn request(path: &str) -> String {
    format!(
        "GET {path} HTTP/1.1\r\nHost: share.example\r\nRange: bytes=0-\r\nX-Probe: {MARKER}\r\n\r\n"
    )
}

#[tokio::test]
async fn a_visitor_reaches_the_engine_and_the_head_is_forwarded_unchanged() {
    let rig = rig().await;
    let mut engine = start_engine(&rig, rig.key.clone(), 1000, &[ID]);
    enrolled(&mut engine).await;
    let asked = request(&format!("/s/{ID}?probe=1"));
    let answer = get(rig.visitors, &asked).await.unwrap();
    let text = String::from_utf8_lossy(&answer);
    assert!(text.starts_with("HTTP/1.1 200 OK"), "{text}");
    // The origin echoes the head it saw: byte for byte what the visitor sent.
    assert!(text.contains(&asked), "head changed in transit");
    assert_eq!(
        answer.len() - text.find("\r\n\r\n").unwrap() - 4,
        asked.len() + 1000
    );
    engine.task.abort();
}

#[tokio::test]
async fn a_large_body_passes_intact() {
    let rig = rig().await;
    let mut engine = start_engine(&rig, rig.key.clone(), 6 * 1024 * 1024, &[ID]);
    enrolled(&mut engine).await;
    let asked = request(&format!("/s/{ID}"));
    let answer = get(rig.visitors, &asked).await.unwrap();
    let start = answer.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
    assert_eq!(answer.len() - start, asked.len() + 6 * 1024 * 1024);
    assert!(
        answer[start + asked.len()..]
            .iter()
            .all(|byte| *byte == b'x')
    );
    engine.task.abort();
}

#[tokio::test]
async fn the_pool_is_topped_up_for_the_next_visitor() {
    let rig = rig().await;
    let mut engine = start_engine(&rig, rig.key.clone(), 10, &[ID]);
    enrolled(&mut engine).await;
    for _ in 0..8 {
        let answer = get(rig.visitors, &request(&format!("/s/{ID}")))
            .await
            .unwrap();
        assert!(String::from_utf8_lossy(&answer).starts_with("HTTP/1.1 200"));
    }
    engine.task.abort();
}

#[tokio::test]
async fn only_share_paths_are_routed() {
    let rig = rig().await;
    let mut engine = start_engine(&rig, rig.key.clone(), 10, &[ID]);
    enrolled(&mut engine).await;
    for path in ["/", "/s/", "/admin", "/s/../etc", "/s/a b"] {
        let answer = get(rig.visitors, &request(path)).await.unwrap();
        let text = String::from_utf8_lossy(&answer);
        assert!(
            text.starts_with("HTTP/1.1 404") || text.starts_with("HTTP/1.1 400"),
            "{path}: {text}"
        );
    }
    // A POST for a real id is not routed either.
    let post = format!("POST /s/{ID} HTTP/1.1\r\nHost: h\r\nContent-Length: 0\r\n\r\n");
    let answer = get(rig.visitors, &post).await.unwrap();
    assert!(String::from_utf8_lossy(&answer).starts_with("HTTP/1.1 404"));
    engine.task.abort();
}

#[tokio::test]
async fn an_unannounced_id_and_a_revoked_id_get_a_bad_gateway() {
    let rig = rig().await;
    let mut engine = start_engine(&rig, rig.key.clone(), 10, &[ID]);
    enrolled(&mut engine).await;
    let other = "z".repeat(43);
    let answer = get(rig.visitors, &request(&format!("/s/{other}")))
        .await
        .unwrap();
    assert!(String::from_utf8_lossy(&answer).starts_with("HTTP/1.1 502"));
    engine.ids.send(Vec::new()).unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let answer = get(rig.visitors, &request(&format!("/s/{ID}")))
        .await
        .unwrap();
    assert!(String::from_utf8_lossy(&answer).starts_with("HTTP/1.1 502"));
    engine.task.abort();
}

#[tokio::test]
async fn an_engine_with_the_wrong_key_is_refused_and_serves_nothing() {
    let rig = rig().await;
    let stranger = EnrollmentKey::generate().unwrap();
    let mut engine = start_engine(&rig, stranger, 10, &[ID]);
    tokio::time::timeout(
        Duration::from_secs(5),
        engine.status.wait_for(|status| *status == Status::Refused),
    )
    .await
    .expect("refused in time")
    .unwrap();
    let answer = get(rig.visitors, &request(&format!("/s/{ID}")))
        .await
        .unwrap();
    assert!(String::from_utf8_lossy(&answer).starts_with("HTTP/1.1 502"));
    assert!(rig.events.lock().unwrap().contains(&Event::EngineRefused));
    engine.task.abort();
}

#[tokio::test]
async fn an_engine_that_leaves_loses_its_ids() {
    let rig = rig().await;
    let mut engine = start_engine(&rig, rig.key.clone(), 10, &[ID]);
    enrolled(&mut engine).await;
    engine.task.abort();
    // Aborting drops the control connection; the relay notices on EOF.
    tokio::time::sleep(Duration::from_millis(500)).await;
    let answer = get(rig.visitors, &request(&format!("/s/{ID}")))
        .await
        .unwrap();
    assert!(String::from_utf8_lossy(&answer).starts_with("HTTP/1.1 502"));
}

#[tokio::test]
async fn the_observer_never_sees_a_secret_or_a_byte_of_traffic() {
    let rig = rig().await;
    let mut engine = start_engine(&rig, rig.key.clone(), 100, &[ID]);
    enrolled(&mut engine).await;
    let _ = get(rig.visitors, &request(&format!("/s/{ID}")))
        .await
        .unwrap();
    let _ = get(rig.visitors, &request("/admin")).await.unwrap();
    let stranger = start_engine(&rig, EnrollmentKey::generate().unwrap(), 1, &[ID]);
    tokio::time::sleep(Duration::from_millis(300)).await;
    let events = rig.events.lock().unwrap();
    // Both renderings: the binary prints `Display`, tests and tools use `Debug`.
    let logged = format!(
        "{events:?} {}",
        events
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    );
    for secret in [MARKER, ID, "share.example", &rig.key.to_hex()] {
        assert!(!logged.contains(secret), "{secret} reached the observer");
    }
    drop(events);
    assert!(!format!("{:?}", rig.key).contains(&rig.key.to_hex()));
    engine.task.abort();
    stranger.task.abort();
}

#[tokio::test]
async fn a_second_control_connection_of_a_name_replaces_and_closes_the_first() {
    let rig = rig().await;
    let mut first = start_engine(&rig, rig.key.clone(), 10, &[ID]);
    enrolled(&mut first).await;
    let mut second = start_engine(&rig, rig.key.clone(), 10, &[ID]);
    enrolled(&mut second).await;
    // The relay closed the first control connection, so it leaves Enrolled.
    tokio::time::timeout(
        Duration::from_secs(5),
        first.status.wait_for(|status| *status != Status::Enrolled),
    )
    .await
    .expect("first engine told it was replaced")
    .unwrap();
    first.task.abort();
    second.task.abort();
}

#[tokio::test]
async fn idle_work_connections_are_recycled_before_they_go_stale() {
    let rig = rig().await;
    let (ids_tx, ids_rx) = watch::channel(vec![ID.to_owned()]);
    let (status_tx, mut status) = watch::channel(Status::Connecting);
    let mut settings = Settings::new(rig.key.clone(), "laptop");
    settings.recycle = Duration::from_millis(200);
    let task = tokio::spawn(client::run(
        settings,
        connector(rig.engines),
        ids_rx,
        origin(10),
        status_tx,
    ));
    status.wait_for(|s| *s == Status::Enrolled).await.unwrap();
    // Several recycle periods pass; visitors still get a live connection.
    tokio::time::sleep(Duration::from_millis(900)).await;
    let answer = get(rig.visitors, &request(&format!("/s/{ID}")))
        .await
        .unwrap();
    assert!(String::from_utf8_lossy(&answer).starts_with("HTTP/1.1 200"));
    drop(ids_tx);
    task.abort();
}
