//! The desktop as a client of a real engine (FP-055): its connection starts
//! and follows the engine, and when the engine is killed mid-download the
//! window is told, a new engine is started, and the download finishes from
//! its checkpoint.
//!
//! Needs `fetchpath.exe` built beside the test binaries (`cargo build -p
//! fetchpath`, which `cargo test --workspace` does).
#![cfg(windows)]

use fetchpath_desktop_lib::engine_link::{self, EngineLink, Signal};
use fetchpath_desktop_lib::view;
use fetchpath_protocol::command::{Command, JobFilter};
use fetchpath_protocol::launch::{self, EngineHome};
use fetchpath_protocol::message::CommandResult;
use fetchpath_protocol::pipe::Limits;
use fetchpath_protocol::{JobSnapshot, Timestamp};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Command as Process, Stdio};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

struct TestEngine(std::process::Child);

impl Drop for TestEngine {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn start_test_engine(home: &EngineHome) -> TestEngine {
    let engine = TestEngine(
        Process::new(engine_exe())
            .args(["engine", "--idle-grace-ms", "30000"])
            .env("FETCHPATH_APP_DATA_DIR", home.dir())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(15);
    while launch::attach(home, Limits::default()).is_err() {
        assert!(Instant::now() < deadline, "test engine never started");
        thread::sleep(Duration::from_millis(50));
    }
    engine
}

fn replacement_instance_is_refused(watch_reconnect: bool) {
    let dir = tempfile::tempdir().unwrap();
    let home = EngineHome::at(dir.path().join("data"));
    let first = start_test_engine(&home);
    let link = Arc::new(EngineLink::new(home.clone(), Some(engine_exe())));
    link.send(Command::QueueStats).unwrap();
    drop(first);
    // A different identity behind the same endpoint must not become selected
    // merely because a connection failed or a watcher attached again.
    std::fs::write(
        home.instance_path(),
        fetchpath_protocol::InstanceId::random().as_str(),
    )
    .unwrap();
    let _replacement = start_test_engine(&home);
    if watch_reconnect {
        let (tell, signals) = mpsc::channel();
        let watcher = engine_link::watch(Arc::clone(&link), move |signal| {
            let _ = tell.send(signal);
        });
        expect(
            &signals,
            |signal| matches!(signal, Signal::Disconnected { .. }),
            "replacement instance refused by watcher",
        );
        watcher.stop();
        thread::sleep(Duration::from_millis(1_500));
        assert!(!signals.try_iter().any(|signal| signal == Signal::Connected));
        let error = link.send(Command::QueueStats).unwrap_err();
        assert_eq!(error.code.as_str(), "contract.wrong_instance");
    }
    let error = link.send(Command::EngineShutdown).unwrap_err();
    assert_eq!(error.code.as_str(), "contract.wrong_instance");
    // The rejected mutation did not stop the replacement engine.
    assert!(launch::attach(&home, Limits::default()).is_ok());
}

#[test]
fn command_retry_keeps_the_selected_instance_after_connection_loss() {
    replacement_instance_is_refused(false);
}

#[test]
fn watcher_reconnection_keeps_the_selected_instance() {
    replacement_instance_is_refused(true);
}

fn engine_exe() -> PathBuf {
    let exe = std::env::current_exe()
        .unwrap()
        .parent()
        .and_then(|deps| deps.parent())
        .unwrap()
        .join("fetchpath.exe");
    assert!(
        exe.is_file(),
        "{} is missing; run `cargo build -p fetchpath` first",
        exe.display()
    );
    exe
}

/// Serves `body` slowly with ranges and a strong validator, so a download
/// can be interrupted and resumed.
fn server(body: Vec<u8>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let body = body.clone();
            thread::spawn(move || {
                let mut request = Vec::new();
                let mut buffer = [0_u8; 1024];
                while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                    match stream.read(&mut buffer) {
                        Ok(0) | Err(_) => return,
                        Ok(read) => request.extend_from_slice(&buffer[..read]),
                    }
                }
                let text = String::from_utf8_lossy(&request).to_ascii_lowercase();
                let start = text
                    .lines()
                    .find_map(|line| line.strip_prefix("range: bytes="))
                    .and_then(|range| range.split('-').next())
                    .and_then(|start| start.trim().parse::<usize>().ok());
                let end = text
                    .lines()
                    .find_map(|line| line.strip_prefix("range: bytes="))
                    .and_then(|range| range.split('-').nth(1))
                    .and_then(|end| end.trim().parse::<usize>().ok())
                    .unwrap_or(body.len() - 1)
                    .min(body.len() - 1);
                let head = match start {
                    Some(start) => format!(
                        "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: bytes {start}-{end}/{}\r\nETag: \"fixed\"\r\nAccept-Ranges: bytes\r\nConnection: close\r\n\r\n",
                        end - start + 1,
                        body.len()
                    ),
                    None => format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nETag: \"fixed\"\r\nAccept-Ranges: bytes\r\nConnection: close\r\n\r\n",
                        body.len()
                    ),
                };
                if stream.write_all(head.as_bytes()).is_err() {
                    return;
                }
                let to = if start.is_some() { end + 1 } else { body.len() };
                for chunk in body[start.unwrap_or(0)..to].chunks(16 * 1024) {
                    if stream.write_all(chunk).is_err() {
                        return;
                    }
                    thread::sleep(Duration::from_millis(10));
                }
            });
        }
    });
    base
}

fn expect(signals: &Receiver<Signal>, wanted: fn(&Signal) -> bool, what: &str) {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match signals.recv_timeout(left) {
            Ok(signal) if wanted(&signal) => return,
            Ok(_) => {}
            Err(_) => panic!("the window was never told: {what}"),
        }
    }
}

fn job(link: &EngineLink) -> JobSnapshot {
    match link
        .send(Command::ListJobs {
            filter: JobFilter::All,
        })
        .unwrap()
    {
        CommandResult::Jobs { mut jobs } => jobs.remove(0),
        other => panic!("{other:?}"),
    }
}

#[test]
fn the_window_follows_the_engine_and_recovers_when_it_is_killed() {
    let dir = tempfile::tempdir().unwrap();
    let home = EngineHome::at(dir.path().join("data"));
    let exe = engine_exe();
    let body: Vec<u8> = (0..4_000_000_u32)
        .map(|index| (index % 251) as u8)
        .collect();
    let base = server(body.clone());

    // An engine the test can kill.
    let mut first = Process::new(&exe)
        .args(["engine", "--idle-grace-ms", "2000"])
        .env("FETCHPATH_APP_DATA_DIR", home.dir())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    while launch::attach(&home, Limits::default()).is_err() {
        assert!(Instant::now() < deadline, "the engine never started");
        thread::sleep(Duration::from_millis(50));
    }

    let link = Arc::new(EngineLink::new(home.clone(), Some(exe)));
    let (tell, signals) = mpsc::channel();
    let watcher = engine_link::watch(Arc::clone(&link), move |signal| {
        let _ = tell.send(signal);
    });
    expect(&signals, |signal| *signal == Signal::Connected, "connected");

    // Added the way the Add download dialog adds a batch.
    let destination = dir.path().join("big.bin");
    let draft: view::JobDraft = serde_json::from_value(serde_json::json!({
        "url": format!("{base}/big.bin"),
        "destination": destination.display().to_string(),
        "notBeforeMs": null,
        "checksum": "",
    }))
    .unwrap();
    let created = link
        .send(Command::CreateJobs {
            requests: vec![draft.request().unwrap()],
        })
        .unwrap();
    assert!(matches!(created, CommandResult::Jobs { ref jobs } if jobs.len() == 1));
    expect(
        &signals,
        |signal| *signal == Signal::Queue,
        "the queue changed",
    );

    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let now = job(&link);
        if view::job(&now, Timestamp::now()).state == "running"
            && now.progress.bytes_received > 500_000
        {
            break;
        }
        assert!(Instant::now() < deadline, "never running: {now:?}");
        thread::sleep(Duration::from_millis(20));
    }

    // Killed, not stopped: nothing is saved on the way out.
    first.kill().unwrap();
    first.wait().unwrap();
    expect(
        &signals,
        // It died rather than stopping, so the window starts another.
        |signal| matches!(signal, Signal::Disconnected { stopped: false, .. }),
        "the engine was lost",
    );
    expect(
        &signals,
        |signal| *signal == Signal::Connected,
        "a new engine is running",
    );

    let deadline = Instant::now() + Duration::from_secs(40);
    let done = loop {
        let now = job(&link);
        if now.state == fetchpath_protocol::model::JobState::Completed {
            break now;
        }
        assert!(Instant::now() < deadline, "never finished: {now:?}");
        thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(done.destination, Some(destination.display().to_string()));
    assert_eq!(std::fs::read(&destination).unwrap(), body);

    // Leave nothing running: stop following, then stop the engine the link
    // started, and wait for it to let go of its folder.
    watcher.stop();
    thread::sleep(Duration::from_millis(1_500));
    let _ = link.send(Command::EngineShutdown);
    let deadline = Instant::now() + Duration::from_secs(20);
    while launch::attach(&home, Limits::default()).is_ok() {
        assert!(Instant::now() < deadline, "the engine did not stop");
        thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn an_engine_stopped_on_purpose_is_not_started_again_until_the_person_asks() {
    let dir = tempfile::tempdir().unwrap();
    let home = EngineHome::at(dir.path().join("data"));
    let link = Arc::new(EngineLink::new(home.clone(), Some(engine_exe())));
    let (tell, signals) = mpsc::channel();
    let watcher = engine_link::watch(Arc::clone(&link), move |signal| {
        let _ = tell.send(signal);
    });
    // The window starts the engine it opens with.
    expect(&signals, |signal| *signal == Signal::Connected, "connected");

    // As `fetchpath engine stop` or an installer would.
    assert!(matches!(
        link.send(Command::EngineShutdown).unwrap(),
        CommandResult::ShuttingDown
    ));
    expect(
        &signals,
        |signal| matches!(signal, Signal::Disconnected { stopped: true, .. }),
        "the engine was stopped on purpose",
    );
    // Nothing starts it again by itself, nor does a command from the page.
    let until = Instant::now() + Duration::from_secs(4);
    while Instant::now() < until {
        assert!(
            launch::attach(&home, Limits::default()).is_err(),
            "the window started a stopped engine again"
        );
        thread::sleep(Duration::from_millis(200));
    }
    assert!(link.send(Command::QueueStats).is_err());

    // The person presses Start.
    link.allow_start();
    expect(
        &signals,
        |signal| *signal == Signal::Connected,
        "started again",
    );

    watcher.stop();
    thread::sleep(Duration::from_millis(1_500));
    let _ = link.send(Command::EngineShutdown);
    let deadline = Instant::now() + Duration::from_secs(20);
    while launch::attach(&home, Limits::default()).is_ok() {
        assert!(Instant::now() < deadline, "the engine did not stop");
        thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn an_engine_that_will_not_start_is_not_retried_forever() {
    let dir = tempfile::tempdir().unwrap();
    let home = EngineHome::at(dir.path().join("data"));
    // No program where the engine should be.
    let link = Arc::new(EngineLink::new(
        home,
        Some(dir.path().join("missing").join("fetchpath.exe")),
    ));
    let (tell, signals) = mpsc::channel();
    let watcher = engine_link::watch(link, move |signal| {
        let _ = tell.send(signal);
    });
    expect(
        &signals,
        |signal| matches!(signal, Signal::Disconnected { stopped: false, .. }),
        "the first failed start",
    );
    expect(
        &signals,
        |signal| matches!(signal, Signal::Disconnected { stopped: true, .. }),
        "giving up after repeated failed starts",
    );
    watcher.stop();
}
