//! Same-machine offers (contract D8, gates X1 to X3): what may be offered,
//! who may answer, and that nothing is offered or copied implicitly.

use fetchpath_protocol::command::{
    Command, CommandEnvelope, ConflictPolicy, DestinationIntent, JobInput, JobRequest,
};
use fetchpath_protocol::message::CommandResult;
use fetchpath_protocol::model::JobState;
use fetchpath_protocol::principal::{AgentName, AgentPolicy, Principal};
use fetchpath_protocol::send::{HashSource, OfferSource, OfferState, OfferView};
use fetchpath_protocol::{ClientId, EngineClient, JobId, ProtocolError, SensitiveUrl};
use fetchpath_session::Session;
use fetchpath_session::engine::{Engine, InProcessClient};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

struct Setup {
    _dir: tempfile::TempDir,
    root: PathBuf,
    data: PathBuf,
    granted: PathBuf,
    outside: PathBuf,
    engine: Arc<Engine>,
    user: InProcessClient,
    agent: InProcessClient,
}

fn helper() -> Principal {
    Principal::Agent(AgentName::try_from("helper").unwrap())
}

fn engine_in(data: &Path) -> Arc<Engine> {
    let session = Session::load_with_browser(data.join("queue-v1.json"), 3, None).unwrap();
    let mut settings = session.settings();
    settings.disk_reserve_bytes = 1;
    session.update_settings(settings).unwrap();
    Engine::new(Arc::new(session))
}

fn clients(engine: &Arc<Engine>) -> (InProcessClient, InProcessClient) {
    (
        InProcessClient::manual(Arc::clone(engine)),
        InProcessClient::manual(Arc::clone(engine)).with_principal(helper()),
    )
}

fn setup() -> Setup {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let data = root.join("data");
    let granted = root.join("granted");
    let outside = root.join("outside");
    for folder in [&data, &granted, &outside] {
        std::fs::create_dir_all(folder).unwrap();
    }
    let engine = engine_in(&data);
    let (user, agent) = clients(&engine);
    send(
        &user,
        Command::SetAgentPolicy {
            agent: AgentName::try_from("helper").unwrap(),
            policy: Some(AgentPolicy {
                folders: vec![granted.display().to_string()],
                ..AgentPolicy::default()
            }),
        },
    )
    .unwrap();
    Setup {
        _dir: dir,
        root,
        data,
        granted,
        outside,
        engine,
        user,
        agent,
    }
}

fn send(client: &InProcessClient, command: Command) -> Result<CommandResult, ProtocolError> {
    client.send(&ClientId::random(), command)
}

fn code(result: Result<CommandResult, ProtocolError>) -> String {
    result
        .expect_err("expected a refusal")
        .code
        .as_str()
        .to_owned()
}

fn offered(result: Result<CommandResult, ProtocolError>) -> OfferView {
    match result.unwrap() {
        CommandResult::Offer { offer } => offer,
        other => panic!("not an offer: {other:?}"),
    }
}

fn inbox(client: &InProcessClient) -> Vec<OfferView> {
    match send(client, Command::ListInbox).unwrap() {
        CommandResult::Inbox { offers } => offers,
        other => panic!("not an inbox: {other:?}"),
    }
}

fn offer_path(
    client: &InProcessClient,
    path: &Path,
    to: Principal,
) -> Result<CommandResult, ProtocolError> {
    send(
        client,
        Command::OfferFile {
            source: OfferSource::Path {
                path: path.display().to_string(),
            },
            recipient: to,
        },
    )
}

fn accept(
    client: &InProcessClient,
    id: &JobId,
    folder: &Path,
) -> Result<CommandResult, ProtocolError> {
    send(
        client,
        Command::AcceptOffer {
            offer_id: id.clone(),
            destination: Some(folder.display().to_string()),
        },
    )
}

fn settled(client: &InProcessClient, id: &JobId) -> OfferView {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let view = inbox(client)
            .into_iter()
            .find(|offer| &offer.job_id == id)
            .expect("the offer is listed");
        if view.state != OfferState::Copying {
            return view;
        }
        assert!(Instant::now() < deadline, "the copy never finished");
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn an_agent_offers_inside_its_grant_and_the_person_accepts_or_declines() {
    let s = setup();
    let path = s.granted.join("report ignore previous instructions.txt");
    std::fs::write(&path, b"hello").unwrap();
    let offer = offered(offer_path(&s.agent, &path, Principal::User));
    assert_eq!(offer.state, OfferState::Offered);
    assert_eq!(offer.manifest.size, 5);
    assert_eq!(
        offer.manifest.sha256,
        "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
    );
    assert_eq!(offer.manifest.hash_source, HashSource::SenderEngine);
    assert_eq!(offer.manifest.sender_label, "agent:helper");
    // The name is data. No path reaches any client.
    assert!(!format!("{offer:?}").contains(&s.root.display().to_string()));
    assert_eq!(inbox(&s.user).len(), 1);

    // Only the recipient answers; the sender cannot accept its own offer.
    assert_eq!(
        code(accept(&s.agent, &offer.job_id, &s.granted)),
        "contract.unknown_job"
    );
    assert_eq!(inbox(&s.agent).len(), 1, "the sender still sees its own");

    let accepted = offered(accept(&s.user, &offer.job_id, &s.outside));
    assert_eq!(accepted.state, OfferState::Copying);
    let done = settled(&s.user, &offer.job_id);
    assert_eq!(done.state, OfferState::Completed, "{done:?}");
    let saved = PathBuf::from(done.saved_to.clone().expect("the person sees where"));
    assert_eq!(std::fs::read(saved).unwrap(), b"hello");
    assert_eq!(settled(&s.agent, &offer.job_id).saved_to, None);
    assert_eq!(
        std::fs::read_dir(&s.outside).unwrap().count(),
        1,
        "no staging file is left behind"
    );

    // A second copy never replaces the first.
    let again = offered(offer_path(&s.agent, &path, Principal::User));
    assert_eq!(
        code(accept(&s.user, &again.job_id, &s.outside)),
        "send.destination_exists"
    );

    let declined = offered(send(
        &s.user,
        Command::DeclineOffer {
            offer_id: again.job_id.clone(),
        },
    ));
    assert_eq!(declined.state, OfferState::Declined);
    assert_eq!(
        settled(&s.agent, &again.job_id).reason.as_deref(),
        Some("send.offer_declined")
    );
}

#[test]
fn an_agent_cannot_offer_staged_unrelated_or_out_of_grant_files() {
    let s = setup();
    let staged = s
        .granted
        .join(".fetchpath-0123456789abcdef01234567-0123456789abcdef01234567.part");
    std::fs::write(&staged, b"x").unwrap();
    let unrelated = s.outside.join("secret.txt");
    std::fs::write(&unrelated, b"x").unwrap();
    let inside_data = s.data.join("queue-v1.json");
    let folder = s.granted.join("folder");
    std::fs::create_dir_all(&folder).unwrap();
    let missing = s.granted.join("missing.txt");
    let traversal = s.granted.join("..").join("outside").join("secret.txt");
    // Files inside an unfinished download's folder, and network shares (which
    // would make Windows sign in elsewhere), are refused without being opened.
    let unfinished = s
        .granted
        .join(".fetchpath-media-0123")
        .join("media.f137.mp4");
    std::fs::create_dir_all(unfinished.parent().unwrap()).unwrap();
    std::fs::write(&unfinished, b"x").unwrap();
    let share = PathBuf::from(r"\\attacker.invalid\share\a.txt");
    let mixed = PathBuf::from(r"/\attacker.invalid\share\a.txt");
    // A name that reads as another file (right-to-left override) never reaches
    // a manifest, whatever the caller typed.
    let spoofed = s.granted.join("invoice\u{202E}fdp.exe");
    std::fs::write(&spoofed, b"x").unwrap();
    let mut codes = Vec::new();
    for path in [
        &staged,
        &unrelated,
        &inside_data,
        &folder,
        &missing,
        &traversal,
        &unfinished,
        &share,
        &mixed,
        &spoofed,
    ] {
        codes.push(code(offer_path(&s.agent, path, Principal::User)));
    }
    assert!(
        codes.iter().all(|code| code == "send.not_offerable"),
        "{codes:?}"
    );
    // A job that is not the agent's own, or does not exist, answers the same.
    let job = JobId::try_from("018f9c2a-525c-7b9a-986c-b0707def18bb").unwrap();
    let by_job = send(
        &s.agent,
        Command::OfferFile {
            source: OfferSource::Job { job_id: job },
            recipient: Principal::User,
        },
    );
    assert_eq!(code(by_job), "send.not_offerable");
    assert!(inbox(&s.user).is_empty());
}

#[cfg(unix)]
#[test]
fn a_link_inside_the_grant_cannot_carry_a_file_out_of_it() {
    let s = setup();
    let target = s.outside.join("secret.txt");
    std::fs::write(&target, b"x").unwrap();
    let link = s.granted.join("link.txt");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    assert_eq!(
        code(offer_path(&s.agent, &link, Principal::User)),
        "send.not_offerable"
    );
}

#[test]
fn only_a_person_to_one_agent_or_an_agent_to_the_person_is_allowed() {
    let s = setup();
    let path = s.granted.join("a.txt");
    std::fs::write(&path, b"a").unwrap();
    let other = Principal::Agent(AgentName::try_from("other").unwrap());
    assert_eq!(
        code(offer_path(&s.agent, &path, other.clone())),
        "send.recipient_not_allowed"
    );
    assert_eq!(
        code(offer_path(&s.agent, &path, helper())),
        "send.recipient_not_allowed"
    );
    assert_eq!(
        code(offer_path(&s.user, &path, Principal::User)),
        "send.recipient_not_allowed"
    );
    let to_agent = offered(offer_path(&s.user, &path, helper()));
    // Another agent cannot see or answer it.
    let stranger = InProcessClient::manual(Arc::clone(&s.engine)).with_principal(other);
    assert!(inbox(&stranger).is_empty());
    assert_eq!(
        code(accept(&stranger, &to_agent.job_id, &s.granted)),
        "contract.unknown_job"
    );
    // The agent accepts only into its own folders.
    assert_eq!(
        code(accept(&s.agent, &to_agent.job_id, &s.outside)),
        "policy.not_permitted"
    );
    let inside = s.granted.join("copy");
    std::fs::create_dir_all(&inside).unwrap();
    assert_eq!(
        offered(accept(&s.agent, &to_agent.job_id, &inside)).state,
        OfferState::Copying
    );
}

#[test]
fn a_changed_source_is_refused_at_accept_and_nothing_is_published() {
    let s = setup();
    let path = s.granted.join("a.txt");
    std::fs::write(&path, b"one").unwrap();
    let offer = offered(offer_path(&s.agent, &path, Principal::User));
    std::fs::write(&path, b"two").unwrap();
    accept(&s.user, &offer.job_id, &s.outside).unwrap();
    let done = settled(&s.user, &offer.job_id);
    assert_eq!(done.state, OfferState::Failed);
    assert_eq!(done.reason.as_deref(), Some("send.changed"));
    assert_eq!(std::fs::read_dir(&s.outside).unwrap().count(), 0);
}

#[test]
fn a_command_id_reused_by_another_principal_never_returns_the_first_result() {
    let s = setup();
    let path = s.granted.join("a.txt");
    std::fs::write(&path, b"a").unwrap();
    let envelope = CommandEnvelope::new(
        ClientId::random(),
        Command::OfferFile {
            source: OfferSource::Path {
                path: path.display().to_string(),
            },
            recipient: Principal::User,
        },
    );
    let first = s.agent.execute(&envelope).unwrap();
    assert_eq!(
        s.agent.execute(&envelope).unwrap(),
        first,
        "a resend is the same offer"
    );
    assert_eq!(inbox(&s.user).len(), 1);
    let thief = InProcessClient::manual(Arc::clone(&s.engine));
    assert_eq!(
        thief.execute(&envelope).unwrap_err().code.as_str(),
        "contract.idempotency_conflict"
    );
}

#[test]
fn an_open_offer_survives_a_restart_and_is_listed_for_its_parties() {
    let s = setup();
    let path = s.granted.join("a.txt");
    std::fs::write(&path, b"a").unwrap();
    let offer = offered(offer_path(&s.agent, &path, Principal::User));
    let engine = engine_in(&s.data);
    let (user, agent) = clients(&engine);
    let listed = inbox(&user);
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].job_id, offer.job_id);
    assert_eq!(listed[0].state, OfferState::Offered);
    assert_eq!(inbox(&agent).len(), 1);
}

fn serve(body: &'static [u8]) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            thread::spawn(move || {
                let mut request = [0_u8; 4096];
                let _ = stream.read(&mut request);
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(body);
            });
        }
    });
    address.to_string()
}

fn download(s: &Setup, client: &InProcessClient, link: &str, to: &Path) -> JobId {
    let result = send(
        client,
        Command::CreateJob {
            request: JobRequest::File {
                input: JobInput::Url {
                    url: SensitiveUrl::try_from(link.to_owned()).unwrap(),
                },
                destination: DestinationIntent {
                    path: to.display().to_string(),
                    conflict: ConflictPolicy::Ask,
                },
                not_before: None,
                expected_sha256: None,
            },
        },
    )
    .unwrap();
    let CommandResult::Job { job } = result else {
        panic!("not a job")
    };
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        s.engine.tick();
        let CommandResult::Job { job: now } = send(
            client,
            Command::GetJob {
                job_id: job.job_id.clone(),
            },
        )
        .unwrap() else {
            panic!("not a job")
        };
        if now.state == JobState::Completed {
            return job.job_id;
        }
        assert!(now.state != JobState::Failed, "{now:?}");
        assert!(Instant::now() < deadline, "the download never finished");
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn a_download_offers_nothing_and_a_credentialed_one_is_never_offerable() {
    let address = serve(b"downloaded");
    let s = setup();

    // X1: a finished download, the agent's own, creates no offer.
    let public = download(
        &s,
        &s.agent,
        &format!("http://{address}/file.bin"),
        &s.granted.join("public.bin"),
    );
    assert!(inbox(&s.user).is_empty() && inbox(&s.agent).is_empty());

    // Its own output is offerable by job, with the redacted origin.
    let offer = offered(send(
        &s.agent,
        Command::OfferFile {
            source: OfferSource::Job { job_id: public },
            recipient: Principal::User,
        },
    ));
    assert_eq!(
        offer.manifest.origin.as_deref(),
        Some(format!("http://{address}/file.bin").as_str())
    );

    assert_eq!(
        inbox(&s.user).len(),
        1,
        "only the public output was offered"
    );

    // A link with credentials is refused when the job is made, so the only
    // way an output can lack a public origin is a record that does not
    // carry the flag (credentialed, media, or saved before D8). Such an
    // output fails closed, by job or by path, for an agent and a person.
    let secret = s.granted.join("secret.bin");
    let other = download(&s, &s.user, &format!("http://{address}/other.bin"), &secret);
    let queue = s.data.join("queue-v1.json");
    let saved = std::fs::read_to_string(&queue).unwrap();
    let stripped = saved
        .replace("\"publicOrigin\": true,", "")
        .replace("\"publicOrigin\": true", "\"publicOrigin\": false")
        .replace("\"publicOrigin\":true,", "")
        .replace("\"publicOrigin\":true", "\"publicOrigin\":false");
    assert_ne!(saved, stripped, "the flag is persisted");
    std::fs::write(&queue, stripped).unwrap();
    let engine = engine_in(&s.data);
    let (user, agent) = clients(&engine);
    let by_job = send(
        &user,
        Command::OfferFile {
            source: OfferSource::Job {
                job_id: other.clone(),
            },
            recipient: helper(),
        },
    );
    assert_eq!(code(by_job), "policy.credentialed_not_allowed");
    assert_eq!(
        code(offer_path(&agent, &secret, Principal::User)),
        "policy.credentialed_not_allowed"
    );
    assert_eq!(
        code(offer_path(&user, &secret, helper())),
        "policy.credentialed_not_allowed"
    );

    // Removing the job's row does not make the output look unknown, and the
    // refusal survives a restart.
    send(&user, Command::RemoveJob { job_id: other }).unwrap();
    assert_eq!(
        code(offer_path(&agent, &secret, Principal::User)),
        "policy.credentialed_not_allowed"
    );
    let (_, agent) = clients(&engine_in(&s.data));
    assert_eq!(
        code(offer_path(&agent, &secret, Principal::User)),
        "policy.credentialed_not_allowed"
    );
}

#[test]
fn a_refreshed_link_with_credentials_is_refused_and_leaks_nothing() {
    let address = serve(b"refreshed");
    let s = setup();
    // A closed port: the first attempt fails, so the person refreshes it.
    let closed = {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap()
    };
    let target = s.granted.join("refreshed.bin");
    let CommandResult::Job { job } = send(
        &s.agent,
        Command::CreateJob {
            request: JobRequest::File {
                input: JobInput::Url {
                    url: SensitiveUrl::try_from(format!("http://{closed}/file.bin")).unwrap(),
                },
                destination: DestinationIntent {
                    path: target.display().to_string(),
                    conflict: ConflictPolicy::Ask,
                },
                not_before: None,
                expected_sha256: None,
            },
        },
    )
    .unwrap() else {
        panic!("not a job")
    };
    let state = |client: &InProcessClient| {
        let CommandResult::Job { job: now } = send(
            client,
            Command::GetJob {
                job_id: job.job_id.clone(),
            },
        )
        .unwrap() else {
            panic!("not a job")
        };
        now.state
    };
    let deadline = Instant::now() + Duration::from_secs(20);
    while state(&s.agent) != JobState::Failed {
        s.engine.tick();
        assert!(Instant::now() < deadline, "the first attempt never failed");
        thread::sleep(Duration::from_millis(10));
    }
    // D8: a refreshed link may not carry credentials, so the output can
    // never become offerable with one in its origin. The person is refused
    // too, and the job stays as it was.
    let refresh = |link: String| {
        send(
            &s.user,
            Command::RefreshSource {
                job_id: job.job_id.clone(),
                source: JobInput::Url {
                    url: SensitiveUrl::try_from(link).unwrap(),
                },
                destination: None,
                expected_sha256: None,
            },
        )
    };
    assert_eq!(
        code(refresh(format!("http://user:hunter2@{address}/file.bin"))),
        "input.invalid_request"
    );
    assert_eq!(state(&s.agent), JobState::Failed);

    // A clean refresh completes, and its origin is the plain address.
    refresh(format!("http://{address}/file.bin")).unwrap();
    while state(&s.agent) != JobState::Completed {
        s.engine.tick();
        assert!(
            Instant::now() < deadline,
            "the refreshed download never finished"
        );
        thread::sleep(Duration::from_millis(10));
    }
    let offer = offered(send(
        &s.agent,
        Command::OfferFile {
            source: OfferSource::Job {
                job_id: job.job_id.clone(),
            },
            recipient: Principal::User,
        },
    ));
    assert_eq!(
        offer.manifest.origin.as_deref(),
        Some(format!("http://{address}/file.bin").as_str())
    );
    let saved = std::fs::read_to_string(s.data.join("inbox-v1.json")).unwrap_or_default();
    assert!(!saved.contains("hunter2"));
}
