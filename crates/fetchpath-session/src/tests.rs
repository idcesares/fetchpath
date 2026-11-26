use super::*;

#[test]
fn the_opening_probe_byte_is_never_reported_as_the_rate() {
    const MIB: u64 = 1024 * 1024;
    let mut rate = RateEstimate::default();
    rate.observe(0, 0);
    rate.observe(1, 1_000); // the one-byte range probe
    assert_eq!(rate.bytes_per_second(), None);
    assert_eq!(rate.eta_seconds(1, Some(24 * MIB)), None);
    rate.observe(4 * MIB, 1_500); // the ranges arrive
    assert!(rate.bytes_per_second().unwrap() > MIB);

    // A link that really is slow still gets a rate, averaged from its start.
    let mut slow = RateEstimate::default();
    slow.observe(0, 0);
    slow.observe(5_000, 5_000);
    assert_eq!(slow.bytes_per_second(), Some(1_000));
}

/// The blank line that ends an HTTP request head.
const HEAD_TERMINATOR: &[u8] = b"\r\n\r\n";
use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread;
use std::time::Duration;

/// Settings promises that only connection problems are retried
/// automatically. A link the server refuses is not one.
#[test]
fn a_link_the_server_refuses_waits_for_a_person_instead_of_retrying() {
    for refused in [400, 401, 403, 404, 410, 451] {
        let error = format!("source.transfer_failed: HTTP status {refused}");
        assert_eq!(action_for_error(&error), "edit_link", "HTTP {refused}");
    }
    for transient in [408, 429, 500, 502, 503] {
        let error = format!("source.transfer_failed: HTTP status {transient}");
        assert_eq!(action_for_error(&error), "retry", "HTTP {transient}");
    }
    assert_eq!(
        action_for_error("source.transfer_failed: [7] Couldn't connect to server"),
        "retry"
    );
}

fn fixture(body: Vec<u8>, slow: bool) -> (String, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0_u8; 1024];
        let _ = stream.read(&mut request);
        stream
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .as_bytes(),
            )
            .unwrap();
        for chunk in body.chunks(16 * 1024) {
            if stream.write_all(chunk).is_err() {
                break;
            }
            if slow {
                thread::sleep(Duration::from_millis(5));
            }
        }
    });
    (url, handle)
}

/// A fixture that honours `Range` and `If-Range`, so a resumed transfer can
/// be observed continuing from an offset rather than starting over.
///
/// It serves connections in a loop and counts the requests that carried an
/// `If-Range` header. That header is sent only by the resume path, so the
/// count is direct evidence that a resume used the checkpoint instead of
/// quietly re-downloading the whole file.
fn resumable_fixture(
    body: Vec<u8>,
    chunk_delay: Duration,
) -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let if_range_requests = Arc::new(AtomicUsize::new(0));
    let counter = if_range_requests.clone();

    thread::spawn(move || {
        let total = body.len();
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let mut head = Vec::new();
            let mut byte = [0_u8; 1];
            while head.len() < 8192 {
                match stream.read(&mut byte) {
                    Ok(1) => head.push(byte[0]),
                    _ => break,
                }
                if head.ends_with(HEAD_TERMINATOR) {
                    break;
                }
            }
            let request = String::from_utf8_lossy(&head).to_string();
            if request.to_ascii_lowercase().contains("if-range:") {
                counter.fetch_add(1, Ordering::SeqCst);
            }
            let start = request
                .lines()
                .find_map(|line| {
                    let rest = line.strip_prefix("Range: bytes=")?;
                    rest.split('-').next()?.parse::<usize>().ok()
                })
                .unwrap_or(0)
                .min(total);

            let header = if start > 0 {
                format!(
                    concat!(
                        "HTTP/1.1 206 Partial Content\r\n",
                        "Content-Length: {}\r\n",
                        "Content-Range: bytes {}-{}/{}\r\n",
                        "ETag: \"fixture-v1\"\r\n",
                        "Accept-Ranges: bytes\r\n",
                        "Connection: close\r\n\r\n",
                    ),
                    total - start,
                    start,
                    total - 1,
                    total
                )
            } else {
                format!(
                    concat!(
                        "HTTP/1.1 200 OK\r\n",
                        "Content-Length: {}\r\n",
                        "ETag: \"fixture-v1\"\r\n",
                        "Accept-Ranges: bytes\r\n",
                        "Connection: close\r\n\r\n",
                    ),
                    total
                )
            };
            if stream.write_all(header.as_bytes()).is_err() {
                continue;
            }
            for chunk in body[start..].chunks(32 * 1024) {
                if stream.write_all(chunk).is_err() {
                    break;
                }
                if !chunk_delay.is_zero() {
                    thread::sleep(chunk_delay);
                }
            }
        }
    });
    (url, if_range_requests)
}

/// Waits for a record to reach one of `states`, returning the snapshot.
fn wait_for_state(jobs: &Session, job_id: &str, states: &[&str]) -> JobSnapshot {
    for _ in 0..600 {
        let snapshot = jobs.snapshot(job_id).unwrap();
        if states.contains(&snapshot.state.as_str()) {
            return snapshot;
        }
        thread::sleep(Duration::from_millis(5));
    }
    panic!(
        "desktop job never reached {states:?}; last state was {:?}",
        jobs.snapshot(job_id).unwrap().state
    );
}

fn wait_for_terminal(jobs: &Session, job_id: &str) -> JobSnapshot {
    for _ in 0..400 {
        let snapshot = jobs.snapshot(job_id).unwrap();
        if is_terminal(&snapshot.state) {
            return snapshot;
        }
        thread::sleep(Duration::from_millis(5));
    }
    panic!("desktop job did not finish");
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(bytes))
}

fn checked_draft(url: String, destination: &Path, checksum: &str) -> JobDraft {
    JobDraft {
        checksum: Some(checksum.into()),
        url,
        destination: destination.display().to_string(),
        not_before_ms: None,
    }
}

#[test]
fn a_download_matching_its_pasted_checksum_completes() {
    let dir = tempfile::tempdir().unwrap();
    let body = vec![0x41; 64 * 1024];
    let digest = sha256_hex(&body);
    let (url, server) = fixture(body.clone(), false);
    let jobs = Session::in_memory(3);
    let destination = dir.path().join("checked.bin");
    // Pasted the way checksum files often print it.
    let pasted = format!("  SHA256:{}  ", digest.to_uppercase());
    let started = jobs
        .enqueue(vec![checked_draft(url, &destination, &pasted)])
        .unwrap()
        .remove(0);
    assert_eq!(started.expected_sha256.as_deref(), Some(digest.as_str()));

    let completed = wait_for_terminal(&jobs, &started.job_id);
    server.join().unwrap();
    assert_eq!(completed.state, "completed");
    assert_eq!(completed.observed_sha256.as_deref(), Some(digest.as_str()));
    assert_eq!(completed.expected_sha256.as_deref(), Some(digest.as_str()));
    assert_eq!(fs::read(&destination).unwrap(), body);
}

#[test]
fn a_checksum_mismatch_saves_nothing_and_is_never_retried_automatically() {
    let dir = tempfile::tempdir().unwrap();
    let body = vec![0x42; 64 * 1024];
    let (url, server) = fixture(body, false);
    let jobs = Session::in_memory(3);
    let destination = dir.path().join("mismatch.bin");
    let wrong = sha256_hex(b"a different file");
    let started = jobs
        .enqueue(vec![checked_draft(url, &destination, &wrong)])
        .unwrap()
        .remove(0);

    let failed = wait_for_terminal(&jobs, &started.job_id);
    server.join().unwrap();
    assert_eq!(failed.state, "failed");
    assert_eq!(failed.action.as_deref(), Some("check_checksum"));
    assert!(failed.retryable, "a person can still retry by hand");
    let error = failed.error.unwrap();
    assert!(error.contains("checksum_mismatch"), "{error}");
    assert!(
        error.contains(&wrong),
        "the expected value is shown: {error}"
    );
    assert!(!destination.exists(), "nothing was saved");

    // Automatic retry is on by default and only repeats plain transport
    // failures. A checksum mismatch needs a person, so it is left alone.
    let mut state = jobs.inner.lock().unwrap();
    jobs.schedule_automatic_retries(&mut state, &jobs.settings());
    let record = find_record(&state, &started.job_id).unwrap();
    assert_eq!(record.retry_at_ms, None);
    assert_eq!(record.view.state, "failed");
}

#[test]
fn a_malformed_checksum_is_refused_with_a_plain_explanation() {
    let dir = tempfile::tempdir().unwrap();
    let jobs = Session::in_memory(3);
    let error = jobs
        .enqueue(vec![checked_draft(
            "http://127.0.0.1:9/file.bin".into(),
            &dir.path().join("x.bin"),
            "d41d8cd98f00b204e9800998ecf8427e",
        )])
        .unwrap_err();
    assert!(error.contains("isn't a SHA-256"), "{error}");
    assert!(jobs.list().unwrap().is_empty(), "nothing was queued");
}

#[test]
fn a_blank_checksum_means_none() {
    let dir = tempfile::tempdir().unwrap();
    let jobs = Session::in_memory(0);
    let queued = jobs
        .enqueue(vec![checked_draft(
            "http://127.0.0.1:9/file.bin".into(),
            &dir.path().join("x.bin"),
            "   ",
        )])
        .unwrap()
        .remove(0);
    assert_eq!(queued.expected_sha256, None);
}

#[test]
fn a_checksum_is_refused_on_a_batch_because_it_describes_one_file() {
    let dir = tempfile::tempdir().unwrap();
    let jobs = Session::in_memory(3);
    let error = jobs
        .enqueue(vec![
            checked_draft(
                "http://127.0.0.1:9/a.bin".into(),
                &dir.path().join("a.bin"),
                &"ab".repeat(32),
            ),
            draft("http://127.0.0.1:9/b.bin", &dir.path().join("b.bin")),
        ])
        .unwrap_err();
    assert!(error.contains("one file"), "{error}");
}

#[test]
fn retrying_with_a_corrected_checksum_completes() {
    let dir = tempfile::tempdir().unwrap();
    let body = vec![0x43; 32 * 1024];
    let digest = sha256_hex(&body);
    let (url, server) = fixture(body.clone(), false);
    let jobs = Session::in_memory(3);
    let destination = dir.path().join("corrected.bin");
    let started = jobs
        .enqueue(vec![checked_draft(url, &destination, &"00".repeat(32))])
        .unwrap()
        .remove(0);
    assert_eq!(wait_for_terminal(&jobs, &started.job_id).state, "failed");
    server.join().unwrap();

    let (url, server) = fixture(body.clone(), false);
    jobs.retry(&started.job_id, Some(url), None, Some(digest.clone()))
        .unwrap();
    let completed = wait_for_terminal(&jobs, &started.job_id);
    server.join().unwrap();
    assert_eq!(completed.state, "completed");
    assert_eq!(completed.expected_sha256.as_deref(), Some(digest.as_str()));
    assert_eq!(fs::read(&destination).unwrap(), body);
}

#[test]
fn an_unreadable_saved_checksum_fails_closed_after_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let record = QueueRecord::new_checked(
        "http://127.0.0.1:9/file.bin".into(),
        dir.path().join("file.bin"),
        None,
        Some("ab".repeat(32)),
    );
    let mut view = record.view.clone();
    view.state = "queued".into();
    view.expected_sha256 = Some("not a checksum".into());
    let saved = PersistedRecord {
        id: record.id.clone(),
        restart_url: record.restart_url.clone(),
        credential_ref: None,
        display_url: record.display_url.clone(),
        destination: record.destination.display().to_string(),
        not_before_ms: None,
        created_at_ms: record.created_at_ms,
        finished_at_ms: None,
        media_variant_id: None,
        media_quality: None,
        torrent_policy: None,
        torrent_metadata_sha256: None,
        torrent_auto: false,
        durable: RecordDurable::default(),
        principal: Principal::User,
        approval: None,
        size_approved: false,
        folder_approved: false,
        public_origin: false,
        provenance_marked: false,
        view,
    };
    let restored = QueueRecord::restore(saved, now_ms(), None, None, Path::new("queue-v1.json"));
    assert!(
        restored.job.is_none(),
        "never a job that downloads unchecked"
    );
    assert_eq!(restored.view.state, "failed");
    assert_eq!(restored.view.action.as_deref(), Some("check_checksum"));
}

#[test]
fn local_torrent_snapshot_survives_original_deletion_and_rejects_tampering() {
    let dir = tempfile::tempdir().unwrap();
    let queue = dir.path().join("queue-v1.json");
    let original = dir.path().join("debian.torrent");
    fs::write(&original, b"d4:infod4:name6:debian6:lengthi0eee").unwrap();
    let destination = dir.path().join("debian");
    let policy = TorrentPolicy {
        discover_peers: true,
        upload: false,
    };
    let id = {
        let session = Session::load(queue.clone(), 1).unwrap();
        session
            .enqueue_torrent_file_for(
                &original,
                &destination.display().to_string(),
                false,
                Some(now_ms() + 3_600_000),
                policy,
                &Origin::default(),
            )
            .unwrap()
            .job_id
    };
    fs::remove_file(&original).unwrap();
    let restored = Session::load(queue.clone(), 1).unwrap();
    assert_eq!(restored.snapshot(&id).unwrap().state, "scheduled");
    assert_eq!(restored.snapshot(&id).unwrap().source, "debian.torrent");
    drop(restored);
    fs::write(
        dir.path()
            .join("torrent-metadata")
            .join(format!("{id}.torrent")),
        b"tampered",
    )
    .unwrap();
    let tampered = Session::load(queue, 1).unwrap();
    assert_eq!(tampered.snapshot(&id).unwrap().state, "failed");
}

#[test]
fn a_default_and_an_approval_hold_survive_a_restart_through_the_engine() {
    use crate::engine::{Engine, InProcessClient};
    use fetchpath_protocol::command::{
        Command, ConflictPolicy, DestinationIntent, JobInput, JobRequest,
    };
    use fetchpath_protocol::principal::{AgentName, AgentPolicy};
    use fetchpath_protocol::{ClientId, EngineClient, Timestamp};
    let dir = tempfile::tempdir().unwrap();
    let queue = dir.path().join("queue-v1.json");
    let granted = dir.path().join("granted");
    fs::create_dir_all(&granted).unwrap();
    let original = dir.path().join("debian.torrent");
    fs::write(&original, b"d4:infod4:name6:debian6:lengthi0eee").unwrap();
    let torrent = |name: &str, discover_peers, input| Command::CreateJob {
        request: JobRequest::Torrent {
            input,
            destination: DestinationIntent {
                path: granted.join(name).display().to_string(),
                conflict: ConflictPolicy::Ask,
            },
            not_before: Some(Timestamp::from_unix_ms(now_ms() as i64 + 3_600_000)),
            discover_peers,
            upload: false,
        },
    };
    let name = AgentName::try_from("helper").unwrap();
    let (person, agent) = {
        let engine = Engine::new(Arc::new(Session::load(queue.clone(), 1).unwrap()));
        let user = InProcessClient::manual(Arc::clone(&engine));
        let helper = InProcessClient::manual(Arc::clone(&engine))
            .with_principal(Principal::Agent(name.clone()));
        user.send(
            &ClientId::random(),
            Command::SetAgentPolicy {
                agent: name,
                policy: Some(AgentPolicy {
                    folders: vec![granted.display().to_string()],
                    ..AgentPolicy::default()
                }),
            },
        )
        .unwrap();
        let id = |client: &InProcessClient, command| match client
            .send(&ClientId::random(), command)
            .unwrap()
        {
            fetchpath_protocol::message::CommandResult::Job { job } => job.job_id.to_string(),
            other => panic!("{other:?}"),
        };
        (
            id(
                &user,
                torrent(
                    "person",
                    None,
                    JobInput::TorrentFile {
                        path: original.display().to_string(),
                    },
                ),
            ),
            id(
                &helper,
                torrent(
                    "agent",
                    Some(true),
                    JobInput::Url {
                        url: fetchpath_protocol::SensitiveUrl::try_from(
                            "magnet:?xt=urn:btih:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                                .to_owned(),
                        )
                        .unwrap(),
                    },
                ),
            ),
        )
    };
    let restored = Session::load(queue, 1).unwrap();
    let state = restored.inner.lock().unwrap();
    let find = |id: &str| state.records.iter().find(|record| record.id == id).unwrap();
    assert!(find(&person).torrent_policy.unwrap().discover_peers);
    let held = find(&agent);
    assert!(held.torrent_policy.unwrap().discover_peers);
    assert!(
        held.approval
            .as_ref()
            .unwrap()
            .reasons
            .contains(&ApprovalReason::PeerDiscovery)
    );
}

#[test]
fn refreshing_a_local_torrent_retires_its_metadata_before_using_the_new_link() {
    let dir = tempfile::tempdir().unwrap();
    let queue = dir.path().join("queue-v1.json");
    let original = dir.path().join("first.torrent");
    fs::write(&original, b"d4:infod4:name5:first6:lengthi0eee").unwrap();
    let destination = dir.path().join("output");
    let session = Session::load(queue.clone(), 1).unwrap();
    let id = session
        .enqueue_torrent_file_for(
            &original,
            &destination.display().to_string(),
            false,
            Some(now_ms() + 3_600_000),
            TorrentPolicy {
                discover_peers: true,
                upload: false,
            },
            &Origin::default(),
        )
        .unwrap()
        .job_id;
    let snapshot = torrent_metadata_path(&queue, &id).unwrap();
    assert!(snapshot.exists());
    session.cancel(&id).unwrap();
    fs::create_dir(&destination).unwrap();
    session.durable.lock().unwrap().defer = true;
    session
        .retry(
            &id,
            Some("https://example.test/second.torrent".into()),
            None,
            None,
        )
        .unwrap();
    assert!(
        snapshot.exists(),
        "deferred save must keep metadata referenced by the old queue"
    );
    let persisted = load_persisted(&queue).unwrap().current().unwrap();
    assert!(persisted.records[0].torrent_metadata_sha256.is_some());
    let state = session.inner.lock().unwrap();
    let record = find_record(&state, &id).unwrap();
    assert_eq!(
        record.live_url.as_deref(),
        Some("https://example.test/second.torrent")
    );
    assert!(record.torrent_metadata_sha256.is_none());
    assert!(record.torrent_metadata_path.is_none());
    let mut durable = session.durable.lock().unwrap();
    durable.defer = false;
    session.write_locked(&state, &mut durable).unwrap();
    assert!(
        snapshot.exists(),
        "the backup queue still names the old metadata"
    );
    let backup: PersistedQueue =
        serde_json::from_slice(&fs::read(queue.with_extension("json.bak")).unwrap()).unwrap();
    assert!(backup.records[0].torrent_metadata_sha256.is_some());
    session.write_locked(&state, &mut durable).unwrap();
    assert!(!snapshot.exists());
}

#[test]
fn an_automatic_torrent_keeps_its_root_across_restart_and_follows_the_published_folder() {
    let dir = tempfile::tempdir().unwrap();
    let queue = dir.path().join("queue-v1.json");
    let root = dir.path().join("downloads");
    fs::create_dir(&root).unwrap();
    let policy = TorrentPolicy {
        discover_peers: true,
        upload: false,
    };
    let id = {
        let session = Session::load(queue.clone(), 1).unwrap();
        session
            .enqueue_torrent_for(
                TorrentDraft {
                    source: "https://example.test/a.torrent".into(),
                    destination: root.display().to_string(),
                    not_before_ms: Some(now_ms() + 3_600_000),
                    policy,
                    auto: true,
                },
                &Origin::default(),
            )
            .unwrap()
            .job_id
    };
    let persisted = load_persisted(&queue).unwrap().current().unwrap();
    assert!(
        persisted.records[0].torrent_auto,
        "{:?}",
        persisted.records[0].view
    );
    assert_eq!(persisted.records[0].destination, root.display().to_string());
    let restored = Session::load(queue, 1).unwrap();
    let state = restored.inner.lock().unwrap();
    let record = find_record(&state, &id).unwrap();
    assert!(record.torrent_auto);
    assert_eq!(
        record.view.state, "scheduled",
        "an existing root is no conflict"
    );
    let Some(JobHandle::Torrent(job)) = record.job.as_ref() else {
        panic!("the restored torrent has a job");
    };
    assert!(job.request().auto_name);
}

#[test]
fn a_published_folder_must_be_one_name_directly_inside_the_root() {
    let root = Path::new(r"C:\downloads");
    assert!(published_inside(root, Path::new(r"C:\downloads\Album")));
    assert!(!published_inside(root, Path::new(r"C:\downloads")));
    assert!(!published_inside(root, Path::new(r"C:\downloads\a\b")));
    assert!(!published_inside(root, Path::new(r"C:\elsewhere\Album")));
    assert!(!published_inside(root, Path::new(r"C:\downloads\..")));
}

#[test]
fn an_agent_refreshing_a_torrent_waits_for_peer_approval_again() {
    let output = tempfile::tempdir().unwrap();
    let agent = AgentName::try_from("helper").unwrap();
    let principal = Principal::Agent(agent);
    let jobs = Session::in_memory(1);
    let mut record = QueueRecord::new_torrent(
        "magnet:?xt=urn:btih:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
        output.path().join("torrent"),
        false,
        None,
        TorrentPolicy {
            discover_peers: true,
            upload: true,
        },
    );
    record.principal = principal.clone();
    record.view.state = "failed".into();
    let job_id = record.id.clone();
    jobs.inner
        .lock()
        .expect("desktop jobs poisoned")
        .records
        .push(record);

    let refreshed = jobs
        .retry_as(
            &job_id,
            Some("magnet:?xt=urn:btih:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into()),
            None,
            None,
            &principal,
            Vec::new(),
        )
        .unwrap();
    assert_eq!(refreshed.state, "awaiting_approval");
    let state = jobs.inner.lock().expect("desktop jobs poisoned");
    let reasons = &find_record(&state, &job_id)
        .unwrap()
        .approval
        .as_ref()
        .unwrap()
        .reasons;
    assert!(reasons.contains(&ApprovalReason::PeerDiscovery));
    assert!(reasons.contains(&ApprovalReason::PeerUpload));
}

#[test]
fn an_automatic_agent_refreshing_a_torrent_still_asks_only_to_upload() {
    let output = tempfile::tempdir().unwrap();
    let agent = AgentName::try_from("helper").unwrap();
    let principal = Principal::Agent(agent.clone());
    let jobs = Session::in_memory(1);
    jobs.agents.lock().expect("agents poisoned").insert(
        agent,
        AgentPolicy {
            automatic: true,
            ..AgentPolicy::default()
        },
    );
    let mut record = QueueRecord::new_torrent(
        "magnet:?xt=urn:btih:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
        output.path().join("torrent"),
        false,
        None,
        TorrentPolicy {
            discover_peers: true,
            upload: true,
        },
    );
    record.principal = principal.clone();
    record.view.state = "failed".into();
    let job_id = record.id.clone();
    jobs.inner
        .lock()
        .expect("desktop jobs poisoned")
        .records
        .push(record);

    jobs.retry_as(
        &job_id,
        Some("magnet:?xt=urn:btih:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into()),
        None,
        None,
        &principal,
        Vec::new(),
    )
    .unwrap();
    let state = jobs.inner.lock().expect("desktop jobs poisoned");
    let reasons = &find_record(&state, &job_id)
        .unwrap()
        .approval
        .as_ref()
        .unwrap()
        .reasons;
    assert!(!reasons.contains(&ApprovalReason::PeerDiscovery));
    assert!(reasons.contains(&ApprovalReason::PeerUpload));
}

#[test]
fn a_stated_length_reaches_the_queue_view_as_a_total() {
    let dir = tempfile::tempdir().unwrap();
    let body = vec![0x31; 256 * 1024];
    let expected = body.len() as u64;
    let (url, server) = fixture(body, false);
    let jobs = Session::in_memory(3);
    let started = jobs
        .enqueue(vec![JobDraft {
            checksum: None,
            url,
            destination: dir.path().join("sized.bin").display().to_string(),
            not_before_ms: None,
        }])
        .unwrap()
        .remove(0);
    // A queued download has not spoken to the source yet, so it has no total.
    assert_eq!(started.total_bytes, None);

    let completed = wait_for_terminal(&jobs, &started.job_id);
    server.join().unwrap();
    assert_eq!(completed.state, "completed");
    assert_eq!(completed.total_bytes, Some(expected));
    // Nothing is running, so no rate or remaining time may be reported.
    assert_eq!(completed.bytes_per_second, None);
    assert_eq!(completed.eta_seconds, None);
}

#[test]
fn pausing_a_running_download_resumes_from_its_checkpoint() {
    use std::sync::atomic::Ordering;

    let dir = tempfile::tempdir().unwrap();
    let destination = dir.path().join("resumable.bin");
    let body: Vec<u8> = (0..768 * 1024).map(|index| (index % 251) as u8).collect();
    // Slow enough that the test can pause partway through rather than
    // racing a transfer that has already finished.
    let (url, if_range_requests) = resumable_fixture(body.clone(), Duration::from_millis(12));

    let jobs = Session::in_memory(3);
    let started = jobs
        .enqueue(vec![JobDraft {
            checksum: None,
            url,
            destination: destination.display().to_string(),
            not_before_ms: None,
        }])
        .unwrap()
        .remove(0);

    // Wait for real bytes before pausing; pausing at zero would prove nothing.
    let mut running = wait_for_state(&jobs, &started.job_id, &["running", "completed"]);
    for _ in 0..600 {
        if running.state != "running" || running.bytes_received > 64 * 1024 {
            break;
        }
        thread::sleep(Duration::from_millis(5));
        running = jobs.snapshot(&started.job_id).unwrap();
    }
    assert_eq!(
        running.state, "running",
        "fixture finished before the pause"
    );
    let paused = jobs.pause(&started.job_id).unwrap();
    assert_eq!(paused.state, "paused");
    assert!(
        paused.bytes_received > 0,
        "a pause has to keep the bytes already verified"
    );
    assert_eq!(
        paused.bytes_per_second, None,
        "a paused row reports no speed"
    );
    assert!(!destination.exists(), "nothing is published while paused");
    let paused_at = paused.bytes_received;

    // Pausing is durable: another poll must not restart it.
    thread::sleep(Duration::from_millis(60));
    assert_eq!(jobs.snapshot(&started.job_id).unwrap().state, "paused");

    let resumed = jobs.resume(&started.job_id).unwrap();
    assert!(matches!(resumed.state.as_str(), "queued" | "running"));

    let completed = wait_for_terminal(&jobs, &started.job_id);
    assert_eq!(completed.state, "completed", "error: {:?}", completed.error);
    assert_eq!(fs::read(&destination).unwrap(), body);
    assert_eq!(
        if_range_requests.load(Ordering::SeqCst),
        1,
        "the resume must continue from the checkpoint at {paused_at} bytes, \
         which is the only thing that sends If-Range"
    );
}

#[test]
fn a_queued_download_pauses_without_ever_contacting_the_source() {
    let dir = tempfile::tempdir().unwrap();
    let body = vec![9; 512 * 1024];
    let (slow_url, slow_server) = fixture(body.clone(), true);
    let (waiting_url, waiting_server) = fixture(body, false);

    // One slot, so the second download is still queued when it is paused.
    let jobs = Session::in_memory(1);
    let created = jobs
        .enqueue(vec![
            JobDraft {
                checksum: None,
                url: slow_url,
                destination: dir.path().join("slow.bin").display().to_string(),
                not_before_ms: None,
            },
            JobDraft {
                checksum: None,
                url: waiting_url,
                destination: dir.path().join("waiting.bin").display().to_string(),
                not_before_ms: None,
            },
        ])
        .unwrap();

    let waiting_id = created[1].job_id.clone();
    let paused = jobs.pause(&waiting_id).unwrap();
    assert_eq!(paused.state, "paused");
    assert_eq!(paused.bytes_received, 0);

    // The first download finishing must not promote the paused one.
    wait_for_terminal(&jobs, &created[0].job_id);
    thread::sleep(Duration::from_millis(60));
    assert_eq!(jobs.snapshot(&waiting_id).unwrap().state, "paused");

    let resumed = jobs.resume(&waiting_id).unwrap();
    assert!(matches!(resumed.state.as_str(), "queued" | "running"));
    let completed = wait_for_terminal(&jobs, &waiting_id);
    assert_eq!(completed.state, "completed");

    let _ = slow_server.join();
    let _ = waiting_server.join();
}

#[test]
fn a_paused_download_is_still_paused_after_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let state_path = dir.path().join("queue-v1.json");
    let body = vec![4; 256 * 1024];
    let (url, server) = fixture(body, true);

    let waiting_id = {
        let jobs = Session::load(state_path.clone(), 1).unwrap();
        let mut settings = jobs.settings();
        settings.disk_reserve_bytes = 1;
        jobs.update_settings(settings).unwrap();
        let created = jobs
            .enqueue(vec![JobDraft {
                checksum: None,
                url,
                destination: dir.path().join("paused.bin").display().to_string(),
                not_before_ms: None,
            }])
            .unwrap();
        let id = created[0].job_id.clone();
        jobs.pause(&id).unwrap();
        id
    };

    // A fresh process reading the same queue file.
    let reopened = Session::load(state_path, 1).unwrap();
    let restored = reopened.snapshot(&waiting_id).unwrap();
    assert_eq!(
        restored.state, "paused",
        "restoring a paused download as queued would start a transfer the user stopped"
    );

    // And it is still resumable from the new process.
    let resumed = reopened.resume(&waiting_id).unwrap();
    assert!(matches!(resumed.state.as_str(), "queued" | "running"));
    let _ = server.join();
}

#[test]
fn media_downloads_refuse_to_pause_rather_than_pretending() {
    let dir = tempfile::tempdir().unwrap();
    let body = vec![1; 64 * 1024];
    let (url, server) = fixture(body, true);
    let jobs = Session::in_memory(1);
    let created = jobs
        .enqueue(vec![JobDraft {
            checksum: None,
            url,
            destination: dir.path().join("file.bin").display().to_string(),
            not_before_ms: None,
        }])
        .unwrap();
    let id = created[0].job_id.clone();

    // Force the record to look like a media job, which has no checkpoint to
    // resume from. The command must say so instead of dropping the bytes.
    {
        let mut state = jobs.inner.lock().unwrap();
        find_record_mut(&mut state, &id).unwrap().view.kind = "media".into();
    }
    let error = jobs.pause(&id).unwrap_err();
    assert!(
        error.contains("cancelled and started again"),
        "unexpected message: {error}"
    );
    let _ = server.join();
}

#[test]
fn settings_bound_concurrency_and_take_effect_immediately() {
    let dir = tempfile::tempdir().unwrap();
    let jobs = Session::load(dir.path().join("queue-v1.json"), 3).unwrap();
    assert_eq!(jobs.max_active(), 3);

    let mut next = jobs.settings();
    next.max_active_downloads = 999;
    let applied = jobs.update_settings(next).unwrap();
    assert_eq!(applied.max_active_downloads, settings::MAX_ACTIVE_DOWNLOADS);
    assert_eq!(jobs.max_active(), settings::MAX_ACTIVE_DOWNLOADS);

    // And the choice survives a restart.
    let reopened = Session::load(dir.path().join("queue-v1.json"), 3).unwrap();
    assert_eq!(
        reopened.max_active(),
        settings::MAX_ACTIVE_DOWNLOADS,
        "a stored concurrency must win over the launch default"
    );
}

#[test]
fn stats_count_the_same_rows_the_queue_shows() {
    let dir = tempfile::tempdir().unwrap();
    let body = vec![2; 128 * 1024];
    let (first_url, first_server) = fixture(body.clone(), false);
    let (second_url, second_server) = fixture(body, true);
    let jobs = Session::in_memory(1);
    let created = jobs
        .enqueue(vec![
            JobDraft {
                checksum: None,
                url: first_url,
                destination: dir.path().join("one.bin").display().to_string(),
                not_before_ms: None,
            },
            JobDraft {
                checksum: None,
                url: second_url,
                destination: dir.path().join("two.bin").display().to_string(),
                not_before_ms: None,
            },
        ])
        .unwrap();
    wait_for_terminal(&jobs, &created[0].job_id);
    jobs.pause(&created[1].job_id).ok();

    let stats = jobs.stats();
    let rows = jobs.list().unwrap();
    let counted = stats.running
        + stats.queued
        + stats.scheduled
        + stats.paused
        + stats.completed
        + stats.failed;
    assert_eq!(
        counted,
        rows.len(),
        "every row has to be counted exactly once: {stats:?} against {} rows",
        rows.len()
    );
    assert_eq!(stats.max_active_downloads, 1);
    assert!(stats.completed_bytes > 0);

    // Neither fixture is joined. Pausing can win the race against the
    // worker's first socket write, in which case that fixture is still
    // blocked in `accept` and joining it would hang the test rather than
    // reveal anything about the queue.
    drop((first_server, second_server));
}
#[test]
fn production_command_path_downloads_real_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let destination = dir.path().join("desktop.bin");
    let body = vec![0x5a; 128 * 1024];
    let (url, server) = fixture(body.clone(), false);
    let jobs = Session::in_memory(3);
    let started = jobs
        .enqueue(vec![JobDraft {
            checksum: None,
            url,
            destination: destination.display().to_string(),
            not_before_ms: None,
        }])
        .unwrap()
        .remove(0);
    let completed = wait_for_terminal(&jobs, &started.job_id);
    server.join().unwrap();
    assert_eq!(completed.state, "completed");
    assert_eq!(fs::read(&destination).unwrap(), body);
    assert!(completed.observed_sha256.is_some());
}

#[test]
fn a_download_into_a_folder_not_yet_made_gets_it_as_it_starts() {
    let dir = tempfile::tempdir().unwrap();
    let destination = dir
        .path()
        .join("tiny-model")
        .join("onnx")
        .join("model.onnx");
    let body = vec![0x5a; 64 * 1024];
    let (url, server) = fixture(body.clone(), false);
    let jobs = Session::in_memory(3);
    let started = jobs
        .enqueue(vec![JobDraft {
            checksum: None,
            url,
            destination: destination.display().to_string(),
            not_before_ms: None,
        }])
        .unwrap()
        .remove(0);
    let completed = wait_for_terminal(&jobs, &started.job_id);
    server.join().unwrap();
    assert_eq!(completed.state, "completed", "{:?}", completed.error);
    assert_eq!(fs::read(&destination).unwrap(), body);
}

#[test]
fn batch_queue_obeys_concurrency_and_catches_up_due_schedules() {
    let dir = tempfile::tempdir().unwrap();
    let body = vec![7; 512 * 1024];
    let (first_url, first_server) = fixture(body.clone(), true);
    let (second_url, second_server) = fixture(body, false);
    let jobs = Session::in_memory(1);
    let due = now_ms() + 40;
    let created = jobs
        .enqueue(vec![
            JobDraft {
                checksum: None,
                url: first_url,
                destination: dir.path().join("first.bin").display().to_string(),
                not_before_ms: None,
            },
            JobDraft {
                checksum: None,
                url: second_url,
                destination: dir.path().join("second.bin").display().to_string(),
                not_before_ms: Some(due),
            },
        ])
        .unwrap();
    assert_eq!(
        jobs.snapshot(&created[1].job_id).unwrap().state,
        "scheduled"
    );
    thread::sleep(Duration::from_millis(60));
    let after_due = jobs.list().unwrap();
    assert!(after_due.iter().any(|job| {
        // A late wake-up may find it already finished: only "still
        // scheduled" would mean the schedule was not caught up.
        job.job_id == created[1].job_id && job.state != "scheduled"
    }));
    wait_for_terminal(&jobs, &created[0].job_id);
    wait_for_terminal(&jobs, &created[1].job_id);
    first_server.join().unwrap();
    second_server.join().unwrap();
}

#[test]
fn queue_persistence_recovers_safe_sources_and_requests_private_source_refresh() {
    let dir = tempfile::tempdir().unwrap();
    let state_path = dir.path().join("queue.json");
    let mut state = QueueState {
        records: vec![
            QueueRecord::new(
                "http://127.0.0.1:9/safe".into(),
                dir.path().join("safe.bin"),
                Some(now_ms() + 60_000),
            ),
            QueueRecord::new(
                "https://example.test/file?token=secret".into(),
                dir.path().join("private.bin"),
                None,
            ),
        ],
        link_reviews: Vec::new(),
        space_stopped: Default::default(),
    };
    state.records[1].view.state = "cancelled".into();
    state.records[1].finished_at_ms = Some(now_ms());
    state.records[1].view.finished_at_ms = state.records[1].finished_at_ms;
    save_persisted(&state_path, &state).unwrap();
    let text = fs::read_to_string(&state_path).unwrap();
    assert!(!text.contains("secret"));

    let recovered = Session::load(state_path, 1).unwrap().list().unwrap();
    assert!(recovered.iter().any(|job| job.state == "scheduled"));
    assert!(
        recovered.iter().any(|job| {
            job.state == "needs_source" && job.action.as_deref() == Some("edit_link")
        })
    );
}

#[test]
fn destination_conflicts_are_actionable_and_never_overwritten() {
    let dir = tempfile::tempdir().unwrap();
    let destination = dir.path().join("existing.bin");
    fs::write(&destination, b"keep").unwrap();
    let jobs = Session::in_memory(1);
    let record = jobs
        .enqueue(vec![JobDraft {
            checksum: None,
            url: "http://127.0.0.1:9/file".into(),
            destination: destination.display().to_string(),
            not_before_ms: None,
        }])
        .unwrap()
        .remove(0);
    assert_eq!(record.state, "failed");
    assert_eq!(record.action.as_deref(), Some("choose_new_path"));
    assert_eq!(fs::read(destination).unwrap(), b"keep");
}

#[test]
fn input_and_unknown_job_errors_are_user_facing() {
    let jobs = Session::in_memory(1);
    assert!(
        jobs.enqueue(Vec::new())
            .unwrap_err()
            .contains("at least one")
    );
    assert!(jobs.snapshot("missing").unwrap_err().contains("no longer"));
}

#[test]
fn browser_inbox_is_consumed_once_into_the_persistent_queue() {
    use crate::browser_inbox::{BrowserCookie, CaptureRequest, SCHEMA_VERSION};

    let dir = tempfile::tempdir().unwrap();
    let download_dir = dir.path().join("downloads");
    fs::create_dir_all(&download_dir).unwrap();
    let state_path = dir.path().join("queue-v1.json");
    let body = b"browser capture bytes".repeat(1024);
    let (url, server) = fixture(body.clone(), false);
    let capture_id = uuid::Uuid::new_v4().to_string();
    let store = BridgeStore::new(dir.path().to_path_buf());
    store
        .accept(&CaptureRequest {
            schema_version: SCHEMA_VERSION,
            capture_id: capture_id.clone(),
            method: "GET".into(),
            url: format!("{url}/archive.bin?token=private"),
            suggested_filename: "archive.bin".into(),
            referrer: None,
            cookies: Vec::<BrowserCookie>::new(),
            user_initiated: true,
        })
        .unwrap();

    let jobs =
        Session::load_with_browser(state_path.clone(), 1, Some(download_dir.clone())).unwrap();
    let mut settings = jobs.settings();
    settings.disk_reserve_bytes = 1;
    jobs.update_settings(settings).unwrap();
    let queued = jobs.list().unwrap();
    assert_eq!(queued.len(), 1);
    let completed = wait_for_terminal(&jobs, &queued[0].job_id);
    server.join().unwrap();
    assert_eq!(completed.state, "completed");
    assert_eq!(fs::read(download_dir.join("archive.bin")).unwrap(), body);
    assert!(store.pending().unwrap().is_empty());
    let persisted = fs::read_to_string(&state_path).unwrap();
    assert!(!persisted.contains("token=private"));

    let restored = Session::load_with_browser(state_path, 1, Some(download_dir))
        .unwrap()
        .list()
        .unwrap();
    assert_eq!(restored.len(), 1);
    assert_eq!(restored[0].job_id, queued[0].job_id);
}

#[test]
fn browser_captures_follow_the_rules_like_any_new_download() {
    use crate::browser_inbox::{BrowserCookie, CaptureRequest, SCHEMA_VERSION};
    use fetchpath_protocol::model::{RuleActions, RuleConditions, RuleSpec};

    let dir = tempfile::tempdir().unwrap();
    let (downloads, isos) = (dir.path().join("downloads"), dir.path().join("isos"));
    let jobs =
        Session::load_with_browser(dir.path().join("queue-v1.json"), 1, Some(downloads.clone()))
            .unwrap();
    let rule = |types: &str, then: RuleActions| RuleSpec {
        name: None,
        when: RuleConditions {
            file_types: vec![types.into()],
            ..Default::default()
        },
        then,
    };
    let folder = RuleActions {
        folder: Some(isos.display().to_string()),
        ..Default::default()
    };
    let checksum = RuleActions {
        require_checksum: true,
        ..Default::default()
    };
    jobs.add_rule(rule("iso", folder), None).unwrap();
    jobs.add_rule(rule("exe", checksum), None).unwrap();

    let store = BridgeStore::new(dir.path().to_path_buf());
    for name in ["disc.iso", "setup.exe", "notes.txt"] {
        store
            .accept(&CaptureRequest {
                schema_version: SCHEMA_VERSION,
                capture_id: uuid::Uuid::new_v4().to_string(),
                method: "GET".into(),
                url: format!("http://127.0.0.1:9/{name}"),
                suggested_filename: name.into(),
                referrer: None,
                cookies: Vec::<BrowserCookie>::new(),
                user_initiated: true,
            })
            .unwrap();
    }
    let queued = jobs.list().unwrap();
    let placed = |name: &str| {
        queued
            .iter()
            .filter_map(|job| job.destination.as_deref())
            .find(|path| path.ends_with(name))
            .map(PathBuf::from)
    };
    assert_eq!(placed("disc.iso"), Some(isos.join("disc.iso")));
    assert_eq!(placed("notes.txt"), Some(downloads.join("notes.txt")));
    // No checksum yet, so it waits in Add download instead of queuing.
    assert_eq!(placed("setup.exe"), None);
    assert_eq!(jobs.take_link_reviews(), ["http://127.0.0.1:9/setup.exe"]);
    assert!(store.pending().unwrap().is_empty());
}

/// A YouTube page sent from the browser used to be queued as a file, which
/// saved the page's HTML. It now waits for Add download instead.
#[test]
fn a_captured_media_page_goes_to_review_instead_of_the_file_queue() {
    use crate::browser_inbox::{BrowserCookie, CaptureRequest, SCHEMA_VERSION};

    let dir = tempfile::tempdir().unwrap();
    let download_dir = dir.path().join("downloads");
    fs::create_dir_all(&download_dir).unwrap();
    let store = BridgeStore::new(dir.path().to_path_buf());
    let page = "https://www.youtube.com/watch?v=arj7oStGLkU";
    store
        .accept(&CaptureRequest {
            schema_version: SCHEMA_VERSION,
            capture_id: uuid::Uuid::new_v4().to_string(),
            method: "GET".into(),
            url: page.into(),
            suggested_filename: "watch".into(),
            referrer: None,
            cookies: Vec::<BrowserCookie>::new(),
            user_initiated: true,
        })
        .unwrap();

    let credential_ref = store.pending().unwrap()[0].credential_ref.clone();
    let queue = dir.path().join("queue-v1.json");
    let jobs = Session::load_with_browser(queue.clone(), 1, Some(download_dir.clone())).unwrap();
    assert!(jobs.list().unwrap().is_empty(), "not queued as a file");
    jobs.list().unwrap();
    // FP-056: taken in, but still pending in the inbox until a client
    // takes it, so an engine leaving now loses nothing.
    assert_eq!(store.pending().unwrap().len(), 1, "kept until handed out");
    drop(jobs);
    let jobs = Session::load_with_browser(queue.clone(), 1, Some(download_dir.clone())).unwrap();
    jobs.list().unwrap();
    jobs.list().unwrap();
    assert_eq!(
        jobs.take_link_reviews(),
        vec![page.to_owned()],
        "once, after a restart"
    );
    assert!(store.pending().unwrap().is_empty(), "handed out, then done");
    assert!(
        store.load_secret(&credential_ref).is_err(),
        "its unused protected context is deleted"
    );
    jobs.list().unwrap();
    assert!(jobs.take_link_reviews().is_empty(), "never offered twice");
    drop(jobs);
    let jobs = Session::load_with_browser(queue, 1, Some(download_dir)).unwrap();
    jobs.list().unwrap();
    assert!(jobs.take_link_reviews().is_empty());
}

#[test]
fn media_pages_are_told_apart_from_files_on_media_sites() {
    assert!(is_media_page("https://www.youtube.com/watch?v=abc"));
    assert!(is_media_page("https://youtu.be/abc"));
    assert!(is_media_page("https://m.youtube.com/shorts/abc"));
    assert!(is_media_page("https://vimeo.com/12345"));
    assert!(!is_media_page("https://archive.org/download/item/film.mp4"));
    assert!(!is_media_page("https://example.com/watch?v=abc"));
    assert!(!is_media_page("https://notyoutube.com/watch"));
    assert!(!is_media_page("not a url"));
}

#[cfg(windows)]
#[test]
#[ignore = "requires isolated CARGO_TARGET_DIR; run explicitly with --ignored"]
fn automatic_policy_restarts_running_helpers_and_applies_at_start() {
    use sha2::Digest;
    let helper = std::env::current_exe()
        .unwrap()
        .with_file_name("fetchpath-torrent-helper.exe");
    // This fixture belongs to an isolated target directory; never replace
    // an installed/previously built helper.
    assert!(
        !helper.exists(),
        "run this test in its own CARGO_TARGET_DIR"
    );
    let fixture =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/policy-torrent-helper.rs");
    assert!(
        std::process::Command::new("rustc")
            .arg(fixture)
            .arg("--edition=2024")
            .arg("-o")
            .arg(&helper)
            .status()
            .unwrap()
            .success()
    );
    struct RemoveHelper(PathBuf);
    impl Drop for RemoveHelper {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.0);
        }
    }
    let _remove = RemoveHelper(helper);
    let fixtures =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../adapters/media/tests/fixtures");
    let tools = MediaTools::new(
        fixtures.join("burst-helper.cmd"),
        &fixtures,
        fixtures.join("fake-ffprobe-video.cmd"),
    )
    .unwrap();
    for torrent in [false, true] {
        for boundary in 0..3 {
            let scheduled = boundary == 1;
            let output = tempfile::tempdir().unwrap();
            let destination = output
                .path()
                .join(if torrent { "torrent" } else { "video.mp4" });
            let jobs = Session::in_memory_with_media(1, tools.clone());
            let agent = AgentName::try_from("helper").unwrap();
            let mut access = AgentPolicy {
                folders: vec![output.path().display().to_string()],
                max_bytes: 256 * 1024,
                max_new_jobs_per_hour: 20,
                automatic: false,
                permission_mode: None,
            };
            jobs.set_agent_policy(agent.clone(), Some(access.clone()))
                .unwrap();
            let due = scheduled.then(|| now_ms() + 60_000);
            let mut record = if torrent {
                QueueRecord::new_torrent(
                    "magnet:?xt=urn:btih:0123456789012345678901234567890123456789".into(),
                    destination.clone(),
                    false,
                    due,
                    TorrentPolicy {
                        discover_peers: true,
                        upload: false,
                    },
                )
            } else {
                QueueRecord::new_media(
                    "https://example.test/watch?v=1".into(),
                    destination.clone(),
                    due,
                    "video-18".into(),
                    "360p".into(),
                    tools.clone(),
                )
            };
            record.principal = Principal::Agent(agent.clone());
            let id = record.id.clone();
            jobs.inner.lock().unwrap().records.push(record);
            let before = jobs.snapshot(&id).unwrap();
            assert_eq!(
                before.state,
                if scheduled { "scheduled" } else { "running" }
            );
            let previous = jobs.inner.lock().unwrap().records[0].job.clone().unwrap();
            if torrent && !scheduled {
                let deadline = std::time::Instant::now() + Duration::from_secs(10);
                while jobs.snapshot(&id).unwrap().bytes_received != 1 {
                    assert!(std::time::Instant::now() < deadline);
                    thread::sleep(Duration::from_millis(20));
                }
            }
            if boundary == 2 {
                let mut state = jobs.inner.lock().unwrap();
                let record = &mut state.records[0];
                record.job.as_ref().unwrap().cancel();
                record.hold(vec![ApprovalReason::SizeLimit]);
            }
            access.automatic = true;
            jobs.set_agent_policy(agent, Some(access)).unwrap();
            if scheduled {
                jobs.start_now(&id).unwrap();
            }
            let deadline = std::time::Instant::now() + Duration::from_secs(30);
            let finished = loop {
                let snapshot = jobs.snapshot(&id).unwrap();
                assert_ne!(snapshot.state, "awaiting_approval", "{snapshot:?}");
                if is_terminal(&snapshot.state) {
                    break snapshot;
                }
                assert!(std::time::Instant::now() < deadline, "{snapshot:?}");
                thread::sleep(Duration::from_millis(50));
            };
            assert_eq!(finished.state, "completed", "{finished:?}");
            assert_eq!(finished.bytes_received, 2 * 1024 * 1024);
            if torrent && !scheduled {
                assert!(matches!(previous, JobHandle::Torrent(ref job)
                    if job.snapshot().state == TorrentJobState::Cancelled));
            }
            if !torrent {
                assert_eq!(fs::metadata(&destination).unwrap().len(), 2 * 1024 * 1024);
            }
            if torrent && scheduled {
                // Reuse the completed helper to prove failed and deferred
                // saves cannot acknowledge publication recovery evidence.
                let source = match jobs.inner.lock().unwrap().records[0].job.as_ref().unwrap() {
                    JobHandle::Torrent(job) => job.request().source.clone(),
                    _ => unreachable!(),
                };
                let hash = format!("{:x}", sha2::Sha256::digest(source.as_bytes()));
                let marker = output.path().join(format!(
                    "torrent.fetchpath-{id}-{}.part.infohash",
                    &hash[..32]
                ));
                fs::write(&marker, b"recovery evidence").unwrap();
                let blocked = output.path().join("blocked-parent");
                fs::write(&blocked, b"not a directory").unwrap();
                let queue = blocked.join("queue.json");
                let mut persistent = jobs;
                persistent.state_path = Some(queue.clone());
                let mut state = persistent.inner.lock().unwrap();
                persistent.durable.lock().unwrap().defer = true;
                persistent.save_locked(&mut state).unwrap();
                assert!(marker.exists());
                persistent.durable.lock().unwrap().defer = false;
                assert!(persistent.save_locked(&mut state).is_err());
                assert!(marker.exists());
                fs::remove_file(&blocked).unwrap();
                persistent.save_locked(&mut state).unwrap();
                assert!(!marker.exists());
            }
        }
    }
    // Publication is separate from worker completion. Persist its
    // intermediate snapshot, stop the process without another save, and
    // reload exactly that durable state as an engine crash would.
    let output = tempfile::tempdir().unwrap();
    let queue = output.path().join("queue.json");
    let root = output.path().join("downloads");
    fs::create_dir(&root).unwrap();
    let published = root.join("Album");
    fs::create_dir(&published).unwrap();
    let source = "https://example.test/publication-window.torrent".to_string();
    let jobs = Session::load(queue.clone(), 1).unwrap();
    let record = QueueRecord::new_torrent(
        source.clone(),
        root.clone(),
        true,
        None,
        TorrentPolicy {
            discover_peers: true,
            upload: false,
        },
    );
    let id = record.id.clone();
    let hash = format!("{:x}", sha2::Sha256::digest(source.as_bytes()));
    let marker = root.join(format!(".fetchpath-{id}-{}.part.infohash", &hash[..32]));
    fs::write(&marker, b"published recovery evidence").unwrap();
    jobs.inner.lock().unwrap().records.push(record);
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let snapshot = jobs.snapshot(&id).unwrap();
        assert_eq!(snapshot.state, "running", "{snapshot:?}");
        if snapshot.destination.as_deref() == Some(published.to_str().unwrap()) {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "{snapshot:?}");
        thread::sleep(Duration::from_millis(20));
    }
    let saved = load_persisted(&queue).unwrap().current().unwrap();
    assert_eq!(saved.records[0].destination, root.display().to_string());
    assert!(saved.records[0].torrent_auto);
    assert_eq!(saved.records[0].view.state, "running");
    assert!(marker.exists());
    let job = jobs.inner.lock().unwrap().records[0].job.clone().unwrap();
    job.cancel();
    job.join();
    drop(jobs);
    fs::write(root.join("replay-publication"), b"resume after crash").unwrap();
    let recovered = Session::load(queue.clone(), 1).unwrap();
    {
        let state = recovered.inner.lock().unwrap();
        let JobHandle::Torrent(job) = state.records[0].job.as_ref().unwrap() else {
            unreachable!()
        };
        assert!(job.request().auto_name);
        assert_eq!(job.request().destination, root.display().to_string());
    }
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let snapshot = recovered.snapshot(&id).unwrap();
        if is_terminal(&snapshot.state) {
            assert_eq!(snapshot.state, "completed", "{snapshot:?}");
            assert_eq!(snapshot.destination.as_deref(), published.to_str());
            break;
        }
        assert!(std::time::Instant::now() < deadline, "{snapshot:?}");
        thread::sleep(Duration::from_millis(20));
    }
    assert!(
        !marker.exists(),
        "durable completion acknowledges the original marker"
    );
    assert!(!root.join("Album (2)").exists());
    let saved = load_persisted(&queue).unwrap().current().unwrap();
    assert_eq!(
        saved.records[0].destination,
        published.display().to_string()
    );
    assert!(!saved.records[0].torrent_auto);
}

/// An agent's video download stops at the agent's size limit and waits
/// for the person, as a file download does (FP-067): the media helper's
/// bytes are measured while it writes.
#[cfg(windows)]
#[test]
fn an_agent_media_download_stops_at_its_size_limit() {
    let fixtures =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../adapters/media/tests/fixtures");
    let tools = MediaTools::new(
        fixtures.join("growing-helper.cmd"),
        &fixtures,
        fixtures.join("fake-ffprobe.cmd"),
    )
    .unwrap();
    let output = tempfile::tempdir().unwrap();
    let destination = output.path().join("video.mp4");
    let jobs = Session::in_memory_with_media(1, tools.clone());
    let agent = AgentName::try_from("helper").unwrap();
    jobs.set_agent_policy(
        agent.clone(),
        Some(AgentPolicy {
            folders: vec![output.path().display().to_string()],
            max_bytes: 256 * 1024,
            max_new_jobs_per_hour: 20,
            automatic: false,
            permission_mode: None,
        }),
    )
    .unwrap();
    let mut record = QueueRecord::new_media(
        "https://example.test/watch?v=1".into(),
        destination.clone(),
        None,
        "video-18".into(),
        "360p".into(),
        tools,
    );
    record.principal = Principal::Agent(agent);
    let job_id = record.id.clone();
    jobs.inner
        .lock()
        .expect("desktop jobs poisoned")
        .records
        .push(record);

    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        let snapshot = jobs.snapshot(&job_id).unwrap();
        if snapshot.state == "awaiting_approval" {
            break;
        }
        assert!(
            !is_terminal(&snapshot.state),
            "finished instead: {snapshot:?}"
        );
        assert!(
            std::time::Instant::now() < deadline,
            "never stopped: {snapshot:?}"
        );
        thread::sleep(Duration::from_millis(50));
    }
    let reasons = {
        let state = jobs.inner.lock().expect("desktop jobs poisoned");
        find_record(&state, &job_id)
            .unwrap()
            .approval
            .as_ref()
            .map(|approval| approval.reasons.clone())
    };
    assert_eq!(reasons, Some(vec![ApprovalReason::SizeLimit]));
    thread::sleep(Duration::from_secs(1));
    assert!(!destination.exists(), "nothing is published while it waits");
}

/// A helper that writes everything at once still cannot get past the
/// agent's limit (FP-067 review): the adapter's cap refuses to publish,
/// and the stop waits for the person as a size stop. Approving it lifts
/// the cap and the download is saved.
#[cfg(windows)]
#[test]
fn an_agent_media_burst_past_its_limit_waits_and_approval_lifts_it() {
    let fixtures =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../adapters/media/tests/fixtures");
    let tools = MediaTools::new(
        fixtures.join("burst-helper.cmd"),
        &fixtures,
        fixtures.join("fake-ffprobe-video.cmd"),
    )
    .unwrap();
    let output = tempfile::tempdir().unwrap();
    let destination = output.path().join("video.mp4");
    let jobs = Session::in_memory_with_media(1, tools.clone());
    let agent = AgentName::try_from("helper").unwrap();
    jobs.set_agent_policy(
        agent.clone(),
        Some(AgentPolicy {
            folders: vec![output.path().display().to_string()],
            max_bytes: 256 * 1024,
            max_new_jobs_per_hour: 20,
            automatic: false,
            permission_mode: None,
        }),
    )
    .unwrap();
    let mut record = QueueRecord::new_media(
        "https://example.test/watch?v=2".into(),
        destination.clone(),
        None,
        "video-18".into(),
        "360p".into(),
        tools,
    );
    record.principal = Principal::Agent(agent);
    let job_id = record.id.clone();
    jobs.inner
        .lock()
        .expect("desktop jobs poisoned")
        .records
        .push(record);

    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        let snapshot = jobs.snapshot(&job_id).unwrap();
        if snapshot.state == "awaiting_approval" {
            break;
        }
        assert!(
            !is_terminal(&snapshot.state),
            "finished instead: {snapshot:?}"
        );
        assert!(
            std::time::Instant::now() < deadline,
            "never stopped: {snapshot:?}"
        );
        thread::sleep(Duration::from_millis(50));
    }
    assert!(
        !destination.exists(),
        "a burst past the limit was published"
    );

    jobs.approve(&job_id).unwrap();
    // The helpers are PowerShell scripts, slow to start under load.
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    let finished = loop {
        let snapshot = jobs.snapshot(&job_id).unwrap();
        if is_terminal(&snapshot.state) {
            break snapshot;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "never finished: {snapshot:?}"
        );
        thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(finished.state, "completed", "{finished:?}");
    assert_eq!(
        std::fs::metadata(&destination).unwrap().len(),
        2 * 1024 * 1024
    );
}

#[cfg(windows)]
#[test]
fn media_expiry_is_a_refreshable_queue_failure() {
    let fixtures =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../adapters/media/tests/fixtures");
    let tools = MediaTools::new(
        fixtures.join("expired-helper.cmd"),
        &fixtures,
        fixtures.join("fake-ffprobe.cmd"),
    )
    .unwrap();
    let output = tempfile::tempdir().unwrap();
    let destination = output.path().join("media.mp4");
    let jobs = Session::in_memory_with_media(1, tools.clone());
    let record = QueueRecord::new_media(
        "https://example.test/watch?session=private".into(),
        destination.clone(),
        None,
        "video-fixture".into(),
        "720p".into(),
        tools,
    );
    let job_id = record.id.clone();
    jobs.inner
        .lock()
        .expect("desktop jobs poisoned")
        .records
        .push(record);

    let failed = wait_for_terminal(&jobs, &job_id);
    assert_eq!(failed.kind, "media");
    assert_eq!(failed.quality_label.as_deref(), Some("720p"));
    assert_eq!(failed.state, "failed");
    assert_eq!(failed.action.as_deref(), Some("refresh_source"));
    assert!(!failed.error.unwrap().contains("private"));
    assert!(!destination.exists());
}

fn draft(url: &str, destination: &Path) -> JobDraft {
    JobDraft {
        checksum: None,
        url: url.into(),
        destination: destination.display().to_string(),
        not_before_ms: None,
    }
}

#[test]
fn destinations_cannot_escape_the_chosen_directory() {
    let dir = tempfile::tempdir().unwrap();
    let jobs = Session::in_memory(1);

    let traversal = dir.path().join("..").join("escaped.bin");
    let error = jobs
        .enqueue(vec![draft("http://127.0.0.1:9/file", &traversal)])
        .unwrap_err();
    assert!(error.contains(".."), "unexpected traversal error: {error}");

    let relative = jobs
        .enqueue(vec![JobDraft {
            checksum: None,
            url: "http://127.0.0.1:9/file".into(),
            destination: "escaped.bin".into(),
            not_before_ms: None,
        }])
        .unwrap_err();
    assert!(
        relative.contains("full destination path"),
        "unexpected relative-path error: {relative}"
    );

    let sibling = dir.path().join("sibling");
    fs::create_dir_all(&sibling).unwrap();
    let split_batch = jobs
        .enqueue(vec![
            draft("http://127.0.0.1:9/one", &dir.path().join("one.bin")),
            draft("http://127.0.0.1:9/two", &sibling.join("two.bin")),
        ])
        .unwrap_err();
    assert!(
        split_batch.contains("same folder"),
        "unexpected batch-containment error: {split_batch}"
    );
    assert!(!dir.path().join("one.bin").exists());
}

#[test]
fn malformed_ipc_input_is_rejected_with_a_user_facing_error() {
    let dir = tempfile::tempdir().unwrap();
    let jobs = Session::in_memory(1);

    for (destination, expected) in [
        ("NUL", "full destination path"),
        ("trailing.", "full destination path"),
    ] {
        let error = jobs
            .enqueue(vec![JobDraft {
                checksum: None,
                url: "http://127.0.0.1:9/file".into(),
                destination: destination.into(),
                not_before_ms: None,
            }])
            .unwrap_err();
        assert!(error.contains(expected), "{destination}: {error}");
    }

    for name in ["NUL.bin", "COM1.bin"] {
        let error = jobs
            .enqueue(vec![draft(
                "http://127.0.0.1:9/file",
                &dir.path().join(name),
            )])
            .unwrap_err();
        assert!(error.contains("reserved by Windows"), "{name}: {error}");
    }

    let trailing_dot = jobs
        .enqueue(vec![draft(
            "http://127.0.0.1:9/file",
            &dir.path().join("name."),
        )])
        .unwrap_err();
    assert!(trailing_dot.contains("dot or a space"), "{trailing_dot}");

    // An alternate data stream would otherwise pass every other check and
    // attach bytes to a file the user never chose.
    let stream = jobs
        .enqueue(vec![JobDraft {
            checksum: None,
            url: "http://127.0.0.1:9/file".into(),
            destination: format!("{}\\notes.txt:hidden", dir.path().display()),
            not_before_ms: None,
        }])
        .unwrap_err();
    assert!(stream.contains("cannot contain :"), "{stream}");
    assert!(!dir.path().join("notes.txt").exists());

    // The drive letter's colon must still be accepted.
    let accepted = jobs.enqueue(vec![draft(
        "http://127.0.0.1:9/file",
        &dir.path().join("plain.bin"),
    )]);
    assert!(accepted.is_ok(), "{accepted:?}");
    jobs.remove(&accepted.unwrap()[0].job_id).unwrap();

    let control = jobs
        .enqueue(vec![JobDraft {
            checksum: None,
            url: "http://127.0.0.1:9/file".into(),
            destination: format!("{}\u{1}bad.bin", dir.path().display()),
            not_before_ms: None,
        }])
        .unwrap_err();
    assert!(control.contains("Windows cannot use"), "{control}");

    for (url, expected) in [
        (
            "file:///C:/Windows/System32/drivers/etc/hosts",
            "not an HTTP or HTTPS address",
        ),
        ("javascript:alert(1)", "not an HTTP or HTTPS address"),
        ("http://", "could not be read as a web address"),
        ("http://host\u{1}/file", "characters Fetchpath cannot use"),
        ("   ", "at least one download address"),
    ] {
        let error = jobs
            .enqueue(vec![draft(url, &dir.path().join("out.bin"))])
            .unwrap_err();
        assert!(error.contains(expected), "{url}: {error}");
    }

    let oversized = format!("https://example.test/{}", "a".repeat(MAX_SOURCE_LENGTH));
    let error = jobs
        .enqueue(vec![draft(&oversized, &dir.path().join("out.bin"))])
        .unwrap_err();
    assert!(error.contains("too long"), "{error}");
    assert!(!error.contains("aaaa"), "oversized address was echoed back");

    assert!(
        jobs.retry("not-a-job", None, None, None)
            .unwrap_err()
            .contains("no longer")
    );
    assert!(jobs.cancel("not-a-job").is_err());
    assert!(jobs.remove("not-a-job").is_err());
    assert!(jobs.start_now("not-a-job").is_err());
    assert!(jobs.list().unwrap().is_empty());
}

#[test]
fn credentials_and_private_queries_never_reach_state_or_errors() {
    let dir = tempfile::tempdir().unwrap();
    let jobs = Session::in_memory(1);

    let error = jobs
        .enqueue(vec![draft(
            "https://user:hunter2@example.test/file.bin",
            &dir.path().join("out.bin"),
        )])
        .unwrap_err();
    assert!(!error.contains("hunter2"), "password echoed: {error}");
    assert!(error.contains("user name or password"), "{error}");

    assert_eq!(
        display_url("https://user:hunter2@example.test/file?token=abc"),
        "https://…@example.test/file?…"
    );
    assert_eq!(
        display_url("https://example.test/plain.bin"),
        "https://example.test/plain.bin"
    );

    let state_path = dir.path().join("queue-v1.json");
    let state = QueueState {
        records: vec![QueueRecord::new(
            "https://example.test/file?token=secret-value#frag".into(),
            dir.path().join("private.bin"),
            None,
        )],
        link_reviews: Vec::new(),
        space_stopped: Default::default(),
    };
    save_persisted(&state_path, &state).unwrap();
    let text = fs::read_to_string(&state_path).unwrap();
    assert!(!text.contains("secret-value"));
    assert!(!text.contains("frag"));
    for record in &state.records {
        assert!(record.restart_url.is_none());
        assert!(!record.view.source.contains("secret-value"));
    }
}

/// A cancel that lands while a download stopped at the disk reserve is
/// being joined, or after it was requeued without a worker, must win.
#[test]
fn a_cancel_during_a_reserve_stop_is_not_lost() {
    let jobs = Session::in_memory(0);
    let dir = tempfile::tempdir().unwrap();
    let started = jobs
        .enqueue(vec![JobDraft {
            checksum: None,
            url: "http://127.0.0.1:9/file.bin".into(),
            destination: dir.path().join("a.bin").display().to_string(),
            not_before_ms: None,
        }])
        .unwrap()
        .remove(0);
    // A drive nothing fits on, so a requeued download stays waiting.
    jobs.settings.lock().unwrap().disk_reserve_bytes = 1 << 60;
    for stopping in [true, false] {
        {
            let mut state = jobs.inner.lock().unwrap();
            let record = find_record_mut(&mut state, &started.job_id).unwrap();
            record.job = None;
            record.view.state = "queued".into();
            record.view.waiting_for_space = true;
            if stopping {
                state.space_stopped.insert(started.job_id.clone());
            }
        }
        let response = jobs.cancel(&started.job_id).unwrap();
        assert_eq!(response.outcome, "accepted", "stopping={stopping}");
        assert_eq!(response.job.state, "cancelled", "stopping={stopping}");
        assert!(!response.job.waiting_for_space);
        // Nothing restarts it afterwards.
        jobs.reconcile_locked(&mut jobs.inner.lock().unwrap());
        let state = jobs.inner.lock().unwrap();
        let record = find_record(&state, &started.job_id).unwrap();
        assert_eq!(record.view.state, "cancelled");
        assert!(record.job.is_none());
    }
}

fn slow_probe(_folder: &Path) -> Option<(PathBuf, space::Space)> {
    std::thread::sleep(Duration::from_millis(2_000));
    None
}

/// A dead network share must not hold the queue lock while it is measured.
#[test]
fn a_slow_drive_measurement_never_blocks_the_queue() {
    let mut jobs = Session::in_memory(1);
    jobs.space = space::SpaceCache::new(slow_probe);
    let dir = tempfile::tempdir().unwrap();
    let started = std::time::Instant::now();
    let job = jobs
        .enqueue(vec![JobDraft {
            checksum: None,
            url: "http://127.0.0.1:9/file.bin".into(),
            destination: dir.path().join("a.bin").display().to_string(),
            not_before_ms: None,
        }])
        .unwrap()
        .remove(0);
    jobs.list().unwrap();
    jobs.cancel(&job.job_id).unwrap();
    assert!(
        started.elapsed() < Duration::from_millis(1_000),
        "queue calls waited {:?} on a drive measurement",
        started.elapsed()
    );
}

#[test]
fn is_media_page_routes_dotted_handles_to_review_and_keeps_direct_files() {
    assert!(is_media_page("https://www.tiktok.com/@john.doe"));
    assert!(is_media_page("https://www.youtube.com/@dr.mike"));
    assert!(is_media_page("https://archive.org/details/ubuntu-24.04"));
    assert!(!is_media_page(
        "https://archive.org/download/item/movie.mp4"
    ));
    assert!(!is_media_page("https://example.com/watch?v=1"));
}

#[test]
fn missing_browser_secret_is_quarantined_without_poisoning_list() {
    use fetchpath_browser_inbox::{BridgeStore, CaptureRequest};

    let root = tempfile::tempdir().unwrap();
    let data = root.path().join("data");
    let downloads = root.path().join("downloads");
    std::fs::create_dir_all(&data).unwrap();
    std::fs::create_dir_all(&downloads).unwrap();
    let inbox = BridgeStore::new(data.clone());
    let bad_id = uuid::Uuid::new_v4().to_string();
    let good_id = uuid::Uuid::new_v4().to_string();
    for (id, name) in [(&bad_id, "bad.bin"), (&good_id, "good.bin")] {
        let req = CaptureRequest {
            schema_version: 1,
            capture_id: id.clone(),
            method: "GET".into(),
            url: format!("http://127.0.0.1:9/{name}"),
            suggested_filename: name.into(),
            referrer: None,
            cookies: Vec::new(),
            user_initiated: true,
        };
        assert!(!inbox.accept(&req).unwrap());
    }
    let bad_ref = inbox
        .pending()
        .unwrap()
        .into_iter()
        .find(|r| r.capture_id == bad_id)
        .unwrap()
        .credential_ref;
    inbox.remove_secret(&bad_ref).unwrap();

    let session =
        Session::load_with_browser(data.join("queue-v1.json"), 1, Some(downloads)).unwrap();
    let listed = session.list().unwrap();
    assert_eq!(listed.len(), 1);
    assert!(
        listed[0]
            .destination
            .as_deref()
            .unwrap_or_default()
            .ends_with("good.bin")
    );
}
