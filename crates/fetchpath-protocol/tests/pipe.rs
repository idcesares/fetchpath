//! The authenticated named pipe (FP-052): access lists, the handshake, and
//! per-connection limits, each exercised against a real pipe.
#![cfg(windows)]

use fetchpath_protocol::command::{Command, CommandEnvelope};
use fetchpath_protocol::frame::{read_frame, write_frame};
use fetchpath_protocol::message::{
    CommandResult, EventPayload, JobEvent, Reply, ReplyResult, ServerMessage, StreamPosition,
};
use fetchpath_protocol::pipe::auth::{self, EngineSecret, Handshake, Nonce, SECRET_BYTES};
use fetchpath_protocol::pipe::{
    ENGINE_PIPE_PREFIX, Limits, PipeClient, PipeEngineClient, PipeListener, PipeName,
    current_user_sid, endpoint, file_security_sddl,
};
use fetchpath_protocol::principal::Principal;
use fetchpath_protocol::{
    ClientId, EngineClient, JobId, ProtocolError, SCHEMA_VERSION, StreamItem, Timestamp,
    decode_server_message,
};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

const SECRET: [u8; SECRET_BYTES] = [0x42; SECRET_BYTES];

fn secret() -> EngineSecret {
    EngineSecret::from_bytes(SECRET)
}

fn unique_name() -> PipeName {
    PipeName::named(&format!("fetchpath-test-{}", uuid::Uuid::new_v4()))
}

fn fast_limits() -> Limits {
    Limits {
        handshake_timeout: Duration::from_millis(800),
        frame_timeout: Duration::from_millis(800),
        idle_timeout: Some(Duration::from_secs(5)),
        write_timeout: Duration::from_secs(2),
        ..Limits::default()
    }
}

#[derive(Debug)]
enum Seen {
    /// With the principal the client declared.
    Authenticated(String),
    AuthFailed(String),
    Command(CommandEnvelope),
    Ended(Option<String>),
}

#[derive(Clone, Copy, PartialEq)]
enum Respond {
    /// Answer every command.
    Reply,
    /// Never answer.
    Silent,
    /// Close the first connection after its first command, unanswered.
    DropFirst,
}

struct Engine {
    name: PipeName,
    seen: Receiver<Seen>,
    stop: Arc<AtomicBool>,
    accept: Option<thread::JoinHandle<()>>,
}

impl Engine {
    fn start(limits: Limits, respond: Respond) -> Self {
        let name = unique_name();
        let mut listener = PipeListener::bind(&name, secret(), limits).unwrap();
        let (tx, seen) = channel();
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = Arc::clone(&stop);
        let dropped_once = Arc::new(Mutex::new(false));
        let accept = thread::spawn(move || {
            while !stopping.load(Ordering::SeqCst) {
                let Some(pending) = listener.accept(Some(Duration::from_millis(50))).unwrap()
                else {
                    continue;
                };
                let tx: Sender<Seen> = tx.clone();
                let dropped_once = Arc::clone(&dropped_once);
                thread::spawn(move || serve(pending, tx, respond, dropped_once));
            }
        });
        Self {
            name,
            seen,
            stop,
            accept: Some(accept),
        }
    }

    fn next(&self) -> Seen {
        self.seen
            .recv_timeout(Duration::from_secs(10))
            .expect("the engine reported nothing in time")
    }

    /// The next report other than a successful authentication.
    fn next_outcome(&self) -> Seen {
        loop {
            match self.next() {
                Seen::Authenticated(_) => {}
                other => return other,
            }
        }
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(accept) = self.accept.take() {
            let _ = accept.join();
        }
    }
}

fn serve(
    pending: fetchpath_protocol::pipe::PendingConnection,
    tx: Sender<Seen>,
    respond: Respond,
    dropped_once: Arc<Mutex<bool>>,
) {
    let connection = match pending.authenticate() {
        Ok(connection) => connection,
        Err(error) => {
            let _ = tx.send(Seen::AuthFailed(error.code.to_string()));
            return;
        }
    };
    let _ = tx.send(Seen::Authenticated(connection.principal().to_string()));
    let sender = connection.sender();
    loop {
        match connection.receive() {
            Ok(Some(envelope)) => {
                let _ = tx.send(Seen::Command(envelope.clone()));
                if respond == Respond::DropFirst {
                    let mut dropped = dropped_once.lock().unwrap();
                    if !*dropped {
                        *dropped = true;
                        return;
                    }
                }
                if respond == Respond::Silent {
                    continue;
                }
                let result = match &envelope.payload {
                    Command::SubscribeQueue { after_cursor } => {
                        // A subscriber only listens from here on.
                        connection.set_idle_timeout(None);
                        sender
                            .send(&ServerMessage::Reply(Reply::ok(
                                envelope.command_id.clone(),
                                CommandResult::Subscribed {
                                    position: StreamPosition::Queue {
                                        after_cursor: *after_cursor,
                                    },
                                },
                            )))
                            .unwrap();
                        for seq in 1..=2 {
                            sender.send(&ServerMessage::Event(event(seq))).unwrap();
                        }
                        continue;
                    }
                    Command::GetJob { job_id } => CommandResult::Removed {
                        job_id: job_id.clone(),
                    },
                    _ => CommandResult::ShuttingDown,
                };
                let _ = sender.send(&ServerMessage::Reply(Reply::ok(
                    envelope.command_id,
                    result,
                )));
            }
            Ok(None) => {
                let _ = tx.send(Seen::Ended(None));
                return;
            }
            Err(error) => {
                let _ = tx.send(Seen::Ended(Some(error.code.to_string())));
                return;
            }
        }
    }
}

fn event(seq: u64) -> JobEvent {
    JobEvent {
        schema_version: SCHEMA_VERSION,
        job_id: job(),
        seq,
        cursor: 100 + seq,
        job_revision: 1,
        occurred_at: Timestamp::from_unix_ms(0),
        payload: EventPayload::JobRemoved,
        correlation: Default::default(),
    }
}

fn job() -> JobId {
    JobId::try_from("018f9c2a-525c-7b9a-986c-b0707def18bb").unwrap()
}

fn client_id() -> ClientId {
    ClientId::try_from("018f9c2a-0d55-74cc-b6c0-7cc8b1c9f221").unwrap()
}

fn connect(engine: &Engine) -> Result<PipeClient, ProtocolError> {
    PipeClient::connect(
        &engine.name,
        &secret(),
        fast_limits(),
        Duration::from_secs(5),
    )
}

/// Opens the pipe as a plain file, for tests that speak the wire by hand.
fn raw_open(name: &PipeName) -> File {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match OpenOptions::new()
            .read(true)
            .write(true)
            .open(name.as_str())
        {
            Ok(file) => return file,
            Err(error) if Instant::now() < deadline => {
                let _ = error;
                thread::sleep(Duration::from_millis(20));
            }
            Err(error) => panic!("could not open the pipe: {error}"),
        }
    }
}

fn raw_read<T: serde::de::DeserializeOwned>(file: &mut File) -> T {
    let body = read_frame(file).unwrap().expect("a frame");
    serde_json::from_slice(&body).unwrap()
}

/// Runs the client side of the handshake by hand and returns the nonces and
/// the proof it sent.
fn raw_handshake(file: &mut File, client_nonce: &Nonce) -> (Nonce, Vec<u8>) {
    write_frame(
        file,
        &Handshake::Hello {
            transport: auth::TRANSPORT.into(),
            version: auth::TRANSPORT_VERSION,
            client_nonce: client_nonce.to_hex(),
            intent: None,
            principal: None,
        },
    )
    .unwrap();
    let Handshake::Challenge {
        server_nonce,
        server_proof,
    } = raw_read(file)
    else {
        panic!("expected a challenge");
    };
    let server_nonce = Nonce::from_hex(&server_nonce).unwrap();
    assert!(secret().verify_server(
        client_nonce,
        &server_nonce,
        &auth::from_hex(&server_proof).unwrap()
    ));
    let proof = secret().client_proof(client_nonce, &server_nonce);
    (server_nonce, proof)
}

fn raw_authenticate(file: &mut File) {
    let nonce = Nonce::random().unwrap();
    let (_, proof) = raw_handshake(file, &nonce);
    write_frame(
        file,
        &Handshake::Proof {
            client_proof: auth::to_hex(&proof),
        },
    )
    .unwrap();
    assert_eq!(raw_read::<Handshake>(file), Handshake::Welcome);
}

#[test]
fn a_connection_declares_its_principal_in_the_handshake_and_a_bad_one_is_refused() {
    let engine = Engine::start(fast_limits(), Respond::Reply);
    let agent = Principal::try_from("agent:claude-code").unwrap();
    let client =
        PipeEngineClient::new(engine.name.clone(), secret(), fast_limits()).with_principal(agent);
    client.send(&client_id(), Command::EngineStatus).unwrap();
    assert!(matches!(engine.next(), Seen::Authenticated(p) if p == "agent:claude-code"));
    assert!(matches!(engine.next(), Seen::Command(_)));
    // A client that declares nothing is the person.
    let _person = connect(&engine).unwrap();
    assert!(matches!(engine.next(), Seen::Authenticated(p) if p == "user"));

    for declared in ["agent:Not Valid", "root", "agent:"] {
        let mut file = raw_open(&engine.name);
        write_frame(
            &mut file,
            &Handshake::Hello {
                transport: auth::TRANSPORT.into(),
                version: auth::TRANSPORT_VERSION,
                client_nonce: Nonce::random().unwrap().to_hex(),
                intent: None,
                principal: Some(declared.into()),
            },
        )
        .unwrap();
        assert!(
            matches!(engine.next_outcome(), Seen::AuthFailed(_)),
            "{declared}"
        );
    }
}

fn envelope(command: Command) -> CommandEnvelope {
    CommandEnvelope::new(client_id(), command)
}

#[test]
fn an_authenticated_client_sends_commands_and_follows_events() {
    let engine = Engine::start(fast_limits(), Respond::Reply);
    let client = PipeEngineClient::new(engine.name.clone(), secret(), fast_limits());
    let result = client
        .send(&client_id(), Command::GetJob { job_id: job() })
        .unwrap();
    assert_eq!(result, CommandResult::Removed { job_id: job() });
    // The same connection carries the next command.
    client.send(&client_id(), Command::EngineStatus).unwrap();

    let mut subscription = client
        .subscribe(&envelope(Command::SubscribeQueue { after_cursor: 100 }))
        .unwrap();
    assert!(matches!(
        subscription.start,
        CommandResult::Subscribed { .. }
    ));
    for seq in 1..=2 {
        match subscription
            .events
            .next_item(Duration::from_secs(5))
            .unwrap()
        {
            Some(StreamItem::Event(event)) => assert_eq!(event.seq, seq),
            other => panic!("{other:?}"),
        }
    }
    // A quiet period is not an error, and the stream stays usable.
    assert!(
        subscription
            .events
            .next_item(Duration::from_millis(100))
            .unwrap()
            .is_none()
    );
    assert!(
        subscription
            .events
            .next_item(Duration::from_millis(100))
            .unwrap()
            .is_none()
    );
}

/// How an access entry names this user in SDDL. Windows writes the machine's
/// built-in Administrator (RID 500, the account CI runners use) as `LA`
/// rather than its SID.
fn trustee(sid: &str) -> String {
    let alias = sid.starts_with("S-1-5-21-") && sid.ends_with("-500");
    format!(";{})", if alias { "LA" } else { sid })
}

#[test]
fn only_this_user_may_open_the_pipe() {
    let name = unique_name();
    let listener = PipeListener::bind(&name, secret(), fast_limits()).unwrap();
    let sddl = listener.security_sddl().unwrap();
    let sid = current_user_sid().unwrap();
    let dacl = sddl.split("D:").nth(1).unwrap().split("S:").next().unwrap();
    assert!(
        dacl.starts_with('P'),
        "the access list must not inherit: {sddl}"
    );
    assert_eq!(
        dacl.matches("(A;").count(),
        1,
        "exactly one allow entry: {sddl}"
    );
    assert!(
        dacl.contains(&trustee(&sid)),
        "the one entry is this user: {sddl}"
    );
    assert!(
        !dacl.contains(";WD)") && !dacl.contains(";AU)") && !dacl.contains(";BU)"),
        "{sddl}"
    );
}

#[test]
fn accepting_with_no_time_left_reports_nothing_rather_than_failing() {
    let name = unique_name();
    let mut listener = PipeListener::bind(&name, secret(), fast_limits()).unwrap();
    for _ in 0..20 {
        assert!(listener.accept(Some(Duration::ZERO)).unwrap().is_none());
    }
    assert!(
        listener
            .accept(Some(Duration::from_millis(20)))
            .unwrap()
            .is_none()
    );
    // A client that arrives afterwards is still accepted.
    let _file = raw_open(&name);
    assert!(
        listener
            .accept(Some(Duration::from_secs(5)))
            .unwrap()
            .is_some()
    );
}

#[test]
fn a_second_engine_cannot_claim_the_pipe_name() {
    let name = unique_name();
    let _first = PipeListener::bind(&name, secret(), fast_limits()).unwrap();
    let error = PipeListener::bind(&name, secret(), fast_limits())
        .err()
        .unwrap();
    assert_eq!(error.code.as_str(), "contract.engine_already_running");
}

#[test]
fn a_client_with_the_wrong_secret_learns_it_is_not_talking_to_its_engine() {
    let engine = Engine::start(fast_limits(), Respond::Reply);
    let wrong = EngineSecret::from_bytes([0x13; SECRET_BYTES]);
    let error = PipeClient::connect(&engine.name, &wrong, fast_limits(), Duration::from_secs(5))
        .err()
        .unwrap();
    assert_eq!(error.code.as_str(), "auth.peer_not_engine");
    // The engine never saw a proof and drops the connection.
    assert!(matches!(engine.next_outcome(), Seen::AuthFailed(_)));
    // It still serves a genuine client.
    let client = connect(&engine).unwrap();
    client
        .call(&envelope(Command::EngineStatus), Duration::from_secs(5))
        .unwrap();
}

#[test]
fn the_engine_refuses_a_wrong_proof_and_a_replayed_one() {
    let engine = Engine::start(fast_limits(), Respond::Reply);

    // A proof made without the secret.
    let mut file = raw_open(&engine.name);
    let _ = raw_handshake(&mut file, &Nonce::random().unwrap());
    write_frame(
        &mut file,
        &Handshake::Proof {
            client_proof: "00".repeat(32),
        },
    )
    .unwrap();
    assert!(
        matches!(engine.next_outcome(), Seen::AuthFailed(code) if code == "auth.handshake_failed")
    );
    assert!(
        read_frame(&mut file)
            .map(|frame| frame.is_none())
            .unwrap_or(true)
    );

    // A genuine exchange, recorded.
    let client_nonce = Nonce::random().unwrap();
    let mut first = raw_open(&engine.name);
    let (first_server_nonce, recorded_proof) = raw_handshake(&mut first, &client_nonce);
    write_frame(
        &mut first,
        &Handshake::Proof {
            client_proof: auth::to_hex(&recorded_proof),
        },
    )
    .unwrap();
    assert_eq!(raw_read::<Handshake>(&mut first), Handshake::Welcome);

    // The same hello and the recorded proof, replayed on a new connection.
    let mut replay = raw_open(&engine.name);
    let (second_server_nonce, _) = raw_handshake(&mut replay, &client_nonce);
    assert_ne!(
        first_server_nonce, second_server_nonce,
        "the server nonce must be fresh"
    );
    write_frame(
        &mut replay,
        &Handshake::Proof {
            client_proof: auth::to_hex(&recorded_proof),
        },
    )
    .unwrap();
    assert!(
        matches!(engine.next_outcome(), Seen::AuthFailed(code) if code == "auth.handshake_failed")
    );
}

#[test]
fn a_silent_connection_is_closed_at_the_handshake_deadline() {
    let engine = Engine::start(fast_limits(), Respond::Reply);
    let started = Instant::now();
    let _file = raw_open(&engine.name);
    match engine.next_outcome() {
        Seen::AuthFailed(code) => assert_eq!(code, "contract.connection_timed_out"),
        other => panic!("{other:?}"),
    }
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[test]
fn an_oversize_frame_is_refused_and_closes_only_that_connection() {
    let engine = Engine::start(fast_limits(), Respond::Reply);
    let other = connect(&engine).unwrap();

    let mut file = raw_open(&engine.name);
    raw_authenticate(&mut file);
    file.write_all(&(fetchpath_protocol::MAX_FRAME_BYTES as u32 + 1).to_le_bytes())
        .unwrap();
    let body = read_frame(&mut file).unwrap().unwrap();
    match decode_server_message(&body).unwrap() {
        ServerMessage::Reply(Reply {
            command_id: None,
            result: ReplyResult::Error(error),
            ..
        }) => assert_eq!(error.code.as_str(), "contract.message_too_large"),
        other => panic!("{other:?}"),
    }
    assert!(
        matches!(engine.next_outcome(), Seen::Ended(Some(code)) if code == "contract.message_too_large")
    );
    // The other connection is untouched.
    other
        .call(&envelope(Command::EngineStatus), Duration::from_secs(5))
        .unwrap();
}

#[test]
fn a_truncated_frame_ends_that_connection_and_the_engine_carries_on() {
    let engine = Engine::start(fast_limits(), Respond::Reply);
    {
        let mut file = raw_open(&engine.name);
        raw_authenticate(&mut file);
        file.write_all(&100_u32.to_le_bytes()).unwrap();
        file.write_all(b"{\"schema").unwrap();
    }
    assert!(
        matches!(engine.next_outcome(), Seen::Ended(Some(code)) if code == "contract.connection_lost")
    );

    // A frame that starts and then stalls is closed at the frame deadline.
    let mut stalled = raw_open(&engine.name);
    raw_authenticate(&mut stalled);
    stalled.write_all(&100_u32.to_le_bytes()).unwrap();
    assert!(
        matches!(engine.next_outcome(), Seen::Ended(Some(code)) if code == "contract.connection_timed_out")
    );

    connect(&engine)
        .unwrap()
        .call(&envelope(Command::EngineStatus), Duration::from_secs(5))
        .unwrap();
}

#[test]
fn an_idle_connection_is_closed_after_the_idle_limit() {
    let limits = Limits {
        idle_timeout: Some(Duration::from_millis(300)),
        ..fast_limits()
    };
    let engine = Engine::start(limits, Respond::Reply);
    let mut file = raw_open(&engine.name);
    raw_authenticate(&mut file);
    assert!(
        matches!(engine.next_outcome(), Seen::Ended(Some(code)) if code == "contract.connection_timed_out")
    );
}

#[test]
fn too_many_unanswered_commands_close_that_connection() {
    let limits = Limits {
        max_pending_requests: 2,
        ..fast_limits()
    };
    let engine = Engine::start(limits, Respond::Silent);
    let client =
        PipeClient::connect(&engine.name, &secret(), limits, Duration::from_secs(5)).unwrap();
    for _ in 0..3 {
        client.send(&envelope(Command::EngineStatus)).unwrap();
    }
    for _ in 0..2 {
        assert!(matches!(engine.next_outcome(), Seen::Command(_)));
    }
    assert!(
        matches!(engine.next_outcome(), Seen::Ended(Some(code)) if code == "resource.pending_limit")
    );
    match client.receive(Some(Duration::from_secs(5))).unwrap() {
        Some(ServerMessage::Reply(Reply {
            result: ReplyResult::Error(error),
            ..
        })) => assert_eq!(error.code.as_str(), "resource.pending_limit"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_flood_of_connections_is_capped_and_the_engine_recovers() {
    let limits = Limits {
        max_connections: 4,
        handshake_timeout: Duration::from_secs(3),
        ..fast_limits()
    };
    let engine = Engine::start(limits, Respond::Reply);
    let held: Vec<File> = (0..4).map(|_| raw_open(&engine.name)).collect();
    // Past the cap, a connection is closed at once, well before the
    // handshake deadline would have closed it.
    let started = Instant::now();
    let mut extra: Vec<File> = (0..8).map(|_| raw_open(&engine.name)).collect();
    for file in &mut extra {
        assert!(
            read_frame(file)
                .map(|frame| frame.is_none())
                .unwrap_or(true)
        );
    }
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "connections over the cap must be refused at once, not at the handshake deadline"
    );
    drop(extra);
    drop(held);
    // Only the four admitted connections ever reached the handshake.
    for _ in 0..4 {
        assert!(matches!(engine.next_outcome(), Seen::AuthFailed(_)));
    }
    assert!(
        engine
            .seen
            .recv_timeout(Duration::from_millis(500))
            .is_err(),
        "a connection over the cap reached the handshake"
    );
    connect(&engine)
        .unwrap()
        .call(&envelope(Command::EngineStatus), Duration::from_secs(5))
        .unwrap();
}

#[test]
fn a_lost_connection_is_reopened_and_the_same_command_resent() {
    let engine = Engine::start(fast_limits(), Respond::DropFirst);
    let client = PipeEngineClient::new(engine.name.clone(), secret(), fast_limits());
    let command = envelope(Command::GetJob { job_id: job() });
    assert_eq!(
        client.execute(&command).unwrap(),
        CommandResult::Removed { job_id: job() }
    );
    let mut ids = Vec::new();
    while ids.len() < 2 {
        if let Seen::Command(seen) = engine.next() {
            ids.push(seen.command_id);
        }
    }
    assert_eq!(ids[0], command.command_id);
    assert_eq!(
        ids[1], command.command_id,
        "the retry must reuse the command id"
    );
}

#[test]
fn a_client_reports_a_missing_engine_plainly() {
    let error = PipeClient::connect(
        &unique_name(),
        &secret(),
        fast_limits(),
        Duration::from_millis(200),
    )
    .err()
    .unwrap();
    assert_eq!(error.code.as_str(), "contract.engine_unavailable");
}

#[test]
fn the_secret_file_is_created_once_private_and_checked_on_load() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nested").join("engine-secret-v1.bin");
    let missing = EngineSecret::load(&path).unwrap_err();
    assert_eq!(missing.code.as_str(), "auth.engine_secret_missing");

    let sid = current_user_sid().unwrap();
    let created = EngineSecret::load_or_create(&path, &sid).unwrap();
    let again = EngineSecret::load_or_create(&path, &sid).unwrap();
    let loaded = EngineSecret::load(&path).unwrap();
    let (client, server) = (Nonce([1; 32]), Nonce([2; 32]));
    let proof = created.client_proof(&client, &server);
    assert!(again.verify_client(&client, &server, &proof));
    assert!(loaded.verify_client(&client, &server, &proof));

    let sddl = file_security_sddl(&path).unwrap();
    let dacl = sddl.split("D:").nth(1).unwrap().split("S:").next().unwrap();
    assert!(dacl.starts_with('P'), "{sddl}");
    assert_eq!(dacl.matches("(A;").count(), 1, "{sddl}");
    assert!(dacl.contains(&trustee(&sid)), "{sddl}");
    let label = sddl.split("S:").nth(1).expect("an integrity label");
    assert!(
        label.contains("ML;") && label.contains("NR") && label.contains(";ME)"),
        "{sddl}"
    );

    std::fs::write(&path, [0_u8; 31]).unwrap();
    assert_eq!(
        EngineSecret::load(&path).unwrap_err().code.as_str(),
        "auth.engine_secret_invalid"
    );
    std::fs::write(&path, [0_u8; 33]).unwrap();
    assert_eq!(
        EngineSecret::load(&path).unwrap_err().code.as_str(),
        "auth.engine_secret_invalid"
    );
}

const CHILD_NAME: &str = "FETCHPATH_PIPE_TEST_CHILD_NAME";

#[test]
fn a_client_killed_mid_frame_ends_only_its_own_connection() {
    let engine = Engine::start(fast_limits(), Respond::Reply);
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["killed_client_child", "--exact", "--ignored", "--nocapture"])
        .env(CHILD_NAME, engine.name.as_str())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut lines = std::io::BufReader::new(child.stdout.take().unwrap()).lines();
    let ready = lines.find_map(|line| line.ok().filter(|text| text.contains("child-ready")));
    assert!(ready.is_some(), "the child never got mid-frame");
    child.kill().unwrap();
    let _ = child.wait();
    assert!(
        matches!(engine.next_outcome(), Seen::Ended(Some(code)) if code == "contract.connection_lost")
    );
    connect(&engine)
        .unwrap()
        .call(&envelope(Command::EngineStatus), Duration::from_secs(5))
        .unwrap();
}

/// Run only by the test above, in a separate process that is then killed.
#[test]
#[ignore = "helper process for a_client_killed_mid_frame_ends_only_its_own_connection"]
fn killed_client_child() {
    let Some(name) = std::env::var_os(CHILD_NAME) else {
        return;
    };
    let name = PipeName::named(name.to_str().unwrap().strip_prefix(r"\\.\pipe\").unwrap());
    let mut file = raw_open(&name);
    raw_authenticate(&mut file);
    file.write_all(&1_000_u32.to_le_bytes()).unwrap();
    file.write_all(b"{\"schema_version\":1,").unwrap();
    file.flush().unwrap();
    println!("child-ready");
    std::io::stdout().flush().unwrap();
    thread::sleep(Duration::from_secs(60));
}

#[test]
fn a_client_that_leaves_before_it_is_accepted_does_not_wedge_the_listener() {
    let name = unique_name();
    let mut listener = PipeListener::bind(&name, secret(), fast_limits()).unwrap();
    // Opens the listening instance and closes it before accept waits on it.
    drop(raw_open(&name));
    let arriving = {
        let name = name.clone();
        thread::spawn(move || {
            let mut file = raw_open(&name);
            raw_authenticate(&mut file);
        })
    };
    let deadline = Instant::now() + Duration::from_secs(5);
    let pending = loop {
        if let Some(pending) = listener.accept(Some(Duration::from_millis(200))).unwrap() {
            break pending;
        }
        assert!(
            Instant::now() < deadline,
            "the listener never accepted again"
        );
    };
    // The departed client may be the one accepted first; keep going until a
    // live one authenticates.
    let mut outcome = pending.authenticate();
    while outcome.is_err() {
        let next = listener
            .accept(Some(Duration::from_secs(5)))
            .unwrap()
            .expect("a client");
        outcome = next.authenticate();
    }
    arriving.join().unwrap();
}

#[test]
fn a_subscription_outlives_the_idle_limit() {
    let limits = Limits {
        idle_timeout: Some(Duration::from_millis(300)),
        ..fast_limits()
    };
    let engine = Engine::start(limits, Respond::Reply);
    let client = PipeEngineClient::new(engine.name.clone(), secret(), limits);
    let mut subscription = client
        .subscribe(&envelope(Command::SubscribeQueue { after_cursor: 100 }))
        .unwrap();
    for _ in 0..2 {
        assert!(
            subscription
                .events
                .next_item(Duration::from_secs(5))
                .unwrap()
                .is_some()
        );
    }
    thread::sleep(Duration::from_millis(900));
    assert!(
        subscription
            .events
            .next_item(Duration::from_millis(100))
            .unwrap()
            .is_none()
    );
    while let Ok(seen) = engine.seen.try_recv() {
        assert!(
            !matches!(seen, Seen::Ended(_)),
            "the subscription was closed: {seen:?}"
        );
    }
}

#[test]
fn each_engine_run_gets_an_unpredictable_name_published_privately() {
    let first = PipeName::fresh().unwrap();
    let second = PipeName::fresh().unwrap();
    assert_ne!(
        first, second,
        "a name seen in one run must not return in the next"
    );
    assert!(first.as_str().starts_with(ENGINE_PIPE_PREFIX));
    assert!(first.as_str().contains(&current_user_sid().unwrap()));

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("data").join("engine-endpoint-v1");
    assert_eq!(
        endpoint::read(&path).unwrap_err().code.as_str(),
        "contract.engine_unavailable"
    );
    let sid = current_user_sid().unwrap();
    endpoint::publish(&path, &first, &sid).unwrap();
    assert_eq!(endpoint::read(&path).unwrap(), first);
    // A later run replaces the record, keeping its protection.
    endpoint::publish(&path, &second, &sid).unwrap();
    assert_eq!(endpoint::read(&path).unwrap(), second);
    let sddl = file_security_sddl(&path).unwrap();
    let dacl = sddl.split("D:").nth(1).unwrap().split("S:").next().unwrap();
    assert!(
        dacl.starts_with('P') && dacl.matches("(A;").count() == 1 && dacl.contains(&trustee(&sid)),
        "{sddl}"
    );
    assert!(
        sddl.split("S:")
            .nth(1)
            .is_some_and(|label| label.contains("NR") && label.contains(";ME)")),
        "{sddl}"
    );

    // A damaged record is refused rather than followed.
    std::fs::write(&path, r"\\.\pipe\somewhere-else").unwrap();
    assert_eq!(
        endpoint::read(&path).unwrap_err().code.as_str(),
        "contract.engine_unavailable"
    );

    // A client that follows the record reaches the engine that wrote it.
    let name = PipeName::fresh().unwrap();
    let mut listener = PipeListener::bind(&name, secret(), fast_limits()).unwrap();
    endpoint::publish(&path, &name, &sid).unwrap();
    let serving = thread::spawn(move || {
        listener
            .accept(Some(Duration::from_secs(5)))
            .unwrap()
            .expect("a client")
            .authenticate()
            .map(|_| ())
    });
    PipeClient::connect(
        &endpoint::read(&path).unwrap(),
        &secret(),
        fast_limits(),
        Duration::from_secs(5),
    )
    .unwrap();
    serving.join().unwrap().unwrap();
}

const LOW_NAME: &str = "FETCHPATH_PIPE_TEST_LOW_NAME";
const LOW_SECRET: &str = "FETCHPATH_PIPE_TEST_LOW_SECRET";
const LOW_ENDPOINT: &str = "FETCHPATH_PIPE_TEST_LOW_ENDPOINT";

#[test]
fn a_lower_integrity_process_cannot_open_the_pipe_or_read_the_secret() {
    let dir = tempfile::tempdir().unwrap();
    let secret_path = dir.path().join("engine-secret-v1.bin");
    let sid = current_user_sid().unwrap();
    let _ = EngineSecret::load_or_create(&secret_path, &sid).unwrap();
    let engine = Engine::start(fast_limits(), Respond::Reply);
    let endpoint_path = dir.path().join("engine-endpoint-v1");
    endpoint::publish(&endpoint_path, &engine.name, &sid).unwrap();

    // A copy of this test binary marked low integrity runs at low integrity.
    let low = dir.path().join("low-integrity-child.exe");
    std::fs::copy(std::env::current_exe().unwrap(), &low).unwrap();
    let marked = std::process::Command::new("icacls")
        .arg(&low)
        .args(["/setintegritylevel", "low"])
        .stdout(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(
        marked.success(),
        "icacls could not mark the copy low integrity"
    );
    let output = std::process::Command::new(&low)
        .args(["low_integrity_child", "--exact", "--ignored", "--nocapture"])
        .env(LOW_NAME, engine.name.as_str())
        .env(LOW_SECRET, &secret_path)
        .env(LOW_ENDPOINT, &endpoint_path)
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&output.stdout);
    let report = text
        .lines()
        .find(|line| line.starts_with("low-report"))
        .unwrap_or_else(|| panic!("the low-integrity child did not report: {text}"));
    assert_eq!(
        report, "low-report rw=denied ro=denied secret=denied endpoint=denied",
        "a lower-integrity process reached the engine"
    );
    // Nothing it tried took a connection slot.
    assert!(
        engine
            .seen
            .recv_timeout(Duration::from_millis(300))
            .is_err()
    );
}

/// Run only by the test above, as a low-integrity process.
#[test]
#[ignore = "helper process for a_lower_integrity_process_cannot_open_the_pipe_or_read_the_secret"]
fn low_integrity_child() {
    let (Some(name), Some(secret_path), Some(endpoint_path)) = (
        std::env::var_os(LOW_NAME),
        std::env::var_os(LOW_SECRET),
        std::env::var_os(LOW_ENDPOINT),
    ) else {
        return;
    };
    // Only "access denied" counts as refused, so a wrong path cannot pass.
    let outcome = |result: std::io::Result<File>| match result {
        Ok(_) => "opened".to_owned(),
        Err(error) if error.raw_os_error() == Some(5) => "denied".to_owned(),
        Err(error) => format!("error-{error}"),
    };
    let rw = outcome(OpenOptions::new().read(true).write(true).open(&name));
    let ro = outcome(OpenOptions::new().read(true).open(&name));
    let secret = outcome(File::open(&secret_path));
    let endpoint = outcome(File::open(&endpoint_path));
    println!("low-report rw={rw} ro={ro} secret={secret} endpoint={endpoint}");
}
