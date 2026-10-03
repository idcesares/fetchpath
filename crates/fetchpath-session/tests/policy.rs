//! Principals and agent policy (contract D1): what an agent may not do, how
//! a request outside its policy waits for the person, and how the person's
//! decision ends the wait. Each test tries to get something past the policy.

use fetchpath_protocol::command::{
    Command, CommandEnvelope, ConflictPolicy, DestinationDecision, DestinationIntent, JobFilter,
    JobInput, JobRequest,
};
use fetchpath_protocol::message::CommandResult;
use fetchpath_protocol::model::{EngineSettings, JobState, Theme};
use fetchpath_protocol::principal::{AgentName, AgentPolicy, ApprovalReason, Principal};
use fetchpath_protocol::{
    Action, ClientId, CredentialRef, EngineClient, JobId, JobSnapshot, ProtocolError, SensitiveUrl,
    Timestamp,
};
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
    granted: PathBuf,
    outside: PathBuf,
    queue: PathBuf,
    engine: Arc<Engine>,
    user: InProcessClient,
    agent: InProcessClient,
}

fn agent_name() -> AgentName {
    AgentName::try_from("helper").unwrap()
}

fn helper() -> Principal {
    Principal::Agent(agent_name())
}

fn open(queue: &Path) -> Arc<Engine> {
    Engine::new(Arc::new(
        Session::load_with_browser(queue.to_path_buf(), 3, None).unwrap(),
    ))
}

fn clients(engine: &Arc<Engine>) -> (InProcessClient, InProcessClient) {
    (
        InProcessClient::manual(Arc::clone(engine)),
        InProcessClient::manual(Arc::clone(engine)).with_principal(helper()),
    )
}

/// A queue, a folder granted to the agent `helper`, and one that is not.
fn setup(policy: impl FnOnce(&Path) -> AgentPolicy) -> Setup {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let granted = root.join("granted");
    let outside = root.join("granted-not");
    std::fs::create_dir_all(&granted).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    let queue = root.join("queue-v1.json");
    let engine = open(&queue);
    let (user, agent) = clients(&engine);
    send(
        &user,
        Command::SetAgentPolicy {
            agent: agent_name(),
            policy: Some(policy(&granted)),
        },
    )
    .unwrap();
    Setup {
        _dir: dir,
        root,
        granted,
        outside,
        queue,
        engine,
        user,
        agent,
    }
}

fn granting(folder: &Path) -> AgentPolicy {
    AgentPolicy {
        folders: vec![folder.display().to_string()],
        ..AgentPolicy::default()
    }
}

fn send(client: &InProcessClient, command: Command) -> Result<CommandResult, ProtocolError> {
    client.send(&ClientId::random(), command)
}

fn url(text: &str) -> SensitiveUrl {
    SensitiveUrl::try_from(text.to_owned()).unwrap()
}

fn file(link: &str, destination: &Path) -> Command {
    Command::CreateJob {
        request: JobRequest::File {
            input: JobInput::Url { url: url(link) },
            destination: DestinationIntent {
                path: destination.display().to_string(),
                conflict: ConflictPolicy::Ask,
            },
            not_before: None,
            expected_sha256: None,
        },
    }
}

/// A job that will not start for an hour, so nothing touches the network.
fn later(destination: &Path) -> Command {
    let mut command = file("http://127.0.0.1:9/file.bin", destination);
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

fn job(result: CommandResult) -> JobSnapshot {
    match result {
        CommandResult::Job { job } | CommandResult::Control { job, .. } => job,
        other => panic!("not a job: {other:?}"),
    }
}

fn get(client: &InProcessClient, job_id: &JobId) -> JobSnapshot {
    job(send(
        client,
        Command::GetJob {
            job_id: job_id.clone(),
        },
    )
    .unwrap())
}

fn list(client: &InProcessClient, filter: JobFilter) -> Vec<JobSnapshot> {
    match send(client, Command::ListJobs { filter }).unwrap() {
        CommandResult::Jobs { jobs } => jobs,
        other => panic!("{other:?}"),
    }
}

fn code(result: Result<CommandResult, ProtocolError>) -> String {
    result
        .expect_err("expected a refusal")
        .code
        .as_str()
        .to_owned()
}

fn reasons(job: &JobSnapshot) -> Vec<ApprovalReason> {
    job.approval.clone().expect("awaiting approval").reasons
}

#[test]
fn inside_its_grant_an_agent_job_queues_and_outside_it_waits_for_the_person() {
    let s = setup(granting);
    let inside = job(send(&s.agent, later(&s.granted.join("a.bin"))).unwrap());
    assert_eq!(inside.state, JobState::Queued);
    assert_eq!(inside.principal, helper());
    assert_eq!(inside.approval, None);

    let nested = job(send(&s.agent, later(&s.granted.join("deeper/still/b.bin"))).unwrap());
    assert_eq!(
        nested.state,
        JobState::Queued,
        "a missing subfolder is inside"
    );

    // A sibling whose name starts with the grant's is not inside it.
    let outside = job(send(&s.agent, later(&s.outside.join("c.bin"))).unwrap());
    assert_eq!(outside.state, JobState::AwaitingApproval);
    assert_eq!(reasons(&outside), [ApprovalReason::OutsideGrantedFolders]);
    let error = outside.error.expect("the agent can relay why");
    assert_eq!(error.code.as_str(), "policy.awaiting_approval");
    assert_eq!(error.action, Some(Action::AwaitApproval));

    // The person's own jobs never wait, wherever they go.
    let own = job(send(&s.user, later(&s.outside.join("d.bin"))).unwrap());
    assert_eq!(own.state, JobState::Queued);
    assert_eq!(own.principal, Principal::User);
}

#[test]
fn an_agent_with_no_configured_access_asks_for_everything() {
    let s = setup(granting);
    let stranger = InProcessClient::manual(Arc::clone(&s.engine))
        .with_principal(Principal::try_from("agent:stranger").unwrap());
    let asked = job(send(&stranger, later(&s.granted.join("a.bin"))).unwrap());
    assert_eq!(asked.state, JobState::AwaitingApproval);
    assert_eq!(reasons(&asked), [ApprovalReason::OutsideGrantedFolders]);
}

#[test]
fn traversal_and_links_cannot_carry_a_destination_out_of_a_grant() {
    let s = setup(granting);
    let traversal = PathBuf::from(format!("{}\\..\\granted-not\\a.bin", s.granted.display()));
    assert_eq!(
        code(send(&s.agent, later(&traversal))),
        "input.invalid_request"
    );

    // A junction inside the grant that leads outside it.
    let link = s.granted.join("escape");
    let made = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(&link)
        .arg(&s.outside)
        .output()
        .unwrap();
    assert!(made.status.success(), "{made:?}");
    let through = job(send(&s.agent, later(&link.join("a.bin"))).unwrap());
    assert_eq!(through.state, JobState::AwaitingApproval);
    assert_eq!(reasons(&through), [ApprovalReason::OutsideGrantedFolders]);

    // A grant that is itself a junction counts where it leads.
    let s2 = setup(|_| AgentPolicy::default());
    let alias = s2.root.join("alias");
    let made = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(&alias)
        .arg(&s2.granted)
        .output()
        .unwrap();
    assert!(made.status.success(), "{made:?}");
    send(
        &s2.user,
        Command::SetAgentPolicy {
            agent: agent_name(),
            policy: Some(granting(&alias)),
        },
    )
    .unwrap();
    let real = job(send(&s2.agent, later(&s2.granted.join("a.bin"))).unwrap());
    assert_eq!(real.state, JobState::Queued);
}

#[test]
fn credentials_and_replacement_are_refused_outright_not_offered_for_approval() {
    let s = setup(granting);
    let target = s.granted.join("a.bin");
    assert_eq!(
        code(send(
            &s.agent,
            file("https://user:hunter2@example.test/a.bin", &target)
        )),
        "policy.credentials_not_allowed"
    );
    assert_eq!(
        code(send(
            &s.agent,
            Command::CreateJob {
                request: JobRequest::File {
                    input: JobInput::CredentialRef {
                        credential_ref: CredentialRef::random(),
                    },
                    destination: DestinationIntent {
                        path: target.display().to_string(),
                        conflict: ConflictPolicy::Ask,
                    },
                    not_before: None,
                    expected_sha256: None,
                },
            }
        )),
        "policy.credentials_not_allowed"
    );
    let mut replace = later(&target);
    if let Command::CreateJob {
        request: JobRequest::File { destination, .. },
    } = &mut replace
    {
        destination.conflict = ConflictPolicy::ReplaceExisting;
    }
    assert_eq!(code(send(&s.agent, replace)), "policy.replace_not_allowed");
    assert_eq!(
        code(send(
            &s.agent,
            Command::InspectMedia {
                url: url("https://user:hunter2@video.example.test/watch"),
            }
        )),
        "policy.credentials_not_allowed"
    );
    assert!(
        list(&s.user, JobFilter::All).is_empty(),
        "nothing was created"
    );

    let own = job(send(&s.agent, later(&target)).unwrap());
    assert_eq!(
        code(send(
            &s.agent,
            Command::ResolveDestination {
                job_id: own.job_id.clone(),
                decision: DestinationDecision::ReplaceExisting,
            }
        )),
        "policy.replace_not_allowed"
    );
    // What the bytes must match, and moving them in the same step as a new
    // link, are the person's to do.
    for command in [
        Command::Retry {
            job_id: own.job_id.clone(),
            expected_sha256: Some(String::new()),
        },
        Command::ResolveDestination {
            job_id: own.job_id.clone(),
            decision: DestinationDecision::ChooseNewPath {
                path: s.granted.join("moved.bin").display().to_string(),
                expected_sha256: Some("ab".repeat(32)),
            },
        },
        Command::RefreshSource {
            job_id: own.job_id.clone(),
            destination: None,
            expected_sha256: Some("ab".repeat(32)),
            source: JobInput::Url {
                url: url("http://127.0.0.1:9/a.bin"),
            },
        },
        Command::RefreshSource {
            job_id: own.job_id.clone(),
            destination: Some(s.outside.join("a.bin").display().to_string()),
            expected_sha256: None,
            source: JobInput::Url {
                url: url("http://127.0.0.1:9/a.bin"),
            },
        },
    ] {
        assert_eq!(code(send(&s.agent, command)), "policy.not_permitted");
    }
    assert_eq!(
        code(send(
            &s.agent,
            Command::RefreshSource {
                job_id: own.job_id,
                destination: None,
                expected_sha256: None,
                source: JobInput::Url {
                    url: url("https://token@example.test/a.bin"),
                },
            }
        )),
        "policy.credentials_not_allowed"
    );
}

#[test]
fn an_agent_cannot_use_a_local_torrent_file_as_a_read_path() {
    let s = setup(granting);
    let result = send(
        &s.agent,
        Command::CreateJob {
            request: JobRequest::Torrent {
                input: JobInput::TorrentFile {
                    path: s.granted.join("private.torrent").display().to_string(),
                },
                destination: DestinationIntent {
                    path: s.granted.join("torrent-output").display().to_string(),
                    conflict: ConflictPolicy::Ask,
                },
                not_before: None,
                discover_peers: Some(true),
                upload: false,
            },
        },
    );
    assert_eq!(code(result), "policy.not_permitted");
}

#[test]
fn an_agent_sees_and_controls_only_the_jobs_it_created() {
    let s = setup(granting);
    let mine = job(send(&s.agent, later(&s.granted.join("mine.bin"))).unwrap());
    let persons = job(send(&s.user, later(&s.outside.join("persons.bin"))).unwrap());
    let other_agent = InProcessClient::manual(Arc::clone(&s.engine))
        .with_principal(Principal::try_from("agent:other").unwrap());
    let others = job(send(&other_agent, later(&s.granted.join("others.bin"))).unwrap());

    let seen: Vec<JobId> = list(&s.agent, JobFilter::All)
        .into_iter()
        .map(|job| job.job_id)
        .collect();
    assert_eq!(seen, std::slice::from_ref(&mine.job_id));
    assert_eq!(list(&s.user, JobFilter::All).len(), 3);

    for hidden in [&persons.job_id, &others.job_id] {
        let job_id = hidden.clone();
        for command in [
            Command::GetJob {
                job_id: job_id.clone(),
            },
            Command::JobDetails {
                job_id: job_id.clone(),
            },
            Command::Pause {
                job_id: job_id.clone(),
            },
            Command::Cancel {
                job_id: job_id.clone(),
                retain_partial: false,
            },
            Command::RemoveJob {
                job_id: job_id.clone(),
            },
            Command::Retry {
                job_id: job_id.clone(),
                expected_sha256: None,
            },
        ] {
            assert_eq!(code(send(&s.agent, command)), "contract.unknown_job");
        }
        let subscribe = CommandEnvelope::new(
            ClientId::random(),
            Command::SubscribeJob {
                job_id,
                after_seq: 0,
            },
        );
        assert_eq!(
            s.agent.subscribe(&subscribe).unwrap_err().code.as_str(),
            "contract.unknown_job"
        );
    }
    // Revision checks come after ownership, so a stale revision on someone
    // else's job reveals nothing either.
    let probe = CommandEnvelope::new(
        ClientId::random(),
        Command::Pause {
            job_id: persons.job_id.clone(),
        },
    )
    .expecting_revision(999);
    assert_eq!(
        s.agent.execute(&probe).unwrap_err().code.as_str(),
        "contract.unknown_job"
    );
    // The person's job is untouched.
    assert_eq!(get(&s.user, &persons.job_id).state, JobState::Queued);

    // Its own job it may control.
    let paused = job(send(
        &s.agent,
        Command::Pause {
            job_id: mine.job_id,
        },
    )
    .unwrap());
    assert_eq!(paused.state, JobState::Paused);
}

#[test]
fn only_the_person_approves_denies_or_changes_access_and_settings() {
    let s = setup(granting);
    let waiting = job(send(&s.agent, later(&s.outside.join("a.bin"))).unwrap());
    let queue_stream = CommandEnvelope::new(
        ClientId::random(),
        Command::SubscribeQueue { after_cursor: 0 },
    );
    assert_eq!(
        s.agent.subscribe(&queue_stream).unwrap_err().code.as_str(),
        "policy.not_permitted"
    );
    let settings = EngineSettings {
        max_active_downloads: 8,
        default_destination_dir: Some(s.outside.display().to_string()),
        auto_retry: true,
        auto_retry_max_attempts: 3,
        auto_retry_base_delay_seconds: 10,
        close_to_tray: true,
        power_mode: true,
        media_tools_dir: None,
        confirm_remove_completed: false,
        theme: Theme::Dark,
        onboarding_completed: true,
        start_engine_at_sign_in: Some(true),
        cache_quota_bytes: None,
        density: None,
        instance_name: None,
        hub_mode: None,
    };
    for command in [
        Command::ApproveJob {
            job_id: waiting.job_id.clone(),
        },
        Command::DenyJob {
            job_id: waiting.job_id.clone(),
        },
        Command::SetAgentPolicy {
            agent: agent_name(),
            policy: Some(granting(&s.root)),
        },
        Command::UpdateSettings { settings },
        Command::GetSettings,
        Command::QueueStats,
        Command::EngineShutdown,
        Command::TakeLinkReviews,
        Command::CreateJobs {
            requests: Vec::new(),
        },
    ] {
        let name = command.name();
        assert_eq!(
            code(send(&s.agent, command)),
            "policy.not_permitted",
            "{name}"
        );
    }
    assert_eq!(
        get(&s.agent, &waiting.job_id).state,
        JobState::AwaitingApproval
    );
    // Nothing the agent can send starts a job that is waiting.
    for command in [
        Command::Start {
            job_id: waiting.job_id.clone(),
        },
        Command::Resume {
            job_id: waiting.job_id.clone(),
        },
        Command::Retry {
            job_id: waiting.job_id.clone(),
            expected_sha256: None,
        },
        Command::ResolveDestination {
            job_id: waiting.job_id.clone(),
            decision: DestinationDecision::ChooseNewPath {
                path: s.granted.join("moved.bin").display().to_string(),
                expected_sha256: None,
            },
        },
    ] {
        let _ = send(&s.agent, command);
        assert_eq!(
            get(&s.agent, &waiting.job_id).state,
            JobState::AwaitingApproval
        );
    }
    let paused = send(
        &s.agent,
        Command::Pause {
            job_id: waiting.job_id.clone(),
        },
    )
    .unwrap();
    assert_eq!(
        job(paused).state,
        JobState::AwaitingApproval,
        "pause is a no-op"
    );

    // The browser host may add downloads and have its inbox taken in
    // (FP-056), and nothing else.
    let browser = InProcessClient::manual(Arc::clone(&s.engine)).with_principal(Principal::Browser);
    let captured = job(send(&browser, later(&s.outside.join("captured.bin"))).unwrap());
    assert_eq!(captured.principal, Principal::Browser);
    assert_eq!(captured.state, JobState::Queued);
    assert!(matches!(
        send(&browser, Command::TakeBrowserCaptures).unwrap(),
        CommandResult::CapturesTaken
    ));
    assert_eq!(
        code(send(&s.agent, Command::TakeBrowserCaptures)),
        "policy.not_permitted",
        "an agent cannot"
    );
    for command in [
        Command::TakeLinkReviews,
        Command::ListJobs {
            filter: JobFilter::All,
        },
        Command::GetJob {
            job_id: captured.job_id.clone(),
        },
        Command::Cancel {
            job_id: captured.job_id,
            retain_partial: false,
        },
        Command::EngineStatus,
    ] {
        assert_eq!(code(send(&browser, command)), "policy.not_permitted");
    }
}

/// An agent may read its own access (D3), so its MCP server can offer its
/// folders; it never sees another agent's.
#[test]
fn an_agent_reads_its_own_access_and_no_one_elses() {
    let s = setup(granting);
    let other = AgentName::try_from("other").unwrap();
    send(
        &s.user,
        Command::SetAgentPolicy {
            agent: other.clone(),
            policy: Some(granting(&s.outside)),
        },
    )
    .unwrap();
    let seen = |client: &InProcessClient| match send(client, Command::GetAgentPolicies).unwrap() {
        CommandResult::AgentPolicies { policies } => policies,
        other => panic!("{other:?}"),
    };
    assert_eq!(seen(&s.user).len(), 2);
    let own = seen(&s.agent);
    assert_eq!(own.len(), 1);
    assert_eq!(own[0].agent, agent_name());
    assert_eq!(own[0].policy.folders, vec![s.granted.display().to_string()]);

    // An agent the person never configured sees the default: no folders.
    let stranger = InProcessClient::manual(Arc::clone(&s.engine))
        .with_principal(Principal::try_from("agent:stranger").unwrap());
    let own = seen(&stranger);
    assert_eq!(own.len(), 1);
    assert_eq!(own[0].agent.as_str(), "stranger");
    assert!(own[0].policy.folders.is_empty());
}

/// An agent's history search cannot spell out a folder it was not granted
/// (FP-065 review): outside its grant only the file name matches.
#[test]
fn an_agent_history_search_matches_hidden_folders_by_file_name_only() {
    let s = setup(granting);
    let hidden = s.root.join("AcmeCorp-private");
    std::fs::create_dir_all(&hidden).unwrap();
    for destination in [hidden.join("report.bin"), s.granted.join("inside.bin")] {
        let asked = job(send(&s.agent, later(&destination)).unwrap());
        send(
            &s.agent,
            Command::Cancel {
                job_id: asked.job_id.clone(),
                retain_partial: false,
            },
        )
        .unwrap();
    }
    let search = |client: &InProcessClient, text: &str| -> usize {
        match send(
            client,
            Command::History {
                query: Some(text.to_owned()),
                limit: None,
            },
        )
        .unwrap()
        {
            CommandResult::Jobs { jobs } => jobs.len(),
            other => panic!("{other:?}"),
        }
    };
    assert_eq!(search(&s.agent, "acmecorp"), 0);
    assert_eq!(search(&s.agent, "report.bin"), 1);
    // Inside the grant the folder matches, and the person matches anything.
    let granted_name = s
        .granted
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    assert_eq!(search(&s.agent, &format!("{granted_name}\\inside")), 1);
    assert_eq!(search(&s.user, "acmecorp"), 1);
}

/// Changing an agent's access re-checks what it started (FP-066): a folder
/// taken away holds that folder's downloads, and revoking the agent stops a
/// running one at its checkpoint until the person approves it.
#[test]
fn revoking_an_agent_stops_its_downloads_until_the_person_approves_them() {
    let body: Vec<u8> = (0..2 * 1024 * 1024).map(|i| (i % 251) as u8).collect();
    let link = serve(body.clone(), true);
    let s = setup(granting);
    let kept = s.root.join("kept");
    std::fs::create_dir_all(&kept).unwrap();
    send(
        &s.user,
        Command::SetAgentPolicy {
            agent: agent_name(),
            policy: Some(AgentPolicy {
                folders: vec![s.granted.display().to_string(), kept.display().to_string()],
                ..AgentPolicy::default()
            }),
        },
    )
    .unwrap();
    let in_granted = job(send(&s.agent, later(&s.granted.join("a.bin"))).unwrap());
    let in_kept = job(send(&s.agent, later(&kept.join("b.bin"))).unwrap());

    // Narrowing: only the folder taken away is held.
    send(
        &s.user,
        Command::SetAgentPolicy {
            agent: agent_name(),
            policy: Some(granting(&kept)),
        },
    )
    .unwrap();
    let held = get(&s.agent, &in_granted.job_id);
    assert_eq!(held.state, JobState::AwaitingApproval);
    assert_eq!(reasons(&held), [ApprovalReason::OutsideGrantedFolders]);
    assert_eq!(get(&s.agent, &in_kept.job_id).state, JobState::Queued);

    // Revoking: a running download stops and nothing is published.
    let destination = kept.join("big.bin");
    let running = job(send(&s.agent, file(&link, &destination)).unwrap());
    wait_for(&s.engine, &s.agent, &running.job_id, |job| {
        job.state == JobState::Running && job.progress.bytes_received > 0
    });
    send(
        &s.user,
        Command::SetAgentPolicy {
            agent: agent_name(),
            policy: None,
        },
    )
    .unwrap();
    let stopped = get(&s.agent, &running.job_id);
    assert_eq!(stopped.state, JobState::AwaitingApproval, "{stopped:?}");
    assert_eq!(reasons(&stopped), [ApprovalReason::OutsideGrantedFolders]);
    assert_eq!(
        get(&s.agent, &in_kept.job_id).state,
        JobState::AwaitingApproval
    );
    thread::sleep(Duration::from_millis(300));
    s.engine.tick();
    assert_eq!(
        get(&s.agent, &running.job_id).state,
        JobState::AwaitingApproval
    );
    assert!(!destination.exists(), "nothing is published while it waits");

    // The person approves it, and it finishes from where it stopped.
    send(
        &s.user,
        Command::ApproveJob {
            job_id: running.job_id.clone(),
        },
    )
    .unwrap();
    let finished = wait_for(&s.engine, &s.agent, &running.job_id, |job| {
        job.state.is_terminal() || job.state == JobState::Failed
    });
    assert_eq!(finished.state, JobState::Completed, "{finished:?}");
    assert_eq!(std::fs::read(&destination).unwrap(), body);
}

#[test]
fn approval_queues_the_job_and_denial_ends_it_with_a_reason_the_agent_can_relay() {
    let s = setup(granting);
    let approve = job(send(&s.agent, later(&s.outside.join("a.bin"))).unwrap());
    let deny = job(send(&s.agent, later(&s.outside.join("b.bin"))).unwrap());
    assert_eq!(list(&s.user, JobFilter::AwaitingApproval).len(), 2);

    let approved = job(send(
        &s.user,
        Command::ApproveJob {
            job_id: approve.job_id.clone(),
        },
    )
    .unwrap());
    assert_eq!(approved.state, JobState::Queued);
    assert_eq!(approved.approval, None);
    assert_eq!(approved.error, None);

    let denied = job(send(
        &s.user,
        Command::DenyJob {
            job_id: deny.job_id.clone(),
        },
    )
    .unwrap());
    assert_eq!(denied.state, JobState::Cancelled);
    assert_eq!(
        denied.error.as_ref().map(|error| error.code.as_str()),
        Some("policy.approval_denied")
    );
    assert_eq!(
        get(&s.agent, &deny.job_id).error.unwrap().code.as_str(),
        "policy.approval_denied"
    );
    // The agent cannot bring back what the person declined.
    for command in [
        Command::Retry {
            job_id: deny.job_id.clone(),
            expected_sha256: None,
        },
        Command::RefreshSource {
            job_id: deny.job_id.clone(),
            destination: None,
            expected_sha256: None,
            source: JobInput::Url {
                url: url("http://127.0.0.1:9/other.bin"),
            },
        },
    ] {
        assert!(send(&s.agent, command).is_err());
        assert_eq!(get(&s.user, &deny.job_id).state, JobState::Cancelled);
    }
    // Deciding twice is refused; nothing is waiting any more.
    assert_eq!(
        code(send(
            &s.user,
            Command::ApproveJob {
                job_id: deny.job_id
            }
        )),
        "contract.invalid_transition"
    );
    assert!(list(&s.user, JobFilter::AwaitingApproval).is_empty());

    // The agent may withdraw its own request.
    let withdrawn = job(send(&s.agent, later(&s.outside.join("c.bin"))).unwrap());
    let cancelled = job(send(
        &s.agent,
        Command::Cancel {
            job_id: withdrawn.job_id,
            retain_partial: false,
        },
    )
    .unwrap());
    assert_eq!(cancelled.state, JobState::Cancelled);
}

#[test]
fn a_wait_for_approval_survives_an_engine_restart() {
    let s = setup(granting);
    let waiting = job(send(&s.agent, later(&s.outside.join("a.bin"))).unwrap());
    let queue = s.queue.clone();
    drop(s.user);
    drop(s.agent);
    drop(s.engine);

    let engine = open(&queue);
    let (user, agent) = clients(&engine);
    engine.tick();
    let restored = get(&agent, &waiting.job_id);
    assert_eq!(restored.state, JobState::AwaitingApproval);
    assert_eq!(restored.principal, helper());
    assert_eq!(reasons(&restored), [ApprovalReason::OutsideGrantedFolders]);
    // The grant survived too.
    match send(&user, Command::GetAgentPolicies).unwrap() {
        CommandResult::AgentPolicies { policies } => assert_eq!(policies.len(), 1),
        other => panic!("{other:?}"),
    }
    let approved = job(send(
        &user,
        Command::ApproveJob {
            job_id: waiting.job_id,
        },
    )
    .unwrap());
    assert_eq!(approved.state, JobState::Queued);
}

#[test]
fn a_command_id_reused_by_another_principal_never_returns_the_first_result() {
    let s = setup(granting);
    let envelope = CommandEnvelope::new(ClientId::random(), later(&s.outside.join("a.bin")));
    let first = s.user.execute(&envelope).unwrap();
    assert_eq!(
        s.agent.execute(&envelope).unwrap_err().code.as_str(),
        "contract.idempotency_conflict"
    );
    // The person's own resend still gets its result.
    assert_eq!(s.user.execute(&envelope).unwrap(), first);
    assert!(list(&s.agent, JobFilter::All).is_empty());
}

#[test]
fn an_agent_past_its_rate_waits_and_one_with_too_many_waiting_is_refused() {
    let s = setup(|granted| AgentPolicy {
        max_new_jobs_per_hour: 2,
        ..granting(granted)
    });
    for name in ["a.bin", "b.bin"] {
        let queued = job(send(&s.agent, later(&s.granted.join(name))).unwrap());
        assert_eq!(queued.state, JobState::Queued);
    }
    let third = job(send(&s.agent, later(&s.granted.join("c.bin"))).unwrap());
    assert_eq!(third.state, JobState::AwaitingApproval);
    assert_eq!(reasons(&third), [ApprovalReason::RateLimit]);

    for index in 1..20 {
        let waiting = job(send(&s.agent, later(&s.granted.join(format!("{index}.bin")))).unwrap());
        assert_eq!(waiting.state, JobState::AwaitingApproval);
    }
    assert_eq!(
        code(send(&s.agent, later(&s.granted.join("one-too-many.bin")))),
        "policy.too_many_pending"
    );
    assert_eq!(list(&s.agent, JobFilter::AwaitingApproval).len(), 20);
}

/// Serves `body` slowly, stating its length only when asked to.
fn serve(body: Vec<u8>, state_length: bool) -> String {
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
                    thread::sleep(Duration::from_millis(15));
                }
            });
        }
    });
    url
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
        thread::sleep(Duration::from_millis(10));
    }
}

fn size_stop_then_approval(state_length: bool) {
    let body: Vec<u8> = (0..512 * 1024).map(|i| (i % 251) as u8).collect();
    let link = serve(body.clone(), state_length);
    let s = setup(|granted| AgentPolicy {
        max_bytes: 64 * 1024,
        ..granting(granted)
    });
    let destination = s.granted.join("big.bin");
    let created = job(send(&s.agent, file(&link, &destination)).unwrap());
    assert!(matches!(
        created.state,
        JobState::Queued | JobState::Running
    ));

    let stopped = wait_for(&s.engine, &s.agent, &created.job_id, |job| {
        job.state != JobState::Queued && job.state != JobState::Running
    });
    assert_eq!(stopped.state, JobState::AwaitingApproval, "{stopped:?}");
    assert_eq!(reasons(&stopped), [ApprovalReason::SizeLimit]);
    assert!(!destination.exists(), "nothing is published while it waits");
    thread::sleep(Duration::from_millis(300));
    s.engine.tick();
    assert_eq!(
        get(&s.agent, &created.job_id).state,
        JobState::AwaitingApproval,
        "the stop holds"
    );

    let approved = job(send(
        &s.user,
        Command::ApproveJob {
            job_id: created.job_id.clone(),
        },
    )
    .unwrap());
    assert_eq!(approved.approval, None);
    let finished = wait_for(&s.engine, &s.agent, &created.job_id, |job| {
        job.state.is_terminal() || job.state == JobState::Failed
    });
    assert_eq!(finished.state, JobState::Completed, "{finished:?}");
    assert_eq!(std::fs::read(&destination).unwrap(), body);
}

#[test]
fn a_stated_size_over_the_limit_stops_the_download_until_the_person_approves_it() {
    size_stop_then_approval(true);
}

#[test]
fn an_unknown_size_that_grows_past_the_limit_stops_until_the_person_approves_it() {
    size_stop_then_approval(false);
}

#[test]
fn a_withdrawn_request_retried_by_its_agent_waits_for_the_person_again() {
    let s = setup(granting);
    let asked = job(send(
        &s.agent,
        file("http://127.0.0.1:9/a.bin", &s.outside.join("a.bin")),
    )
    .unwrap());
    assert_eq!(asked.state, JobState::AwaitingApproval);
    let withdrawn = job(send(
        &s.agent,
        Command::Cancel {
            job_id: asked.job_id.clone(),
            retain_partial: false,
        },
    )
    .unwrap());
    assert_eq!(withdrawn.state, JobState::Cancelled);

    let retried = job(send(
        &s.agent,
        Command::Retry {
            job_id: asked.job_id.clone(),
            expected_sha256: None,
        },
    )
    .unwrap());
    assert_eq!(retried.state, JobState::AwaitingApproval);
    assert_eq!(reasons(&retried), [ApprovalReason::OutsideGrantedFolders]);

    // The same through a new link.
    send(
        &s.agent,
        Command::Cancel {
            job_id: asked.job_id.clone(),
            retain_partial: false,
        },
    )
    .unwrap();
    let refreshed = job(send(
        &s.agent,
        Command::RefreshSource {
            job_id: asked.job_id.clone(),
            destination: None,
            expected_sha256: None,
            source: JobInput::Url {
                url: url("http://127.0.0.1:9/b.bin"),
            },
        },
    )
    .unwrap());
    assert_eq!(refreshed.state, JobState::AwaitingApproval);
    assert!(
        refreshed.source_display.ends_with("/b.bin"),
        "{refreshed:?}"
    );
}

#[test]
fn an_approved_job_given_a_new_link_by_its_agent_is_checked_again() {
    let s = setup(granting);
    let asked = job(send(
        &s.agent,
        file("http://127.0.0.1:9/a.bin", &s.outside.join("a.bin")),
    )
    .unwrap());
    send(
        &s.user,
        Command::ApproveJob {
            job_id: asked.job_id.clone(),
        },
    )
    .unwrap();
    send(
        &s.agent,
        Command::Cancel {
            job_id: asked.job_id.clone(),
            retain_partial: false,
        },
    )
    .unwrap();
    wait_for(&s.engine, &s.agent, &asked.job_id, |job| {
        matches!(job.state, JobState::Cancelled | JobState::Failed)
    });
    let redirected = job(send(
        &s.agent,
        Command::RefreshSource {
            job_id: asked.job_id,
            destination: None,
            expected_sha256: None,
            source: JobInput::Url {
                url: url("http://127.0.0.1:9/other.bin"),
            },
        },
    )
    .unwrap());
    assert_eq!(redirected.state, JobState::AwaitingApproval);
    assert_eq!(
        reasons(&redirected),
        [ApprovalReason::OutsideGrantedFolders]
    );
}

#[test]
fn a_request_withdrawn_past_the_rate_cannot_be_retried_around_it() {
    let s = setup(|granted| AgentPolicy {
        max_new_jobs_per_hour: 1,
        ..granting(granted)
    });
    job(send(&s.agent, later(&s.granted.join("a.bin"))).unwrap());
    let over = job(send(&s.agent, later(&s.granted.join("b.bin"))).unwrap());
    assert_eq!(reasons(&over), [ApprovalReason::RateLimit]);
    send(
        &s.agent,
        Command::Cancel {
            job_id: over.job_id.clone(),
            retain_partial: false,
        },
    )
    .unwrap();
    let retried = job(send(
        &s.agent,
        Command::Retry {
            job_id: over.job_id,
            expected_sha256: None,
        },
    )
    .unwrap());
    assert_eq!(retried.state, JobState::AwaitingApproval);
    assert_eq!(reasons(&retried), [ApprovalReason::RateLimit]);
}

#[test]
fn a_refused_retry_keeps_the_hold_it_would_have_shed() {
    let s = setup(|granted| AgentPolicy {
        max_new_jobs_per_hour: 1,
        ..granting(granted)
    });
    job(send(&s.agent, later(&s.granted.join("a.bin"))).unwrap());
    let over = job(send(&s.agent, later(&s.granted.join("b.bin"))).unwrap());
    send(
        &s.agent,
        Command::Cancel {
            job_id: over.job_id.clone(),
            retain_partial: false,
        },
    )
    .unwrap();
    assert!(
        send(
            &s.agent,
            Command::RefreshSource {
                job_id: over.job_id.clone(),
                destination: None,
                expected_sha256: None,
                source: JobInput::Url {
                    url: url("ftp://nope"),
                },
            },
        )
        .is_err()
    );
    let retried = job(send(
        &s.agent,
        Command::Retry {
            job_id: over.job_id,
            expected_sha256: None,
        },
    )
    .unwrap());
    assert_eq!(retried.state, JobState::AwaitingApproval);
    assert_eq!(reasons(&retried), [ApprovalReason::RateLimit]);
}

/// Answers every request with one fixed response.
fn answer_with(response: &'static str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/get", listener.local_addr().unwrap());
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut request = [0_u8; 4096];
            let _ = stream.read(&mut request);
            let _ = stream.write_all(response.as_bytes());
        }
    });
    url
}

fn inspect(client: &InProcessClient, link: &str) -> fetchpath_protocol::model::LinkInspection {
    match send(client, Command::InspectLink { url: url(link) }).unwrap() {
        CommandResult::LinkInspection { inspection } => inspection,
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn a_link_is_looked_at_without_downloading_it_and_never_with_credentials_for_an_agent() {
    use fetchpath_protocol::model::LinkKind;
    let s = setup(granting);
    // A known video site is recognized without a request.
    let video = inspect(&s.agent, "https://www.youtube.com/watch?v=x");
    assert_eq!(video.kind, LinkKind::MediaPage);
    let file = inspect(
        &s.user,
        &answer_with(
            "HTTP/1.1 206 Partial Content\r\nContent-Type: application/zip\r\n\
             Content-Range: bytes 0-0/9000\r\nContent-Length: 1\r\n\
             Content-Disposition: attachment; filename=\"r.zip\"\r\n\r\nx",
        ),
    );
    assert_eq!(file.kind, LinkKind::File);
    assert_eq!(file.file_name.as_deref(), Some("r.zip"));
    assert_eq!((file.size_bytes, file.resumable), (Some(9000), true));
    let page = inspect(
        &s.user,
        &answer_with(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: 5\r\n\r\n<html",
        ),
    );
    assert_eq!(page.kind, LinkKind::WebPage);
    assert_eq!(
        code(send(
            &s.agent,
            Command::InspectLink {
                url: url("https://user:hunter2@example.test/a.bin"),
            }
        )),
        "policy.credentials_not_allowed"
    );
    assert!(
        list(&s.user, JobFilter::All).is_empty(),
        "looking queued nothing"
    );
}

fn rules(result: CommandResult) -> Vec<fetchpath_protocol::model::Rule> {
    match result {
        CommandResult::Rules { rules } => rules,
        other => panic!("not rules: {other:?}"),
    }
}

fn add_rule(
    client: &InProcessClient,
    when: fetchpath_protocol::model::RuleConditions,
    then: fetchpath_protocol::model::RuleActions,
) -> Result<CommandResult, ProtocolError> {
    send(
        client,
        Command::AddRule {
            rule: Box::new(fetchpath_protocol::model::RuleSpec {
                name: None,
                when,
                then,
            }),
            position: None,
        },
    )
}

/// `later`, from `link`, with an optional checksum.
fn later_from(link: &str, destination: &str, checksum: Option<&str>) -> Command {
    let mut command = later(Path::new(destination));
    if let Command::CreateJob {
        request:
            JobRequest::File {
                input,
                expected_sha256,
                ..
            },
    } = &mut command
    {
        *input = JobInput::Url { url: url(link) };
        *expected_sha256 = checksum.map(str::to_owned);
    }
    command
}

#[test]
fn smart_rules_place_and_check_new_jobs_for_every_principal_and_an_agent_stays_in_its_grant() {
    use fetchpath_protocol::model::{RuleActions, RuleConditions};
    let s = setup(granting);
    let isos = add_rule(
        &s.user,
        RuleConditions {
            file_types: vec!["iso".into()],
            ..RuleConditions::default()
        },
        RuleActions {
            folder: Some(s.outside.display().to_string()),
            ..RuleActions::default()
        },
    )
    .unwrap();
    assert_eq!(rules(isos)[0].id, 1);
    add_rule(
        &s.user,
        RuleConditions {
            domains: vec!["checked.test".into()],
            ..RuleConditions::default()
        },
        RuleActions {
            require_checksum: true,
            ..RuleActions::default()
        },
    )
    .unwrap();

    // Only the person changes rules.
    assert_eq!(
        code(add_rule(
            &s.agent,
            RuleConditions {
                file_types: vec!["exe".into()],
                ..RuleConditions::default()
            },
            RuleActions {
                folder: Some(s.granted.display().to_string()),
                ..RuleActions::default()
            },
        )),
        "policy.not_permitted"
    );
    assert_eq!(
        code(send(&s.agent, Command::RemoveRule { rule_id: 1 })),
        "policy.not_permitted"
    );

    // A bare file name goes where the matching rule says.
    let placed = job(send(
        &s.user,
        later_from("http://127.0.0.1:9/os.iso", "os.iso", None),
    )
    .unwrap());
    assert_eq!(
        placed.destination.as_deref().map(PathBuf::from),
        Some(s.outside.join("os.iso"))
    );
    // For an agent the rule's folder is outside its grant, so it waits.
    let held = job(send(
        &s.agent,
        later_from("http://127.0.0.1:9/os.iso", "os.iso", None),
    )
    .unwrap());
    assert_eq!(held.state, JobState::AwaitingApproval);
    assert_eq!(reasons(&held), [ApprovalReason::OutsideGrantedFolders]);

    // A rule that requires a checksum refuses a file without one, for anyone.
    let target = s.granted.join("a.bin");
    let target = target.display().to_string();
    for client in [&s.user, &s.agent] {
        assert_eq!(
            code(send(
                client,
                later_from("http://checked.test/a.bin", &target, None)
            )),
            "integrity.checksum_required"
        );
    }
    let checksum = "a".repeat(64);
    job(send(
        &s.user,
        later_from("http://checked.test/a.bin", &target, Some(&checksum)),
    )
    .unwrap());

    // Looking at a link says which rule decides and why.
    let look = inspect(
        &s.user,
        &answer_with(
            "HTTP/1.1 206 Partial Content\r\nContent-Type: application/octet-stream\r\n\
             Content-Range: bytes 0-0/9000\r\nContent-Length: 1\r\n\
             Content-Disposition: attachment; filename=\"disc.ISO\"\r\n\r\nx",
        ),
    );
    let verdict = look.rules.expect("a verdict");
    assert_eq!(verdict.matched.map(|rule| rule.id), Some(1));
    assert_eq!(verdict.checks[0].reasons, ["the file is a .iso"]);

    // Rules are kept with the settings and survive a restart; removing one
    // that does not exist is refused.
    drop(s.user);
    drop(s.agent);
    drop(s.engine);
    let engine = open(&s.queue);
    let (user, _) = clients(&engine);
    assert_eq!(rules(send(&user, Command::ListRules).unwrap()).len(), 2);
    assert_eq!(
        rules(send(&user, Command::RemoveRule { rule_id: 1 }).unwrap())
            .iter()
            .map(|rule| rule.id)
            .collect::<Vec<_>>(),
        [2]
    );
    assert_eq!(
        code(send(&user, Command::RemoveRule { rule_id: 1 })),
        "input.invalid_request"
    );
}

// ------------------------------------------------------------------ FP-067
// Attacks on the agent boundary, derived from the platform design (§6, §8)
// and contract D1 to D4. Each is a way an agent could break an invariant; the
// test proves it holds. Findings and residual risks: docs/development/MCP.md.

fn status(client: &InProcessClient) -> fetchpath_protocol::model::EngineStatus {
    match send(client, Command::EngineStatus).unwrap() {
        CommandResult::EngineStatus { status } => status,
        other => panic!("{other:?}"),
    }
}

/// The engine's status shows an agent nothing of other principals' activity:
/// not the queue's event cursor, not how many clients are connected.
#[test]
fn engine_status_tells_an_agent_nothing_about_others() {
    let s = setup(granting);
    job(send(&s.user, later(&s.outside.join("persons.bin"))).unwrap());
    job(send(&s.user, later(&s.outside.join("persons-2.bin"))).unwrap());
    assert!(status(&s.user).queue_cursor > 0);
    let seen = status(&s.agent);
    assert_eq!(seen.queue_cursor, 0);
    assert_eq!(seen.connected_clients, 0);
    assert_eq!(seen.active_jobs, 0);
}

/// Every part of a destination is a name Windows can hold as a plain file or
/// folder: no reserved device names, alternate data streams or trailing dots
/// in a folder, and no device or verbatim paths that skip Windows' own path
/// rules. These are refused for everyone, before any grant is considered.
#[test]
fn destinations_are_refused_when_any_part_is_not_a_plain_name() {
    let s = setup(granting);
    let granted = s.granted.display().to_string();
    let refused = [
        format!(r"{granted}\CON\a.bin"),
        format!(r"{granted}\nul.txt\a.bin"),
        format!(r"{granted}\sub.\a.bin"),
        format!(r"{granted}\sub \a.bin"),
        format!(r"{granted}\notes.txt:stream\a.bin"),
        format!(r"{granted}\CONIN$"),
        format!(r"{granted}\CONOUT$.txt"),
        format!("{granted}\\COM\u{b9}.txt"),
        format!("{granted}\\LPT\u{b3}"),
        format!(r"\\?\{granted}\a.bin"),
        format!(r"\\.\{granted}\a.bin"),
    ];
    for destination in &refused {
        for client in [&s.agent, &s.user] {
            let result = send(client, later(Path::new(destination)));
            assert!(result.is_err(), "{destination} was accepted: {result:?}");
        }
    }
    // A plain name still works, in the grant and without asking.
    let fine = job(send(&s.agent, later(&s.granted.join("sub").join("a.bin"))).unwrap());
    assert_eq!(fine.state, JobState::Queued);
}

/// User info smuggled in other spellings is still user info: percent-encoded,
/// upper-case schemes, IPv6 hosts, a bare token, or behind a backslash that
/// URL parsing treats as a slash. None of it downloads or is looked at.
#[test]
fn credentials_in_any_spelling_are_refused_for_an_agent() {
    let s = setup(granting);
    let target = s.granted.join("smuggled.bin");
    let mut tried = 0;
    for link in [
        "http://u%40x:p%3Aw@example.test/a.bin",
        "HTTPS://User:Secret@Example.test/a.bin",
        "http://token@[::1]:8080/a.bin",
        "http://:secret@example.test/a.bin",
        "http:\\\\user:secret@example.test\\a.bin",
    ] {
        // A link the protocol cannot even carry is refused before the engine.
        let Ok(url) = SensitiveUrl::try_from(link.to_owned()) else {
            continue;
        };
        tried += 1;
        let request = JobRequest::File {
            input: JobInput::Url { url: url.clone() },
            destination: DestinationIntent {
                path: target.display().to_string(),
                conflict: ConflictPolicy::Ask,
            },
            not_before: None,
            expected_sha256: None,
        };
        let created = send(&s.agent, Command::CreateJob { request });
        assert!(created.is_err(), "{link} was accepted: {created:?}");
        let looked = send(&s.agent, Command::InspectLink { url });
        assert!(looked.is_err(), "{link} was looked at: {looked:?}");
    }
    assert!(tried >= 4, "only {tried} spellings reached the engine");
    assert!(list(&s.agent, JobFilter::All).is_empty());
}

/// Withdrawing requests does not give back the hourly rate: an agent that
/// cancels and asks again still waits for the person once past it.
#[test]
fn cancelling_and_asking_again_does_not_reset_the_rate() {
    let s = setup(|granted| AgentPolicy {
        max_new_jobs_per_hour: 2,
        ..granting(granted)
    });
    for name in ["a.bin", "b.bin"] {
        let queued = job(send(&s.agent, later(&s.granted.join(name))).unwrap());
        send(
            &s.agent,
            Command::Cancel {
                job_id: queued.job_id.clone(),
                retain_partial: false,
            },
        )
        .unwrap();
        send(
            &s.agent,
            Command::RemoveJob {
                job_id: queued.job_id,
            },
        )
        .unwrap();
    }
    let again = job(send(&s.agent, later(&s.granted.join("c.bin"))).unwrap());
    assert_eq!(again.state, JobState::AwaitingApproval);
    assert_eq!(reasons(&again), [ApprovalReason::RateLimit]);
}

/// A name that reads differently from what it is cannot reach the person's
/// approval card: direction overrides (`invoice\u{202E}txt.exe` shows as
/// "invoiceexe.txt"), isolates, and line or paragraph separators are refused
/// in every part of a destination. Joiners that languages and emoji need are
/// not (FP-067 review).
#[test]
fn a_destination_cannot_disguise_itself_on_the_approval_card() {
    let s = setup(granting);
    // Persian with a zero-width non-joiner, and an emoji family with joiners.
    for name in [
        "\u{06AF}\u{0632}\u{0627}\u{0631}\u{0634}\u{200C}\u{0647}\u{0627}.pdf",
        "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}.jpg",
    ] {
        let fine = job(send(&s.agent, later(&s.granted.join(name))).unwrap());
        assert_eq!(fine.state, JobState::Queued, "{name}");
    }
    for name in [
        "invoice\u{202E}txt.exe",
        "report\u{2066}.pdf\u{2069}.exe",
        "embedded\u{202B}.exe",
        "two\u{2028}lines.bin",
        "two\u{2029}paragraphs.bin",
    ] {
        let destination = s.outside.join(name);
        let result = send(&s.agent, later(&destination));
        assert!(result.is_err(), "{name:?} was accepted: {result:?}");
        let folder = s.outside.join(name).join("a.bin");
        assert!(
            send(&s.agent, later(&folder)).is_err(),
            "{name:?} as a folder"
        );
    }
}

/// An agent following its own download learns nothing of others from the
/// stream: its events' cursors count its own stream, not the engine's queue
/// (FP-067 review).
#[test]
fn an_agent_job_stream_counts_only_its_own_events() {
    let s = setup(granting);
    for index in 0..5 {
        job(send(
            &s.user,
            later(&s.outside.join(format!("persons-{index}.bin"))),
        )
        .unwrap());
    }
    let mine = job(send(&s.agent, later(&s.granted.join("mine.bin"))).unwrap());
    let stream = CommandEnvelope::new(
        ClientId::random(),
        Command::SubscribeJob {
            job_id: mine.job_id.clone(),
            after_seq: 0,
        },
    );
    let mut subscription = s.agent.subscribe(&stream).unwrap();
    send(
        &s.agent,
        Command::Cancel {
            job_id: mine.job_id.clone(),
            retain_partial: false,
        },
    )
    .unwrap();
    s.engine.tick();
    let mut cursors = Vec::new();
    while let Ok(Some(item)) = subscription.events.next_item(Duration::from_millis(300)) {
        if let fetchpath_protocol::StreamItem::Event(event) = item {
            cursors.push(event.cursor);
        }
    }
    assert!(!cursors.is_empty());
    let expected: Vec<u64> = (1..=cursors.len() as u64).collect();
    assert_eq!(cursors, expected, "the person's five jobs show through");
}

fn checked(link: &str, destination: &Path, sha256: &str) -> Command {
    let mut command = file(link, destination);
    if let Command::CreateJob {
        request: JobRequest::File {
            expected_sha256, ..
        },
    } = &mut command
    {
        *expected_sha256 = Some(sha256.to_owned());
    }
    command
}

fn cache(client: &InProcessClient, command: Command) -> fetchpath_protocol::model::CacheView {
    match send(client, command).unwrap() {
        CommandResult::Cache { cache } => cache,
        other => panic!("{other:?}"),
    }
}

/// FP-032: a checksum-verified download fills the cache, the same file again
/// completes from it with no transfer and no rate, an agent's job never
/// reads it, and the person bounds and clears it.
#[test]
fn a_checksum_download_is_reused_from_the_cache_for_the_person_but_never_for_an_agent() {
    use sha2::{Digest, Sha256};
    let dir = tempfile::tempdir().unwrap();
    let granted = dir.path().join("granted");
    std::fs::create_dir_all(&granted).unwrap();
    let session = Session::load_with_browser(dir.path().join("queue-v1.json"), 3, None).unwrap();
    session.use_cache(dir.path().join("cache"));
    let engine = Engine::new(Arc::new(session));
    let (user, agent) = clients(&engine);
    send(
        &user,
        Command::SetAgentPolicy {
            agent: agent_name(),
            policy: Some(granting(&granted)),
        },
    )
    .unwrap();

    let body: Vec<u8> = (0..96 * 1024).map(|i| (i % 253) as u8).collect();
    let sha256 = format!("{:x}", Sha256::digest(&body));
    let finished = |job: &JobSnapshot| matches!(job.state, JobState::Completed | JobState::Failed);
    let first = job(send(
        &user,
        checked(
            &serve(body.clone(), true),
            &granted.join("first.bin"),
            &sha256,
        ),
    )
    .unwrap());
    let first = wait_for(&engine, &user, &first.job_id, finished);
    assert_eq!(first.state, JobState::Completed);
    assert!(!first.reused_from_cache);
    // The copy into the cache follows the completion.
    let deadline = Instant::now() + Duration::from_secs(10);
    while cache(&user, Command::CacheStatus).entries == 0 && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(20));
    }
    let status = cache(&user, Command::CacheStatus);
    assert_eq!((status.entries, status.bytes), (1, body.len() as u64));

    // Nothing listens here: only the cache can complete these.
    let dead = "http://127.0.0.1:9/file.bin";
    let again = job(send(&user, checked(dead, &granted.join("again.bin"), &sha256)).unwrap());
    let again = wait_for(&engine, &user, &again.job_id, finished);
    assert_eq!(again.state, JobState::Completed, "{:?}", again.error);
    assert!(again.reused_from_cache);
    assert_eq!(again.progress.rate_bytes_per_second, None);
    assert_eq!(std::fs::read(granted.join("again.bin")).unwrap(), body);

    let asked = job(send(&agent, checked(dead, &granted.join("agent.bin"), &sha256)).unwrap());
    let asked = wait_for(&engine, &agent, &asked.job_id, finished);
    assert_eq!(
        asked.state,
        JobState::Failed,
        "an agent must not read the cache"
    );
    assert!(!granted.join("agent.bin").exists());
    assert_eq!(
        code(send(&agent, Command::CacheStatus)),
        code(send(&agent, Command::ListRules))
    );
    assert!(send(&agent, Command::ClearCache).is_err());

    // The quota is clamped to its bounds.
    let mut settings = match send(&user, Command::GetSettings).unwrap() {
        CommandResult::Settings { view } => view.settings,
        other => panic!("{other:?}"),
    };
    settings.cache_quota_bytes = Some(1);
    send(&user, Command::UpdateSettings { settings }).unwrap();
    let status = cache(&user, Command::CacheStatus);
    assert_eq!(status.quota_bytes, status.min_quota_bytes);

    let cleared = cache(&user, Command::ClearCache);
    assert_eq!((cleared.entries, cleared.bytes), (0, 0));
    assert_eq!(
        std::fs::read(granted.join("first.bin")).unwrap(),
        body,
        "saved files stay"
    );
}

fn later_torrent(destination: &str) -> Command {
    Command::CreateJob {
        request: JobRequest::Torrent {
            input: JobInput::Url {
                url: url("magnet:?xt=urn:btih:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
            },
            destination: DestinationIntent {
                path: destination.to_owned(),
                conflict: ConflictPolicy::Ask,
            },
            not_before: Some(Timestamp::from_unix_ms(
                Timestamp::now().unix_ms() + 3_600_000,
            )),
            discover_peers: Some(true),
            upload: false,
        },
    }
}

fn set_default_folder(client: &InProcessClient, folder: Option<&Path>) {
    let mut settings = match send(client, Command::GetSettings).unwrap() {
        CommandResult::Settings { view } => view.settings,
        other => panic!("{other:?}"),
    };
    settings.default_destination_dir = folder.map(|path| path.display().to_string());
    send(client, Command::UpdateSettings { settings }).unwrap();
}

#[test]
fn a_torrent_without_a_destination_uses_the_default_folder_and_rules_like_other_downloads() {
    use fetchpath_protocol::model::{RuleActions, RuleConditions};
    let s = setup(granting);
    // Neither a setting nor a download folder: ask for one, as a file does.
    assert_eq!(
        code(send(&s.user, later_torrent(""))),
        "input.invalid_request"
    );

    set_default_folder(&s.user, Some(&s.outside));
    let automatic = job(send(&s.user, later_torrent("  ")).unwrap());
    assert_eq!(
        automatic.destination.as_deref(),
        Some(s.outside.display().to_string().as_str()),
        "the root, until the helper names the folder"
    );
    let named = job(send(&s.user, later_torrent("Album")).unwrap());
    assert_eq!(
        named.destination.as_deref(),
        Some(s.outside.join("Album").display().to_string().as_str())
    );
    let full = s.root.join("elsewhere").join("Full");
    let explicit = job(send(&s.user, later_torrent(&full.display().to_string())).unwrap());
    assert_eq!(
        explicit.destination.as_deref(),
        Some(full.display().to_string().as_str())
    );

    // A matching rule's folder wins over the default, and a checksum rule
    // does not block a torrent.
    let ruled = s.root.join("ruled");
    add_rule(
        &s.user,
        RuleConditions {
            file_types: vec!["torrent".into()],
            ..RuleConditions::default()
        },
        RuleActions {
            folder: Some(ruled.display().to_string()),
            require_checksum: true,
            ..RuleActions::default()
        },
    )
    .unwrap();
    let mut linked = later_torrent("");
    if let Command::CreateJob {
        request: JobRequest::Torrent { input, .. },
    } = &mut linked
    {
        *input = JobInput::Url {
            url: url("https://example.test/a.torrent"),
        };
    }
    let by_rule = job(send(&s.user, linked).unwrap());
    assert_eq!(
        by_rule.destination.as_deref(),
        Some(ruled.display().to_string().as_str())
    );
}

#[test]
fn an_agents_automatic_torrent_destination_is_checked_against_its_grants() {
    let s = setup(granting);
    // The default folder is outside the grant: wait for the person.
    set_default_folder(&s.user, Some(&s.outside));
    let outside = job(send(&s.agent, later_torrent("")).unwrap());
    assert_eq!(outside.state, JobState::AwaitingApproval);
    assert!(reasons(&outside).contains(&ApprovalReason::OutsideGrantedFolders));
    let bare = job(send(&s.agent, later_torrent("Album")).unwrap());
    assert_eq!(bare.state, JobState::AwaitingApproval);

    // Inside the grant it only waits for the peer approvals every agent
    // torrent needs.
    set_default_folder(&s.user, Some(&s.granted));
    let inside = job(send(&s.agent, later_torrent("")).unwrap());
    assert!(!reasons(&inside).contains(&ApprovalReason::OutsideGrantedFolders));
}

fn torrent_now(destination: &Path, discover_peers: Option<bool>) -> Command {
    let mut command = later_torrent(&destination.display().to_string());
    if let Command::CreateJob {
        request:
            JobRequest::Torrent {
                not_before,
                discover_peers: discover,
                ..
            },
    } = &mut command
    {
        *not_before = None;
        *discover = discover_peers;
    }
    command
}

#[test]
fn a_persons_torrent_discovers_peers_unless_they_turn_it_off() {
    let s = setup(granting);
    for (name, choice) in [("a", None), ("b", Some(true))] {
        let added = send(&s.user, torrent_now(&s.outside.join(name), choice));
        assert!(added.is_ok(), "{choice:?}: {added:?}");
    }
    let refused = send(&s.user, torrent_now(&s.outside.join("c"), Some(false))).unwrap_err();
    assert_eq!(refused.code.as_str(), "policy.discovery_off");
    assert!(refused.message.contains("Turn discovery on"));
    assert!(
        list(&s.user, JobFilter::All)
            .iter()
            .all(|job| { !job.destination.as_deref().unwrap_or("").ends_with('c') })
    );
}

#[test]
fn an_agent_never_gets_peer_discovery_implicitly_and_asking_needs_approval() {
    let s = setup(granting);
    for choice in [None, Some(false)] {
        let refused = send(&s.agent, torrent_now(&s.granted.join("a"), choice)).unwrap_err();
        assert_eq!(refused.code.as_str(), "policy.discovery_off");
        assert!(refused.message.contains("discover_peers"));
    }
    let asked = job(send(&s.agent, torrent_now(&s.granted.join("b"), Some(true))).unwrap());
    assert_eq!(asked.state, JobState::AwaitingApproval);
    assert!(reasons(&asked).contains(&ApprovalReason::PeerDiscovery));
    assert!(!reasons(&asked).contains(&ApprovalReason::PeerUpload));
}

#[test]
fn a_browser_capture_cannot_start_a_torrent() {
    let s = setup(granting);
    let browser = InProcessClient::manual(Arc::clone(&s.engine)).with_principal(Principal::Browser);
    for discover in [None, Some(true)] {
        assert_eq!(
            code(send(&browser, torrent_now(&s.outside.join("t"), discover))),
            "contract.unsupported"
        );
    }
}

/// Moves every saved request's start back by `days` (FP-101 tests only).
fn age_requests(queue: &Path, days: u64) {
    let text = std::fs::read_to_string(queue).unwrap();
    let mut saved: serde_json::Value = serde_json::from_str(&text).unwrap();
    let back = days * 24 * 60 * 60 * 1000;
    for record in saved["records"].as_array_mut().unwrap() {
        if let Some(approval) = record.get_mut("approval").and_then(|a| a.as_object_mut()) {
            let at = approval["requestedAtMs"]
                .as_u64()
                .expect("a start is saved");
            approval.insert("requestedAtMs".into(), (at - back).into());
        }
    }
    std::fs::write(queue, serde_json::to_vec(&saved).unwrap()).unwrap();
}

#[test]
fn a_request_nobody_decides_on_expires_after_seven_days() {
    let s = setup(granting);
    let fresh = job(send(&s.agent, later(&s.outside.join("fresh.bin"))).unwrap());
    let queue = s.queue.clone();
    drop((s.user, s.agent, s.engine));

    // Six days: still waiting.
    age_requests(&queue, 6);
    let engine = open(&queue);
    engine.reconcile();
    let (_, agent) = clients(&engine);
    assert_eq!(get(&agent, &fresh.job_id).state, JobState::AwaitingApproval);
    drop((agent, engine));

    // Eight in all: expired, as a denial ends it, with its own reason.
    age_requests(&queue, 2);
    let engine = open(&queue);
    engine.reconcile();
    let (user, agent) = clients(&engine);
    let expired = get(&agent, &fresh.job_id);
    assert_eq!(expired.state, JobState::Cancelled);
    assert_eq!(
        expired.error.map(|error| error.code.as_str().to_owned()),
        Some("policy.approval_expired".to_owned())
    );
    // Nothing is left for the person to approve.
    assert!(
        send(
            &user,
            Command::ApproveJob {
                job_id: fresh.job_id.clone()
            }
        )
        .is_err()
    );
    drop((user, agent, engine));

    // It stays ended across a restart, and an agent retrying it asks again.
    let engine = open(&queue);
    engine.reconcile();
    let (_, agent) = clients(&engine);
    assert_eq!(get(&agent, &fresh.job_id).state, JobState::Cancelled);
    let retried = job(send(
        &agent,
        Command::Retry {
            job_id: fresh.job_id.clone(),
            expected_sha256: None,
        },
    )
    .unwrap());
    assert_eq!(retried.state, JobState::AwaitingApproval);
}

#[test]
fn a_withdrawn_request_stays_withdrawn_after_a_restart() {
    let s = setup(granting);
    let waiting = job(send(&s.agent, later(&s.outside.join("w.bin"))).unwrap());
    send(
        &s.agent,
        Command::Cancel {
            job_id: waiting.job_id.clone(),
            retain_partial: false,
        },
    )
    .unwrap();
    assert_eq!(get(&s.agent, &waiting.job_id).state, JobState::Cancelled);
    let queue = s.queue.clone();
    drop((s.user, s.agent, s.engine));

    let engine = open(&queue);
    engine.reconcile();
    let (_, agent) = clients(&engine);
    assert_eq!(get(&agent, &waiting.job_id).state, JobState::Cancelled);
}
