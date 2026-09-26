//! Protocol commands on the session (FP-051), through the in-process
//! `EngineClient`: the job contract's adversarial cases for the ledger,
//! revisions and event streams.

use fetchpath_protocol::command::{
    Command, CommandEnvelope, ConflictPolicy, DestinationIntent, JobFilter, JobInput, JobRequest,
    PolicyPatch, Schedule,
};
use fetchpath_protocol::message::{CommandResult, ControlOutcome, EventPayload, StreamPosition};
use fetchpath_protocol::model::JobState;
use fetchpath_protocol::{
    ClientId, EngineClient, JobId, ProtocolError, SensitiveUrl, StreamItem, Timestamp,
};
use fetchpath_session::Session;
use fetchpath_session::engine::{Engine, InProcessClient};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

fn open(path: &Path) -> InProcessClient {
    let session = Arc::new(Session::load_with_browser(path.to_path_buf(), 3, None).unwrap());
    InProcessClient::manual(Engine::new(session))
}

fn client() -> ClientId {
    ClientId::random()
}

fn create(url: &str, destination: PathBuf) -> Command {
    Command::CreateJob {
        request: JobRequest::File {
            input: JobInput::Url {
                url: SensitiveUrl::try_from(url.to_owned()).unwrap(),
            },
            destination: DestinationIntent {
                path: destination.display().to_string(),
                conflict: ConflictPolicy::Ask,
            },
            not_before: None,
            expected_sha256: None,
        },
    }
}

/// A job that is queued but will not start for an hour, so tests can act on
/// it without a server.
fn scheduled(dir: &Path, name: &str) -> Command {
    let mut command = create("http://127.0.0.1:9/file.bin", dir.join(name));
    if let Command::CreateJob {
        request: JobRequest::File { not_before, .. },
    } = &mut command
    {
        *not_before = Some(Timestamp::from_unix_ms(
            Timestamp::now().unix_ms() + 3_600_000,
        ));
    }
    command
}

fn job_of(result: &CommandResult) -> fetchpath_protocol::JobSnapshot {
    match result {
        CommandResult::Job { job } | CommandResult::Control { job, .. } => job.clone(),
        other => panic!("not a job result: {other:?}"),
    }
}

fn jobs(client: &InProcessClient) -> Vec<fetchpath_protocol::JobSnapshot> {
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

fn code(error: &ProtocolError) -> &str {
    error.code.as_str()
}

#[test]
fn a_resent_command_returns_its_first_result_and_creates_nothing_more() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("queue-v1.json");
    let envelope = CommandEnvelope::new(client(), scheduled(dir.path(), "a.bin"));

    let engine = open(&path);
    let first = engine.execute(&envelope).unwrap();
    let again = engine.execute(&envelope).unwrap();
    assert_eq!(first, again);
    assert_eq!(jobs(&engine).len(), 1);
    drop(engine);

    // The reply was lost and the engine restarted: the ledger was committed
    // with the job, so the resend still finds it.
    let reopened = open(&path);
    assert_eq!(reopened.execute(&envelope).unwrap(), first);
    assert_eq!(jobs(&reopened).len(), 1);
}

#[test]
fn a_reused_command_id_with_a_different_request_is_refused_without_a_change() {
    let dir = tempfile::tempdir().unwrap();
    let engine = open(&dir.path().join("queue-v1.json"));
    let envelope = CommandEnvelope::new(client(), scheduled(dir.path(), "a.bin"));
    engine.execute(&envelope).unwrap();
    let mut different = envelope.clone();
    different.payload = scheduled(dir.path(), "b.bin");
    let refused = engine.execute(&different).unwrap_err();
    assert_eq!(code(&refused), "contract.idempotency_conflict");
    assert_eq!(jobs(&engine).len(), 1);
    // The same command id in another client's namespace is a new command.
    let mut elsewhere = different.clone();
    elsewhere.client_id = client();
    engine.execute(&elsewhere).unwrap();
    assert_eq!(jobs(&engine).len(), 2);
}

#[test]
fn stale_and_future_commands_are_refused_but_a_recorded_one_is_still_answered() {
    let dir = tempfile::tempdir().unwrap();
    let engine = open(&dir.path().join("queue-v1.json"));
    let now = Timestamp::now().unix_ms();

    let mut stale = CommandEnvelope::new(client(), scheduled(dir.path(), "old.bin"));
    stale.issued_at = Timestamp::from_unix_ms(now - 11 * 60 * 1_000);
    assert_eq!(
        code(&engine.execute(&stale).unwrap_err()),
        "contract.command_expired"
    );

    let mut future = CommandEnvelope::new(client(), scheduled(dir.path(), "new.bin"));
    future.issued_at = Timestamp::from_unix_ms(now + 3 * 60 * 1_000);
    assert_eq!(
        code(&engine.execute(&future).unwrap_err()),
        "contract.clock_skew"
    );
    assert!(
        jobs(&engine).is_empty(),
        "neither command may change anything"
    );

    // Lookup comes before the time check (contract §5): a command already
    // recorded is answered even when resent later than it could be sent anew.
    let mut recorded = CommandEnvelope::new(client(), scheduled(dir.path(), "kept.bin"));
    recorded.issued_at = Timestamp::from_unix_ms(now - 9 * 60 * 1_000);
    let first = engine.execute(&recorded).unwrap();
    assert_eq!(engine.execute(&recorded).unwrap(), first);
}

#[test]
fn two_clients_racing_on_one_revision_cannot_both_win() {
    let dir = tempfile::tempdir().unwrap();
    let engine = open(&dir.path().join("queue-v1.json"));
    let job = job_of(
        &engine
            .execute(&CommandEnvelope::new(
                client(),
                scheduled(dir.path(), "a.bin"),
            ))
            .unwrap(),
    );
    let seen = job.job_revision;

    // Both clients saw the same revision. The first change wins.
    let later = Timestamp::from_unix_ms(Timestamp::now().unix_ms() + 7_200_000);
    let first = CommandEnvelope::new(
        client(),
        Command::UpdatePolicy {
            job_id: job.job_id.clone(),
            patch: PolicyPatch {
                schedule: Some(Schedule::At { not_before: later }),
            },
        },
    )
    .expecting_revision(seen);
    let changed = job_of(&engine.execute(&first).unwrap());
    assert!(changed.job_revision > seen);
    assert_eq!(changed.not_before, Some(later));

    let second = CommandEnvelope::new(
        client(),
        Command::Pause {
            job_id: job.job_id.clone(),
        },
    )
    .expecting_revision(seen);
    let conflict = engine.execute(&second).unwrap_err();
    assert_eq!(code(&conflict), "contract.revision_conflict");
    assert_eq!(conflict.current_revision, Some(changed.job_revision));
    let now = jobs(&engine)
        .into_iter()
        .find(|j| j.job_id == job.job_id)
        .unwrap();
    assert_eq!(
        now.state,
        JobState::Queued,
        "the losing change was not applied"
    );

    // With the current revision it goes through.
    let retried = CommandEnvelope::new(
        client(),
        Command::Pause {
            job_id: job.job_id.clone(),
        },
    )
    .expecting_revision(changed.job_revision);
    match engine.execute(&retried).unwrap() {
        CommandResult::Control { outcome, job } => {
            assert_eq!(outcome, ControlOutcome::Accepted);
            assert_eq!(job.state, JobState::Paused);
        }
        other => panic!("{other:?}"),
    }
    // Pausing again is the matrix's no-op, not an error.
    let again = CommandEnvelope::new(client(), Command::Pause { job_id: job.job_id });
    assert!(matches!(
        engine.execute(&again).unwrap(),
        CommandResult::Control {
            outcome: ControlOutcome::NoOp,
            ..
        }
    ));
}

fn drain(
    stream: &mut Box<dyn fetchpath_protocol::EventStream>,
) -> Vec<fetchpath_protocol::JobEvent> {
    let mut events = Vec::new();
    while let Some(item) = stream.next_item(Duration::from_millis(50)).unwrap() {
        if let StreamItem::Event(event) = item {
            events.push(event);
        }
    }
    events
}

#[test]
fn a_reconnecting_subscriber_replays_exactly_what_it_missed() {
    let dir = tempfile::tempdir().unwrap();
    let engine = open(&dir.path().join("queue-v1.json"));
    let command = CommandEnvelope::new(client(), scheduled(dir.path(), "a.bin"));
    let job = job_of(&engine.execute(&command).unwrap());

    let mut first = engine
        .subscribe(&CommandEnvelope::new(
            client(),
            Command::SubscribeQueue { after_cursor: 0 },
        ))
        .unwrap();
    assert!(matches!(first.start, CommandResult::Subscribed { .. }));
    let replayed = drain(&mut first.events);
    assert_eq!(replayed.len(), 1);
    assert!(matches!(
        replayed[0].payload,
        EventPayload::JobCreated { .. }
    ));
    assert_eq!(replayed[0].seq, 1);
    assert_eq!(
        replayed[0].correlation.command_id.as_ref(),
        Some(&command.command_id),
        "the creating command is named on its event"
    );
    let seen = replayed[0].cursor;
    drop(first);

    // While disconnected the job is paused and then removed.
    engine
        .execute(&CommandEnvelope::new(
            client(),
            Command::Pause {
                job_id: job.job_id.clone(),
            },
        ))
        .unwrap();
    engine
        .execute(&CommandEnvelope::new(
            client(),
            Command::RemoveJob {
                job_id: job.job_id.clone(),
            },
        ))
        .unwrap();

    let mut second = engine
        .subscribe(&CommandEnvelope::new(
            client(),
            Command::SubscribeQueue { after_cursor: seen },
        ))
        .unwrap();
    let missed = drain(&mut second.events);
    let kinds: Vec<(u64, u64)> = missed
        .iter()
        .map(|event| (event.cursor, event.seq))
        .collect();
    assert_eq!(
        kinds,
        vec![(seen + 1, 2), (seen + 2, 3)],
        "no gap and no repeat"
    );
    assert!(matches!(
        missed[0].payload,
        EventPayload::StateChanged {
            previous: JobState::Queued,
            state: JobState::Paused,
            ..
        }
    ));
    assert!(matches!(missed[1].payload, EventPayload::JobRemoved));

    // The job's own stream agrees.
    let mut per_job = engine
        .subscribe(&CommandEnvelope::new(
            client(),
            Command::SubscribeJob {
                job_id: job.job_id.clone(),
                after_seq: 1,
            },
        ))
        .unwrap();
    let seqs: Vec<u64> = drain(&mut per_job.events)
        .iter()
        .map(|event| event.seq)
        .collect();
    assert_eq!(seqs, vec![2, 3]);
}

const COMPACTION_ROUNDS: usize = 300;

#[test]
fn a_subscriber_asking_for_compacted_events_gets_an_atomic_snapshot_boundary() {
    let dir = tempfile::tempdir().unwrap();
    let engine = open(&dir.path().join("queue-v1.json"));
    // More than the 512 retained events: create and remove jobs.
    let keep = job_of(
        &engine
            .execute(&CommandEnvelope::new(
                client(),
                scheduled(dir.path(), "keep.bin"),
            ))
            .unwrap(),
    );
    for index in 0..COMPACTION_ROUNDS {
        let job = job_of(
            &engine
                .execute(&CommandEnvelope::new(
                    client(),
                    scheduled(dir.path(), &format!("{index}.bin")),
                ))
                .unwrap(),
        );
        engine
            .execute(&CommandEnvelope::new(
                client(),
                Command::RemoveJob { job_id: job.job_id },
            ))
            .unwrap();
    }
    let subscription = engine
        .subscribe(&CommandEnvelope::new(
            client(),
            Command::SubscribeQueue { after_cursor: 0 },
        ))
        .unwrap();
    let CommandResult::SnapshotBoundary { jobs, position } = subscription.start else {
        panic!("expected a snapshot boundary, got {:?}", subscription.start);
    };
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].job_id, keep.job_id);
    let StreamPosition::Queue { after_cursor } = position else {
        panic!("{position:?}");
    };
    assert_eq!(
        after_cursor,
        1 + 2 * COMPACTION_ROUNDS as u64,
        "positioned after the last event"
    );

    // Events committed after the boundary arrive on the stream.
    let mut events = subscription.events;
    engine
        .execute(&CommandEnvelope::new(
            client(),
            Command::Pause {
                job_id: keep.job_id.clone(),
            },
        ))
        .unwrap();
    let next = drain(&mut events);
    assert_eq!(next.len(), 1);
    assert_eq!(next[0].cursor, 2 + 2 * COMPACTION_ROUNDS as u64);

    // A job asked for from before its retained events likewise.
    let per_job = engine
        .subscribe(&CommandEnvelope::new(
            client(),
            Command::SubscribeJob {
                job_id: keep.job_id.clone(),
                after_seq: 0,
            },
        ))
        .unwrap();
    assert!(matches!(
        per_job.start,
        CommandResult::SnapshotBoundary { .. }
    ));
}

/// Serves every connection the same body, slowly enough to be sampled.
fn serve(body: Vec<u8>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let body = body.clone();
            thread::spawn(move || {
                let mut request = [0_u8; 2048];
                let _ = stream.read(&mut request);
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                if stream.write_all(head.as_bytes()).is_err() {
                    return;
                }
                for chunk in body.chunks(32 * 1024) {
                    if stream.write_all(chunk).is_err() {
                        return;
                    }
                    thread::sleep(Duration::from_millis(2));
                }
            });
        }
    });
    url
}

#[test]
fn a_subscriber_that_never_reads_cannot_hold_up_the_queue() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("queue-v1.json");
    let session = Arc::new(Session::load_with_browser(path, 8, None).unwrap());
    let engine = InProcessClient::new(Engine::new(session));
    // Subscribed, then never read from again.
    let mut stalled = engine
        .subscribe(&CommandEnvelope::new(
            client(),
            Command::SubscribeQueue { after_cursor: 0 },
        ))
        .unwrap();

    let url = serve(vec![7_u8; 2 * 1024 * 1024]);
    let started = Instant::now();
    let real: Vec<JobId> = (0..4)
        .map(|index| {
            job_of(
                &engine
                    .execute(&CommandEnvelope::new(
                        client(),
                        create(
                            &format!("{url}/{index}.bin"),
                            dir.path().join(format!("real-{index}.bin")),
                        ),
                    ))
                    .unwrap(),
            )
            .job_id
        })
        .collect();
    // Enough durable events to overflow the stalled subscriber.
    for index in 0..600 {
        let job = job_of(
            &engine
                .execute(&CommandEnvelope::new(
                    client(),
                    scheduled(dir.path(), &format!("s{index}.bin")),
                ))
                .unwrap(),
        );
        engine
            .execute(&CommandEnvelope::new(
                client(),
                Command::RemoveJob { job_id: job.job_id },
            ))
            .unwrap();
    }
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let states: Vec<JobState> = jobs(&engine)
            .into_iter()
            .filter(|job| real.contains(&job.job_id))
            .map(|job| job.state)
            .collect();
        if states.iter().all(|state| *state == JobState::Completed) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "downloads did not finish: {states:?}"
        );
        thread::sleep(Duration::from_millis(100));
    }
    assert!(started.elapsed() < Duration::from_secs(60));

    // The stalled subscriber was closed rather than allowed to grow, and is
    // told to resubscribe.
    let mut outcome = None;
    for _ in 0..2_000 {
        match stalled.events.next_item(Duration::from_millis(10)) {
            Ok(Some(_)) => continue,
            Ok(None) => break,
            Err(error) => {
                outcome = Some(error);
                break;
            }
        }
    }
    let error = outcome.expect("the stalled subscriber must be closed");
    assert_eq!(code(&error), "resource.subscriber_lagging");

    // A live subscriber meanwhile receives coalesced progress, at most one
    // pending sample per job.
    let mut live = engine
        .subscribe(&CommandEnvelope::new(
            client(),
            Command::SubscribeQueue {
                after_cursor: u64::MAX,
            },
        ))
        .unwrap();
    assert!(matches!(live.start, CommandResult::SnapshotBoundary { .. }));
    let _ = live.events.next_item(Duration::from_millis(10));
}

#[test]
fn a_failed_commit_leaves_nothing_behind_and_the_resend_applies_once() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("queue-v1.json");
    let engine = open(&path);
    // The save's temporary file cannot be created while a folder sits where
    // it goes.
    let blocker = path.with_extension("json.new");
    std::fs::create_dir(&blocker).unwrap();
    let envelope = CommandEnvelope::new(client(), scheduled(dir.path(), "a.bin"));
    let failure = engine.execute(&envelope).unwrap_err();
    assert_eq!(code(&failure), "internal.persistence_failed");
    assert!(failure.retryable, "resending the same envelope is safe");
    // A restart now would find nothing: nothing reached the disk.
    assert!(!path.exists());
    // While the disk still fails, a resend is not acknowledged either: its
    // entry is in memory but not durable (contract §5).
    let still = engine.execute(&envelope).unwrap_err();
    assert_eq!(code(&still), "internal.persistence_failed");
    assert!(!path.exists());

    std::fs::remove_dir(&blocker).unwrap();
    // The change and its ledger entry were kept in memory together, so the
    // resend commits them and answers with that job, not a second one.
    let resent = engine.execute(&envelope).unwrap();
    assert!(path.exists(), "acknowledged only once on disk");
    let once_more = engine.execute(&envelope).unwrap();
    assert_eq!(job_of(&once_more).job_id, job_of(&resent).job_id);
    assert_eq!(jobs(&engine).len(), 1);
    drop(engine);
    let reopened = open(&path);
    assert_eq!(jobs(&reopened).len(), 1);
    assert_eq!(reopened.execute(&envelope).unwrap(), resent);
}

#[test]
fn a_crash_before_commit_loses_the_command_so_the_resend_creates_one_job() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("queue-v1.json");
    let envelope = CommandEnvelope::new(client(), scheduled(dir.path(), "a.bin"));
    {
        let engine = open(&path);
        let blocker = path.with_extension("json.new");
        std::fs::create_dir(&blocker).unwrap();
        assert!(engine.execute(&envelope).is_err());
        // The process dies here, before any save succeeds.
        std::mem::forget(engine);
        std::fs::remove_dir(&blocker).unwrap();
    }
    // The engine file was written with the lost command's entry and event;
    // the queue file, the commit point, was not.
    assert!(path.with_file_name("engine-v1.json").exists());
    let restarted = open(&path);
    assert!(jobs(&restarted).is_empty());
    let mut stream = restarted
        .subscribe(&CommandEnvelope::new(
            client(),
            Command::SubscribeQueue { after_cursor: 0 },
        ))
        .unwrap();
    assert!(
        drain(&mut stream.events).is_empty(),
        "an uncommitted event survived"
    );
    restarted.execute(&envelope).unwrap();
    restarted.execute(&envelope).unwrap();
    assert_eq!(jobs(&restarted).len(), 1);
    let events = drain(&mut stream.events);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].seq, 1, "a new job's first event");
    // The lost write had already used cursor 1. Nobody saw it, but that cannot
    // be known, so numbering resumes past it rather than reusing it.
    assert!(
        events[0].cursor > 1,
        "cursor {} was reused",
        events[0].cursor
    );
}

#[test]
fn a_failure_reaches_clients_as_a_code_and_action_and_unknown_ones_are_not_retryable() {
    let dir = tempfile::tempdir().unwrap();
    let engine = open(&dir.path().join("queue-v1.json"));
    // Nothing listens on port 9: a transport failure the queue may retry.
    let job = job_of(
        &engine
            .execute(&CommandEnvelope::new(
                client(),
                create("http://127.0.0.1:9/gone.bin", dir.path().join("gone.bin")),
            ))
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(30);
    let failed = loop {
        engine.engine().tick();
        let current = jobs(&engine)
            .into_iter()
            .find(|j| j.job_id == job.job_id)
            .unwrap();
        if current.state == JobState::Failed
            || current.state == JobState::Queued && current.attempt > 0
        {
            break current;
        }
        assert!(Instant::now() < deadline, "{current:?}");
        thread::sleep(Duration::from_millis(50));
    };
    if failed.state == JobState::Failed {
        let error = failed.error.expect("a failed job carries its error");
        assert_eq!(error.code.as_str(), "source.transfer_failed");
        assert!(error.retryable);
        assert_eq!(error.action, Some(fetchpath_protocol::Action::Retry));
    } else {
        // Already rescheduled by automatic retry, which only transport
        // failures get.
        assert!(failed.not_before.is_some());
    }

    // Refused state changes and unknown jobs are coded, not described.
    let unknown = CommandEnvelope::new(
        client(),
        Command::Resume {
            job_id: JobId::random(),
        },
    );
    assert_eq!(
        code(&engine.execute(&unknown).unwrap_err()),
        "contract.unknown_job"
    );
    let other = job_of(
        &engine
            .execute(&CommandEnvelope::new(
                client(),
                scheduled(dir.path(), "b.bin"),
            ))
            .unwrap(),
    );
    let refused = engine
        .execute(&CommandEnvelope::new(
            client(),
            Command::Resume {
                job_id: other.job_id,
            },
        ))
        .unwrap_err();
    assert_eq!(code(&refused), "contract.invalid_transition");
}

#[test]
fn a_queue_restored_from_its_backup_never_reuses_numbers_a_subscriber_saw() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("queue-v1.json");
    let seen = {
        let engine = open(&path);
        let mut stream = engine
            .subscribe(&CommandEnvelope::new(
                client(),
                Command::SubscribeQueue { after_cursor: 0 },
            ))
            .unwrap()
            .events;
        engine
            .execute(&CommandEnvelope::new(
                client(),
                scheduled(dir.path(), "a.bin"),
            ))
            .unwrap();
        engine
            .execute(&CommandEnvelope::new(
                client(),
                scheduled(dir.path(), "b.bin"),
            ))
            .unwrap();
        drain(&mut stream)
            .iter()
            .map(|event| event.cursor)
            .max()
            .unwrap()
    };
    // The main queue file is damaged; loading falls back to the backup,
    // which predates job b.
    std::fs::write(&path, "{ torn").unwrap();
    let engine = open(&path);
    assert_eq!(jobs(&engine).len(), 1, "the backup holds job a only");
    let created = engine
        .execute(&CommandEnvelope::new(
            client(),
            scheduled(dir.path(), "c.bin"),
        ))
        .unwrap();
    let c = job_of(&created);

    // Resubscribing from what it saw, the client must not be told it is up
    // to date: job b is gone and c is new.
    let resumed = engine
        .subscribe(&CommandEnvelope::new(
            client(),
            Command::SubscribeQueue { after_cursor: seen },
        ))
        .unwrap();
    match resumed.start {
        CommandResult::SnapshotBoundary { jobs, position } => {
            assert!(jobs.iter().any(|job| job.job_id == c.job_id));
            let StreamPosition::Queue { after_cursor } = position else {
                panic!("{position:?}");
            };
            assert!(after_cursor > seen, "a cursor was reused");
        }
        other => panic!("expected a snapshot boundary, got {other:?}"),
    }
}

fn file_request(url: &str, destination: String, checksum: Option<String>) -> JobRequest {
    JobRequest::File {
        input: JobInput::Url {
            url: SensitiveUrl::try_from(url.to_owned()).unwrap(),
        },
        destination: DestinationIntent {
            path: destination,
            conflict: ConflictPolicy::Ask,
        },
        not_before: Some(Timestamp::from_unix_ms(
            Timestamp::now().unix_ms() + 3_600_000,
        )),
        expected_sha256: checksum,
    }
}

#[test]
fn a_batch_creates_every_job_or_none_and_a_resend_creates_nothing_more() {
    let dir = tempfile::tempdir().unwrap();
    let engine = open(&dir.path().join("queue-v1.json"));
    let link = "http://127.0.0.1:9/file.bin";
    let good = |name: &str| file_request(link, dir.path().join(name).display().to_string(), None);

    // One request the queue refuses (a relative path) and none is created.
    let refused = engine
        .send(
            &client(),
            Command::CreateJobs {
                requests: vec![good("a.bin"), file_request(link, "b.bin".into(), None)],
            },
        )
        .unwrap_err();
    assert_eq!(code(&refused), "input.invalid_request");
    assert!(jobs(&engine).is_empty());

    let envelope = CommandEnvelope::new(
        client(),
        Command::CreateJobs {
            requests: vec![good("a.bin"), good("b.bin")],
        },
    );
    let first = engine.execute(&envelope).unwrap();
    let CommandResult::Jobs { jobs: created } = &first else {
        panic!("{first:?}");
    };
    let names: Vec<_> = created
        .iter()
        .map(|job| job.destination.clone().unwrap())
        .collect();
    assert_eq!(
        names,
        [
            dir.path().join("a.bin").display().to_string(),
            dir.path().join("b.bin").display().to_string()
        ]
    );
    assert_eq!(engine.execute(&envelope).unwrap(), first);
    assert_eq!(jobs(&engine).len(), 2);
}

#[test]
fn the_person_can_correct_the_expected_checksum_before_a_retry() {
    let dir = tempfile::tempdir().unwrap();
    let engine = open(&dir.path().join("queue-v1.json"));
    let body = b"checked bytes".repeat(1000);
    let base = serve(body.clone());
    let destination = dir.path().join("checked.bin");
    let mut request = file_request(
        &format!("{base}/checked.bin"),
        destination.display().to_string(),
        Some("0".repeat(64)),
    );
    if let JobRequest::File { not_before, .. } = &mut request {
        *not_before = None;
    }
    let job = job_of(
        &engine
            .send(&client(), Command::CreateJob { request })
            .unwrap(),
    );
    let settled = |engine: &InProcessClient, state: JobState| {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            engine.engine().tick();
            let now = jobs(engine)
                .into_iter()
                .find(|j| j.job_id == job.job_id)
                .unwrap();
            if now.state == state {
                return now;
            }
            assert!(Instant::now() < deadline, "{now:?}");
            thread::sleep(Duration::from_millis(20));
        }
    };
    let failed = settled(&engine, JobState::Failed);
    assert_eq!(
        failed.error.unwrap().code.as_str(),
        "integrity.checksum_mismatch"
    );
    assert!(!destination.exists());

    // sha256 of the body, computed independently of the engine.
    let expected = {
        use sha2::{Digest, Sha256};
        format!("{:x}", Sha256::digest(&body))
    };
    let retried = job_of(
        &engine
            .send(
                &client(),
                Command::Retry {
                    job_id: job.job_id.clone(),
                    expected_sha256: Some(expected.clone()),
                },
            )
            .unwrap(),
    );
    assert_eq!(retried.expected_sha256.as_deref(), Some(expected.as_str()));
    let done = settled(&engine, JobState::Completed);
    assert_eq!(done.observed_sha256.as_deref(), Some(expected.as_str()));
    assert_eq!(std::fs::read(&destination).unwrap(), body);
}

#[test]
fn a_media_page_from_the_browser_is_offered_for_review_once() {
    use fetchpath_session::browser_inbox::{BridgeStore, CaptureRequest, SCHEMA_VERSION};
    let dir = tempfile::tempdir().unwrap();
    let queue = dir.path().join("queue-v1.json");
    let downloads = dir.path().join("Downloads");
    std::fs::create_dir_all(&downloads).unwrap();
    let session =
        Arc::new(Session::load_with_browser(queue.clone(), 3, Some(downloads.clone())).unwrap());
    let engine = InProcessClient::manual(Engine::new(session));
    let page = "https://www.youtube.com/watch?v=abc123";
    BridgeStore::new(dir.path().to_path_buf())
        .accept(&CaptureRequest {
            schema_version: SCHEMA_VERSION,
            capture_id: "5f0c3f7e-8b2a-4d6e-9c1f-2a3b4c5d6e7f".into(),
            method: "GET".into(),
            url: page.into(),
            suggested_filename: "watch.html".into(),
            referrer: None,
            cookies: Vec::new(),
            user_initiated: true,
        })
        .unwrap();

    // Listing takes in what the browser sent: a page is not a download.
    assert!(jobs(&engine).is_empty());
    let take = || match engine.send(&client(), Command::TakeLinkReviews).unwrap() {
        CommandResult::LinkReviews { urls } => urls,
        other => panic!("{other:?}"),
    };
    let first = take();
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].expose(), page);
    assert!(take().is_empty());
}

/// A queue written by a newer Fetchpath (FP-070): clients see its jobs and a
/// stable reason, every change is refused with it, and the engine can still
/// be stopped, all without the file changing.
#[test]
fn a_queue_from_a_newer_build_is_served_read_only_and_never_written() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("queue-v1.json");
    let saved = format!(
        concat!(
            "{{\"schemaVersion\": 2, \"records\": [{{\"id\": \"00000000-0000-4000-8000-0000000000aa\", ",
            "\"restartUrl\": \"https://example.test/a.bin\", \"displayUrl\": \"https://example.test/a.bin\", ",
            "\"destination\": {dest}, \"createdAtMs\": 1790000000000, \"futureField\": [1, 2], ",
            "\"view\": {{\"jobId\": \"00000000-0000-4000-8000-0000000000aa\", \"source\": \"https://example.test/a.bin\", ",
            "\"state\": \"queued\", \"bytesReceived\": 5, \"totalBytes\": 10, \"attempt\": 0, ",
            "\"destination\": {dest}, \"cleanupPending\": false, \"retryable\": false, \"createdAtMs\": 1790000000000}}}}]}}\n"
        ),
        dest = serde_json::to_string(&dir.path().join("a.bin").display().to_string()).unwrap()
    );
    std::fs::write(&path, &saved).unwrap();
    let engine = open(&path);

    let listed = jobs(&engine);
    assert_eq!(listed.len(), 1, "{listed:?}");
    assert_eq!(listed[0].progress.bytes_received, 5);

    let refused = engine
        .execute(&CommandEnvelope::new(
            client(),
            create("https://example.test/b.bin", dir.path().join("b.bin")),
        ))
        .unwrap_err();
    assert_eq!(refused.code.as_str(), "storage.queue_from_newer_version");
    assert_eq!(
        refused.action,
        Some(fetchpath_protocol::error::Action::UpdateSoftware)
    );
    assert!(refused.message.contains("newer version"));
    let job_id = listed[0].job_id.clone();
    for command in [
        Command::Resume {
            job_id: job_id.clone(),
        },
        Command::RemoveJob { job_id },
    ] {
        let refused = engine
            .execute(&CommandEnvelope::new(client(), command))
            .unwrap_err();
        assert_eq!(refused.code.as_str(), "storage.queue_from_newer_version");
    }

    let status = match engine
        .execute(&CommandEnvelope::new(client(), Command::EngineStatus))
        .unwrap()
    {
        CommandResult::EngineStatus { status } => status,
        other => panic!("{other:?}"),
    };
    assert_eq!(
        status
            .queue_read_only
            .map(|reason| reason.code.as_str().to_owned()),
        Some("storage.queue_from_newer_version".to_owned())
    );
    assert!(matches!(
        engine
            .execute(&CommandEnvelope::new(client(), Command::EngineShutdown))
            .unwrap(),
        CommandResult::ShuttingDown
    ));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), saved);
    assert!(!dir.path().join("engine-v1.json").exists());
    // Nothing was prepared for the saved job: no destination or partial file.
    let files: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(files, ["queue-v1.json"], "{files:?}");
}
