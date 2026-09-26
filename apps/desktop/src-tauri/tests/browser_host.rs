//! The browser host hands captures to the engine without the window (FP-056):
//! the real host and engine as processes, from a folder with no desktop app
//! beside them, each run with its own data folder.
//!
//! Needs `fetchpath.exe` built beside the test binaries (`cargo build -p
//! fetchpath`, which `cargo test --workspace` does). Sources are on the
//! reserved `.test` domain, so no download ever reaches the network or the
//! Downloads folder.
#![cfg(windows)]

use fetchpath_protocol::command::{Command, JobFilter};
use fetchpath_protocol::launch::{self, EngineHome};
use fetchpath_protocol::message::CommandResult;
use fetchpath_protocol::pipe::Limits;
use fetchpath_protocol::{ClientId, EngineClient, JobSnapshot};
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command as Process, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const CALLER: &str = "chrome-extension://lfikhkjdpjcjaboanknaabncpkbgoele/";

fn built(name: &str) -> PathBuf {
    let exe = std::env::current_exe()
        .unwrap()
        .parent()
        .and_then(|deps| deps.parent())
        .unwrap()
        .join(name);
    assert!(
        exe.is_file(),
        "{} is missing; run `cargo build -p fetchpath` first",
        exe.display()
    );
    exe
}

/// The host, and the engine only when asked, as installed: side by side, with
/// no desktop app to wake.
fn install(with_engine: bool) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::copy(
        env!("CARGO_BIN_EXE_fetchpath-browser-host"),
        dir.path().join("fetchpath-browser-host.exe"),
    )
    .unwrap();
    if with_engine {
        std::fs::copy(built("fetchpath.exe"), dir.path().join("fetchpath.exe")).unwrap();
    }
    dir
}

fn capture(url: &str) -> Value {
    json!({
        "schema_version": 1,
        "type": "capture",
        "capture_id": uuid(),
        "method": "GET",
        "url": url,
        "suggested_filename": format!("fp056-{}.zip", uuid()),
        "referrer": null,
        "cookies": [],
        "user_initiated": true,
    })
}

/// A version 4 UUID, as the extension makes capture ids.
fn uuid() -> String {
    ClientId::random().as_str().to_owned()
}

/// One framed request to the host, as a browser sends it; returns its answer
/// and how long the browser waited for it.
fn send(installed: &Path, home: &EngineHome, request: &Value) -> (Value, Duration) {
    let started = Instant::now();
    let mut child = Process::new(installed.join("fetchpath-browser-host.exe"))
        .arg(CALLER)
        .env("FETCHPATH_APP_DATA_DIR", home.dir())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let body = serde_json::to_vec(request).unwrap();
    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(&(body.len() as u32).to_le_bytes()).unwrap();
    stdin.write_all(&body).unwrap();
    drop(stdin);
    let mut output = Vec::new();
    child
        .stdout
        .take()
        .unwrap()
        .read_to_end(&mut output)
        .unwrap();
    assert!(child.wait().unwrap().success());
    let length = u32::from_le_bytes(output[..4].try_into().unwrap()) as usize;
    assert_eq!(output.len(), length + 4);
    (
        serde_json::from_slice(&output[4..]).unwrap(),
        started.elapsed(),
    )
}

fn jobs(home: &EngineHome) -> Vec<JobSnapshot> {
    let client = launch::attach(home, Limits::default()).expect("an engine is running");
    match client
        .send(
            &ClientId::random(),
            Command::ListJobs {
                filter: JobFilter::All,
            },
        )
        .unwrap()
    {
        CommandResult::Jobs { jobs } => jobs,
        other => panic!("{other:?}"),
    }
}

fn wait_for_jobs(home: &EngineHome, count: usize) -> Vec<JobSnapshot> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(client) = launch::attach(home, Limits::default()) {
            drop(client);
            let jobs = jobs(home);
            if jobs.len() >= count {
                return jobs;
            }
        }
        assert!(
            Instant::now() < deadline,
            "the capture never reached the queue"
        );
        thread::sleep(Duration::from_millis(100));
    }
}

fn pending(home: &EngineHome) -> usize {
    fetchpath_browser_inbox::BridgeStore::new(home.dir().to_path_buf())
        .pending()
        .unwrap()
        .len()
}

fn stop(home: &EngineHome) {
    let _ = launch::request_restart(home, Limits::default(), Duration::from_secs(10));
}

/// Stops whatever engine serves `home` when the test ends, passed or not.
struct StopsEngine(EngineHome);

impl Drop for StopsEngine {
    fn drop(&mut self) {
        stop(&self.0);
    }
}

#[test]
fn a_capture_reaches_the_queue_with_no_window_and_no_engine_running() {
    let installed = install(true);
    let data = tempfile::tempdir().unwrap();
    let home = EngineHome::at(data.path().to_path_buf());
    let _stops = StopsEngine(home.clone());
    assert!(
        launch::attach(&home, Limits::default()).is_err(),
        "nothing is running"
    );

    // No engine: the host starts one, which takes the capture in.
    let first = capture("https://files.example.test/one.zip?token=kept-private");
    let (answer, waited) = send(installed.path(), &home, &first);
    assert_eq!(answer["accepted"], true, "{answer}");
    assert!(waited < Duration::from_secs(5), "{waited:?}");
    let listed = wait_for_jobs(&home, 1);
    assert_eq!(listed.len(), 1);
    // Redacted: the signed query stays in the protected context.
    assert_eq!(
        listed[0].source_display,
        "https://files.example.test/one.zip?…"
    );
    assert!(
        !serde_json::to_string(&listed[0])
            .unwrap()
            .contains("kept-private")
    );
    assert_eq!(pending(&home), 0);

    // The same capture again: acknowledged as a duplicate, still one job.
    let (again, _) = send(installed.path(), &home, &first);
    assert_eq!(again["deduplicated"], true, "{again}");
    // A second capture goes to the engine that is now running.
    let (answer, _) = send(
        installed.path(),
        &home,
        &capture("https://files.example.test/two.zip"),
    );
    assert_eq!(answer["accepted"], true);
    assert_eq!(wait_for_jobs(&home, 2).len(), 2);
    thread::sleep(Duration::from_millis(500));
    assert_eq!(jobs(&home).len(), 2, "no duplicate job");
    stop(&home);
}

#[test]
fn a_capture_no_engine_can_take_waits_in_the_inbox_for_the_next_one() {
    let data = tempfile::tempdir().unwrap();
    let home = EngineHome::at(data.path().to_path_buf());
    let _stops = StopsEngine(home.clone());

    // Beside no engine at all: accepted, and kept.
    let bare = install(false);
    let (answer, _) = send(
        bare.path(),
        &home,
        &capture("https://files.example.test/later.zip"),
    );
    assert_eq!(answer["accepted"], true, "{answer}");
    assert_eq!(pending(&home), 1);

    // While setup holds engines off: accepted at once, nothing started.
    let installed = install(true);
    home.hold_for_update().unwrap();
    let (answer, waited) = send(
        installed.path(),
        &home,
        &capture("https://files.example.test/during-setup.zip"),
    );
    assert_eq!(answer["accepted"], true);
    assert!(waited < Duration::from_secs(2), "{waited:?}");
    assert!(launch::attach(&home, Limits::default()).is_err());
    assert_eq!(pending(&home), 2);

    // The next engine, however it starts, takes both in.
    std::fs::remove_file(home.update_hold_path()).unwrap();
    launch::attach_or_launch(
        &home,
        &installed.path().join("fetchpath.exe"),
        Limits::default(),
        launch::LAUNCH_WAIT,
    )
    .unwrap();
    assert_eq!(wait_for_jobs(&home, 2).len(), 2);
    assert_eq!(pending(&home), 0);
    stop(&home);
}

#[test]
fn a_media_page_waits_for_a_window_even_across_engine_restarts() {
    let installed = install(true);
    let data = tempfile::tempdir().unwrap();
    let home = EngineHome::at(data.path().to_path_buf());
    let _stops = StopsEngine(home.clone());
    let page = "https://www.youtube.com/watch?v=fp056";
    let (answer, _) = send(installed.path(), &home, &capture(page));
    assert_eq!(answer["accepted"], true);
    // Taken in by the engine the host started, but not handed out yet.
    let deadline = Instant::now() + Duration::from_secs(10);
    while launch::attach(&home, Limits::default()).is_err() {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(100));
    }
    assert!(jobs(&home).is_empty(), "a page is not saved as a file");
    stop(&home);
    assert_eq!(pending(&home), 1, "kept while no window has taken it");

    let client = launch::attach_or_launch(
        &home,
        &installed.path().join("fetchpath.exe"),
        Limits::default(),
        launch::LAUNCH_WAIT,
    )
    .unwrap();
    let take = || match client
        .send(&ClientId::random(), Command::TakeLinkReviews)
        .unwrap()
    {
        CommandResult::LinkReviews { urls } => urls
            .iter()
            .map(|url| url.expose().to_owned())
            .collect::<Vec<_>>(),
        other => panic!("{other:?}"),
    };
    let deadline = Instant::now() + Duration::from_secs(10);
    let urls = loop {
        let urls = take();
        if !urls.is_empty() || Instant::now() > deadline {
            break urls;
        }
        thread::sleep(Duration::from_millis(100));
    };
    assert_eq!(urls, vec![page.to_owned()]);
    assert!(take().is_empty(), "handed out once");
    assert_eq!(pending(&home), 0);
    stop(&home);
}
