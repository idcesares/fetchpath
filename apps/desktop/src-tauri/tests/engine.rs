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
        |signal| matches!(signal, Signal::Disconnected(_)),
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
