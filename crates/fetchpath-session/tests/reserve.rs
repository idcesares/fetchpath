//! The disk reserve (FP-101 gate G8, contract D6), against the real drive
//! holding a temporary folder. A reserve larger than any drive stands in
//! for a full one: downloads wait or stop at their checkpoint, nothing fails
//! or publishes partly, and they finish once the reserve allows.

use fetchpath_protocol::command::{
    Command, CommandEnvelope, ConflictPolicy, DestinationIntent, JobInput, JobRequest,
};
use fetchpath_protocol::message::CommandResult;
use fetchpath_protocol::model::{JobState, WaitingReason};
use fetchpath_protocol::{ClientId, EngineClient, JobId, JobSnapshot, SensitiveUrl};
use fetchpath_session::Session;
use fetchpath_session::engine::{Engine, InProcessClient};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

/// Larger than any drive.
const FULL: u64 = 1 << 60;

fn send(client: &InProcessClient, command: Command) -> CommandResult {
    client
        .execute(&CommandEnvelope::new(ClientId::random(), command))
        .unwrap()
}

fn reserve(client: &InProcessClient, bytes: u64) {
    let mut settings = match send(client, Command::GetSettings) {
        CommandResult::Settings { view } => view.settings,
        other => panic!("{other:?}"),
    };
    settings.disk_reserve_bytes = Some(bytes);
    send(client, Command::UpdateSettings { settings });
}

fn create(client: &InProcessClient, link: &str, destination: &Path) -> JobId {
    let request = JobRequest::File {
        input: JobInput::Url {
            url: SensitiveUrl::try_from(link.to_owned()).unwrap(),
        },
        destination: DestinationIntent {
            path: destination.display().to_string(),
            conflict: ConflictPolicy::Ask,
        },
        not_before: None,
        expected_sha256: None,
    };
    match send(client, Command::CreateJob { request }) {
        CommandResult::Job { job } => job.job_id,
        other => panic!("{other:?}"),
    }
}

fn get(client: &InProcessClient, job_id: &JobId) -> JobSnapshot {
    match send(
        client,
        Command::GetJob {
            job_id: job_id.clone(),
        },
    ) {
        CommandResult::Job { job } => job,
        other => panic!("{other:?}"),
    }
}

fn wait_for(
    engine: &Arc<Engine>,
    client: &InProcessClient,
    job_id: &JobId,
    done: impl Fn(&JobSnapshot) -> bool,
) -> JobSnapshot {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        engine.tick();
        let job = get(client, job_id);
        if done(&job) {
            return job;
        }
        assert!(Instant::now() < deadline, "timed out at {job:?}");
        thread::sleep(Duration::from_millis(20));
    }
}

/// Serves `body`, `pause` between 16 KiB chunks, stating its length only
/// when asked to and ignoring ranges.
fn serve(body: Vec<u8>, state_length: bool, pause: Duration) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/file.bin", listener.local_addr().unwrap());
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let body = body.clone();
            thread::spawn(move || {
                let mut request = [0_u8; 4096];
                let _ = stream.read(&mut request);
                let length = if state_length {
                    format!("Content-Length: {}\r\n", body.len())
                } else {
                    String::new()
                };
                let head = format!("HTTP/1.1 200 OK\r\n{length}Connection: close\r\n\r\n");
                if stream.write_all(head.as_bytes()).is_err() {
                    return;
                }
                for chunk in body.chunks(16 * 1024) {
                    if stream.write_all(chunk).is_err() {
                        return;
                    }
                    thread::sleep(pause);
                }
            });
        }
    });
    url
}

fn open(dir: &Path) -> (Arc<Engine>, InProcessClient) {
    let session = Arc::new(Session::load_with_browser(dir.join("queue-v1.json"), 3, None).unwrap());
    let engine = Engine::new(session);
    let client = InProcessClient::manual(Arc::clone(&engine));
    (engine, client)
}

#[test]
fn a_download_waits_for_room_and_starts_by_itself_when_there_is() {
    let dir = tempfile::tempdir().unwrap();
    let (engine, client) = open(dir.path());
    let body: Vec<u8> = (0..256 * 1024).map(|i| (i % 251) as u8).collect();
    let link = serve(body.clone(), true, Duration::from_millis(1));
    reserve(&client, FULL);

    let job_id = create(&client, &link, &dir.path().join("a.bin"));
    for _ in 0..5 {
        engine.tick();
    }
    let waiting = get(&client, &job_id);
    assert_eq!(waiting.state, JobState::Queued);
    assert_eq!(waiting.waiting_reason, Some(WaitingReason::StorageReserve));
    assert_eq!(waiting.progress.bytes_received, 0);
    assert!(!dir.path().join("a.bin").exists());

    reserve(&client, 1);
    let done = wait_for(&engine, &client, &job_id, |job| {
        matches!(job.state, JobState::Completed | JobState::Failed)
    });
    assert_eq!(done.state, JobState::Completed, "{done:?}");
    assert_eq!(done.waiting_reason, None);
    assert_eq!(std::fs::read(dir.path().join("a.bin")).unwrap(), body);
}

#[test]
fn a_running_download_stops_at_the_reserve_and_finishes_once_there_is_room() {
    let dir = tempfile::tempdir().unwrap();
    let (engine, client) = open(dir.path());
    // About six seconds at full speed, of unknown size.
    let body: Vec<u8> = (0..2 * 1024 * 1024).map(|i| (i % 241) as u8).collect();
    let link = serve(body.clone(), false, Duration::from_millis(45));
    let job_id = create(&client, &link, &dir.path().join("b.bin"));
    wait_for(&engine, &client, &job_id, |job| {
        job.state == JobState::Running && job.progress.bytes_received > 0
    });

    reserve(&client, FULL);
    let stopped = wait_for(&engine, &client, &job_id, |job| {
        job.state != JobState::Running && job.state != JobState::Cancelling
    });
    assert_eq!(stopped.state, JobState::Queued, "{stopped:?}");
    assert_eq!(stopped.waiting_reason, Some(WaitingReason::StorageReserve));
    assert!(
        !dir.path().join("b.bin").exists(),
        "nothing is published partly"
    );

    reserve(&client, 1);
    let done = wait_for(&engine, &client, &job_id, |job| {
        matches!(job.state, JobState::Completed | JobState::Failed)
    });
    assert_eq!(done.state, JobState::Completed, "{done:?}");
    assert_eq!(std::fs::read(dir.path().join("b.bin")).unwrap(), body);
}
