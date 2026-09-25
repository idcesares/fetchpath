//! `fetchpath engine` (FP-053), run as its own process against a temporary
//! data folder: one owner, launch or attach, recovery after a kill, idle exit
//! and version refusal.
#![cfg(windows)]

use fetchpath_protocol::command::{
    Command, CommandEnvelope, ConflictPolicy, DestinationIntent, JobFilter, JobInput, JobRequest,
};
use fetchpath_protocol::launch::{self, EngineHome};
use fetchpath_protocol::message::CommandResult;
use fetchpath_protocol::model::JobState;
use fetchpath_protocol::pipe::{EngineSecret, Limits, PipeClient, endpoint};
use fetchpath_protocol::{ClientId, EngineClient, JobSnapshot, SensitiveUrl, Timestamp};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command as Process, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

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

fn attached(home: &EngineHome) -> fetchpath_protocol::pipe::PipeEngineClient {
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

fn create(url: &str, destination: PathBuf, not_before: Option<Timestamp>) -> Command {
    Command::CreateJob {
        request: JobRequest::File {
            input: JobInput::Url {
                url: SensitiveUrl::try_from(url.to_owned()).unwrap(),
            },
            destination: DestinationIntent {
                path: destination.display().to_string(),
                conflict: ConflictPolicy::Ask,
            },
            not_before,
            expected_sha256: None,
        },
    }
}

fn jobs(client: &impl EngineClient) -> Vec<JobSnapshot> {
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

#[test]
fn two_engines_started_together_leave_exactly_one_owner() {
    let dir = tempfile::tempdir().unwrap();
    let home = EngineHome::at(dir.path().to_path_buf());
    let mut first = engine(&home, 30_000);
    let mut second = engine(&home, 30_000);
    let client = attached(&home);
    client
        .send(&ClientId::random(), Command::EngineStatus)
        .unwrap();
    // One of them found the lock held and left at once, successfully.
    let deadline = Instant::now() + Duration::from_secs(10);
    let (winner, loser_code) = loop {
        if let Some(status) = first.try_wait().unwrap() {
            break (&mut second, status.code());
        }
        if let Some(status) = second.try_wait().unwrap() {
            break (&mut first, status.code());
        }
        assert!(Instant::now() < deadline, "both engines are still running");
        thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(loser_code, Some(0));
    assert!(
        winner.try_wait().unwrap().is_none(),
        "the owner must keep running"
    );
    client
        .send(&ClientId::random(), Command::EngineShutdown)
        .unwrap();
    assert_eq!(exited_within(winner, Duration::from_secs(10)), Some(0));
}

#[test]
fn a_client_starts_a_windowless_engine_when_none_is_running() {
    let dir = tempfile::tempdir().unwrap();
    let home = EngineHome::at(dir.path().to_path_buf());
    assert_eq!(
        launch::attach(&home, Limits::default())
            .err()
            .unwrap()
            .code
            .as_str(),
        "contract.engine_unavailable"
    );
    let client = launch::attach_or_launch(
        &home,
        Path::new(EXE),
        Limits::default(),
        launch::LAUNCH_WAIT,
    )
    .unwrap();
    assert!(matches!(
        client
            .send(&ClientId::random(), Command::EngineStatus)
            .unwrap(),
        CommandResult::EngineStatus { .. }
    ));
    // A second client attaches to the same engine rather than starting one.
    let other = launch::attach_or_launch(
        &home,
        Path::new(EXE),
        Limits::default(),
        launch::LAUNCH_WAIT,
    )
    .unwrap();
    let a = match client
        .send(&ClientId::random(), Command::EngineStatus)
        .unwrap()
    {
        CommandResult::EngineStatus { status } => status.started_at,
        other => panic!("{other:?}"),
    };
    let b = match other
        .send(&ClientId::random(), Command::EngineStatus)
        .unwrap()
    {
        CommandResult::EngineStatus { status } => status.started_at,
        other => panic!("{other:?}"),
    };
    assert_eq!(a, b, "both clients reached the same engine");
    client
        .send(&ClientId::random(), Command::EngineShutdown)
        .unwrap();
}

#[test]
fn the_engine_leaves_when_idle_but_stays_for_a_scheduled_job() {
    let dir = tempfile::tempdir().unwrap();
    let home = EngineHome::at(dir.path().to_path_buf());
    let mut idle = engine(&home, 500);
    attached(&home);
    // The probing connection closed; nothing to do: it leaves after 0.5 s.
    assert_eq!(exited_within(&mut idle, Duration::from_secs(10)), Some(0));

    let mut busy = engine(&home, 500);
    let client = attached(&home);
    let later = Timestamp::from_unix_ms(Timestamp::now().unix_ms() + 3_600_000);
    let job = match client
        .execute(&CommandEnvelope::new(
            ClientId::random(),
            create(
                "http://127.0.0.1:9/later.bin",
                dir.path().join("later.bin"),
                Some(later),
            ),
        ))
        .unwrap()
    {
        CommandResult::Job { job } => job,
        other => panic!("{other:?}"),
    };
    drop(client);
    // No client, but a scheduled job: it stays well past the grace period.
    assert_eq!(exited_within(&mut busy, Duration::from_secs(3)), None);
    let client = attached(&home);
    client
        .send(
            &ClientId::random(),
            Command::RemoveJob { job_id: job.job_id },
        )
        .unwrap();
    drop(client);
    assert_eq!(exited_within(&mut busy, Duration::from_secs(10)), Some(0));

    // A connected client keeps it too, even one that only listens.
    let mut watched = engine(&home, 500);
    let client = attached(&home);
    let subscription = client
        .subscribe(&CommandEnvelope::new(
            ClientId::random(),
            Command::SubscribeQueue { after_cursor: 0 },
        ))
        .unwrap();
    assert_eq!(exited_within(&mut watched, Duration::from_secs(3)), None);
    drop(subscription);
    drop(client);
    assert_eq!(
        exited_within(&mut watched, Duration::from_secs(10)),
        Some(0)
    );
}

/// Serves one body with a strong ETag and byte ranges, slowly, and counts
/// requests that resumed with `If-Range`.
fn resumable(body: Vec<u8>) -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/file.bin", listener.local_addr().unwrap());
    let resumed = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&resumed);
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let body = body.clone();
            let counter = Arc::clone(&counter);
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
                if text.contains("if-range:") {
                    counter.fetch_add(1, Ordering::SeqCst);
                }
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
                let from = start.unwrap_or(0);
                let to = if start.is_some() { end + 1 } else { body.len() };
                for chunk in body[from..to].chunks(32 * 1024) {
                    if stream.write_all(chunk).is_err() {
                        return;
                    }
                    thread::sleep(Duration::from_millis(20));
                }
            });
        }
    });
    (url, resumed)
}

#[test]
fn an_engine_killed_mid_download_resumes_it_when_started_again() {
    let dir = tempfile::tempdir().unwrap();
    let home = EngineHome::at(dir.path().to_path_buf());
    let body: Vec<u8> = (0..6 * 1024 * 1024).map(|i| (i * 31 % 251) as u8).collect();
    let (url, resumed) = resumable(body.clone());
    let destination = dir.path().join("big.bin");

    let mut first = engine(&home, 30_000);
    let client = attached(&home);
    client
        .execute(&CommandEnvelope::new(
            ClientId::random(),
            create(&url, destination.clone(), None),
        ))
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let job = jobs(&client).remove(0);
        if job.state == JobState::Running && job.progress.bytes_received > 1024 * 1024 {
            break;
        }
        assert!(Instant::now() < deadline, "{job:?}");
        thread::sleep(Duration::from_millis(50));
    }
    // Killed outright, as a crash would: nothing is saved on the way out.
    first.kill().unwrap();
    let _ = first.wait();
    drop(client);

    let mut second = engine(&home, 30_000);
    let client = attached(&home);
    let deadline = Instant::now() + Duration::from_secs(60);
    let finished = loop {
        let job = jobs(&client).remove(0);
        if job.state == JobState::Completed {
            break job;
        }
        assert!(
            matches!(job.state, JobState::Queued | JobState::Running),
            "recovery must bring the download back, not fail it: {job:?}"
        );
        assert!(Instant::now() < deadline, "{job:?}");
        thread::sleep(Duration::from_millis(100));
    };
    assert_eq!(std::fs::read(&destination).unwrap(), body);
    assert!(finished.observed_sha256.is_some());
    assert!(
        resumed.load(Ordering::SeqCst) > 0,
        "the relaunch resumed from its checkpoint"
    );
    client
        .send(&ClientId::random(), Command::EngineShutdown)
        .unwrap();
    assert_eq!(exited_within(&mut second, Duration::from_secs(10)), Some(0));
}

#[test]
fn another_protocol_version_is_refused_and_can_still_restart_the_engine() {
    let dir = tempfile::tempdir().unwrap();
    let home = EngineHome::at(dir.path().to_path_buf());
    let mut running = engine(&home, 30_000);
    attached(&home);
    let name = endpoint::read(&home.endpoint_path()).unwrap();
    let secret = EngineSecret::load(&home.secret_path()).unwrap();
    let pipe =
        PipeClient::connect(&name, &secret, Limits::default(), Duration::from_secs(5)).unwrap();
    let mut future = CommandEnvelope::new(ClientId::random(), Command::EngineStatus);
    future.schema_version = 2;
    let refused = pipe.call(&future, Duration::from_secs(5)).unwrap_err();
    assert_eq!(refused.code.as_str(), "contract.unsupported_version");
    drop(pipe);

    // A client of any version can ask it to make way for a new one.
    launch::request_restart(&home, Limits::default(), Duration::from_secs(10)).unwrap();
    assert_eq!(
        exited_within(&mut running, Duration::from_secs(10)),
        Some(0)
    );
    let client = launch::attach_or_launch(
        &home,
        Path::new(EXE),
        Limits::default(),
        launch::LAUNCH_WAIT,
    )
    .unwrap();
    assert_ne!(
        endpoint::read(&home.endpoint_path()).unwrap(),
        name,
        "a new engine publishes a new pipe name"
    );
    client
        .send(&ClientId::random(), Command::EngineShutdown)
        .unwrap();
}

#[test]
fn once_stopping_the_engine_refuses_commands_and_a_new_client_gets_a_new_engine() {
    let dir = tempfile::tempdir().unwrap();
    let home = EngineHome::at(dir.path().to_path_buf());
    let mut stopping = engine(&home, 30_000);
    let bystander = attached(&home);
    bystander
        .send(&ClientId::random(), Command::EngineStatus)
        .unwrap();
    let stopper = attached(&home);
    stopper
        .send(&ClientId::random(), Command::EngineShutdown)
        .unwrap();
    // Acknowledged: nothing more is carried out on any connection.
    let refused = bystander
        .send(
            &ClientId::random(),
            Command::ListJobs {
                filter: JobFilter::All,
            },
        )
        .unwrap_err();
    assert_eq!(refused.code.as_str(), "contract.engine_unavailable");

    // A client arriving while it winds down ends up on a fresh engine.
    let fresh = launch::attach_or_launch(
        &home,
        Path::new(EXE),
        Limits::default(),
        launch::LAUNCH_WAIT,
    )
    .unwrap();
    fresh
        .send(&ClientId::random(), Command::EngineStatus)
        .unwrap();
    assert_eq!(
        exited_within(&mut stopping, Duration::from_secs(10)),
        Some(0)
    );
    fresh
        .send(&ClientId::random(), Command::EngineShutdown)
        .unwrap();
}
