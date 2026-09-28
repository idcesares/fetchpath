//! Protocol v1: every message round-trips and matches the checked-in schema;
//! wrong versions, oversize frames and malformed JSON are refused with the
//! contract's codes; nothing secret appears in debug output.

use fetchpath_protocol::command::{
    ConflictPolicy, DestinationDecision, DestinationIntent, JobFilter, PolicyPatch, Schedule,
};
use fetchpath_protocol::frame::{
    FrameDecoder, FrameError, KNOWN_COMMANDS, read_frame, write_frame,
};
use fetchpath_protocol::message::{
    ControlOutcome, Correlation, ProgressKind, ReplyResult, StreamPosition,
};
use fetchpath_protocol::model::{
    CacheView, Confidence, EngineSettings, EngineStatus, IntegrityOutcome, JobDetails, JobKind,
    LinkInspection, LinkKind, MediaInspection, MediaVariant, MediaVariantKind, Phase, Progress,
    QueueStats, Rule, RuleActions, RuleCheck, RuleConditions, RuleSpec, RulesVerdict, Segment,
    SettingsView, Theme, WaitingReason,
};
use fetchpath_protocol::principal::{AgentAccess, ApprovalReason, ApprovalRequest};
use fetchpath_protocol::schema::{protocol_schema, protocol_schema_text};
use fetchpath_protocol::*;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::fmt::Debug;
use std::io::Cursor;
use std::path::PathBuf;
use std::time::Duration;

const SECRET_URL: &str = "https://user:hunter2@files.example.test/a.bin?token=signed-secret#frag";

fn id<T: TryFrom<&'static str, Error = String>>(text: &'static str) -> T {
    T::try_from(text).unwrap()
}

fn job_id() -> JobId {
    id("018f9c2a-525c-7b9a-986c-b0707def18bb")
}

fn at(text: &str) -> Timestamp {
    Timestamp::parse(text).unwrap()
}

fn secret_url() -> SensitiveUrl {
    SensitiveUrl::try_from(SECRET_URL.to_owned()).unwrap()
}

fn destination() -> DestinationIntent {
    DestinationIntent {
        path: "C:\\Users\\person\\Downloads\\a.bin".into(),
        conflict: ConflictPolicy::Ask,
    }
}

fn settings() -> EngineSettings {
    EngineSettings {
        max_active_downloads: 3,
        default_destination_dir: Some("D:\\Downloads".into()),
        auto_retry: true,
        auto_retry_max_attempts: 3,
        auto_retry_base_delay_seconds: 15,
        close_to_tray: true,
        power_mode: false,
        media_tools_dir: None,
        confirm_remove_completed: true,
        theme: Theme::Dark,
        onboarding_completed: true,
        start_engine_at_sign_in: Some(false),
        cache_quota_bytes: None,
    }
}

fn inspection() -> MediaInspection {
    MediaInspection {
        title: "A talk".into(),
        duration_seconds: Some(61.5),
        variants: vec![MediaVariant {
            id: "v1080".into(),
            label: "1080p".into(),
            kind: MediaVariantKind::Video,
            extension: "mp4".into(),
            height: Some(1080),
            fps: Some(30),
        }],
    }
}

fn error() -> ProtocolError {
    let mut error = ProtocolError::new(
        ErrorCode::try_from("storage.destination_conflict".to_owned()).unwrap(),
        ErrorScope::Job,
        "A file already exists at this destination.",
    )
    .with_action(Action::ChooseNewPath);
    error.retry_after_seconds = Some(15);
    error.current_revision = Some(4);
    error
}

fn agent_policy() -> AgentPolicy {
    AgentPolicy {
        folders: vec!["C:\\Users\\person\\Downloads\\agent".into()],
        max_bytes: 104_857_600,
        max_new_jobs_per_hour: 10,
    }
}

fn snapshot() -> JobSnapshot {
    JobSnapshot {
        job_id: job_id(),
        kind: JobKind::File,
        state: JobState::Running,
        waiting_reason: Some(WaitingReason::DestinationConflict),
        job_revision: 3,
        last_seq: 7,
        source_display: "https://…@files.example.test/a.bin?…".into(),
        destination: Some("C:\\Users\\person\\Downloads\\a.bin".into()),
        progress: Progress {
            phase: Some(Phase::Receive),
            bytes_received: 8_388_608,
            bytes_checkpointed: Some(4_194_304),
            bytes_verified: Some(0),
            bytes_total: None,
            rate_bytes_per_second: Some(3_145_728),
            eta_seconds: None,
            confidence: Some(Confidence::Low),
        },
        integrity: Some(IntegrityOutcome::DownloadedObserved),
        expected_sha256: Some("ab".repeat(32)),
        observed_sha256: Some("cd".repeat(32)),
        cleanup_pending: true,
        error: Some(error()),
        attempt: 1,
        retry_at: Some(at("2026-09-20T12:00:30Z")),
        created_at: at("2026-09-20T12:00:00Z"),
        not_before: Some(at("2026-09-20T13:00:00Z")),
        finished_at: Some(at("2026-09-20T12:05:00.125Z")),
        quality_label: Some("1080p".into()),
        principal: Principal::Agent(AgentName::try_from("helper").unwrap()),
        approval: Some(ApprovalRequest {
            reasons: vec![ApprovalReason::SizeLimit, ApprovalReason::RateLimit],
        }),
        reused_from_cache: true,
    }
}

/// One of every command. The match makes adding a command without a sample
/// a compile error.
fn every_command() -> Vec<Command> {
    let job_id = job_id;
    let commands = vec![
        Command::CreateJob {
            request: JobRequest::File {
                input: JobInput::Url { url: secret_url() },
                destination: destination(),
                not_before: Some(at("2026-09-20T13:00:00Z")),
                expected_sha256: Some("ab".repeat(32)),
            },
        },
        Command::CreateJob {
            request: JobRequest::Media {
                input: JobInput::CredentialRef {
                    credential_ref: id("0b7f4a4e-1e1f-4c52-9d4e-6a3b2c1d0e9f"),
                },
                destination: DestinationIntent {
                    conflict: ConflictPolicy::ReplaceExisting,
                    ..destination()
                },
                not_before: None,
                variant_id: "v1080".into(),
                quality_label: "1080p".into(),
            },
        },
        Command::CreateJobs {
            requests: vec![JobRequest::File {
                input: JobInput::Url { url: secret_url() },
                destination: destination(),
                not_before: None,
                expected_sha256: None,
            }],
        },
        Command::Start { job_id: job_id() },
        Command::Pause { job_id: job_id() },
        Command::Resume { job_id: job_id() },
        Command::Cancel {
            job_id: job_id(),
            retain_partial: true,
        },
        Command::Retry {
            job_id: job_id(),
            expected_sha256: None,
        },
        Command::Retry {
            job_id: job_id(),
            expected_sha256: Some("ab".repeat(32)),
        },
        Command::UpdatePolicy {
            job_id: job_id(),
            patch: PolicyPatch {
                schedule: Some(Schedule::At {
                    not_before: at("2026-09-21T08:00:00Z"),
                }),
            },
        },
        Command::UpdatePolicy {
            job_id: job_id(),
            patch: PolicyPatch {
                schedule: Some(Schedule::Now),
            },
        },
        Command::ResolveDestination {
            job_id: job_id(),
            decision: DestinationDecision::ChooseNewPath {
                path: "C:\\Downloads\\a (2).bin".into(),
                expected_sha256: None,
            },
        },
        Command::ResolveDestination {
            job_id: job_id(),
            decision: DestinationDecision::ChooseNewPath {
                path: "C:\\Downloads\\a (3).bin".into(),
                expected_sha256: Some("ab".repeat(32)),
            },
        },
        Command::ResolveDestination {
            job_id: job_id(),
            decision: DestinationDecision::ReplaceExisting,
        },
        Command::ResolveDestination {
            job_id: job_id(),
            decision: DestinationDecision::Cancel,
        },
        Command::SelectMedia {
            job_id: job_id(),
            selection_id: "v1080".into(),
        },
        Command::RefreshSource {
            job_id: job_id(),
            destination: None,
            source: JobInput::Url { url: secret_url() },
            expected_sha256: None,
        },
        Command::RefreshSource {
            job_id: job_id(),
            destination: Some(r"D:\Downloads\renamed.bin".into()),
            source: JobInput::Url { url: secret_url() },
            expected_sha256: Some(String::new()),
        },
        Command::RefreshMediaChoices { job_id: job_id() },
        Command::RemoveJob { job_id: job_id() },
        Command::ListJobs {
            filter: JobFilter::Active,
        },
        Command::GetJob { job_id: job_id() },
        Command::JobDetails { job_id: job_id() },
        Command::InspectMedia { url: secret_url() },
        Command::InspectLink { url: secret_url() },
        Command::QueueStats,
        Command::History {
            query: Some("archive".into()),
            limit: Some(50),
        },
        Command::GetSettings,
        Command::TakeLinkReviews,
        Command::TakeBrowserCaptures,
        Command::UpdateSettings {
            settings: settings(),
        },
        Command::ApproveJob { job_id: job_id() },
        Command::DenyJob { job_id: job_id() },
        Command::GetAgentPolicies,
        Command::SetAgentPolicy {
            agent: AgentName::try_from("claude-code").unwrap(),
            policy: Some(agent_policy()),
        },
        Command::SetAgentPolicy {
            agent: AgentName::try_from("claude-code").unwrap(),
            policy: None,
        },
        Command::ListRules,
        Command::AddRule {
            rule: Box::new(rule().spec),
            position: Some(1),
        },
        Command::RemoveRule { rule_id: 3 },
        Command::CacheStatus,
        Command::ClearCache,
        Command::SubscribeJob {
            job_id: job_id(),
            after_seq: 7,
        },
        Command::SubscribeQueue { after_cursor: 120 },
        Command::EngineStatus,
        Command::EngineShutdown,
    ];
    for command in &commands {
        match command {
            Command::CreateJob { .. }
            | Command::CreateJobs { .. }
            | Command::Start { .. }
            | Command::Pause { .. }
            | Command::Resume { .. }
            | Command::Cancel { .. }
            | Command::Retry { .. }
            | Command::UpdatePolicy { .. }
            | Command::ResolveDestination { .. }
            | Command::SelectMedia { .. }
            | Command::RefreshSource { .. }
            | Command::RefreshMediaChoices { .. }
            | Command::RemoveJob { .. }
            | Command::ListJobs { .. }
            | Command::GetJob { .. }
            | Command::JobDetails { .. }
            | Command::InspectMedia { .. }
            | Command::InspectLink { .. }
            | Command::QueueStats
            | Command::History { .. }
            | Command::GetSettings
            | Command::TakeLinkReviews
            | Command::TakeBrowserCaptures
            | Command::UpdateSettings { .. }
            | Command::ApproveJob { .. }
            | Command::DenyJob { .. }
            | Command::GetAgentPolicies
            | Command::SetAgentPolicy { .. }
            | Command::ListRules
            | Command::AddRule { .. }
            | Command::RemoveRule { .. }
            | Command::CacheStatus
            | Command::ClearCache
            | Command::SubscribeJob { .. }
            | Command::SubscribeQueue { .. }
            | Command::EngineStatus
            | Command::EngineShutdown => {}
        }
    }
    commands
}

fn rule() -> Rule {
    Rule {
        id: 3,
        spec: RuleSpec {
            name: Some("Disc images".into()),
            when: RuleConditions {
                domains: vec!["example.com".into()],
                file_types: vec!["iso".into()],
                min_size_bytes: Some(1 << 30),
                max_size_bytes: None,
            },
            then: RuleActions {
                folder: Some(r"D:\ISOs".into()),
                media_quality: None,
                require_checksum: true,
                max_connections: Some(2),
            },
        },
    }
}

fn every_result() -> Vec<CommandResult> {
    let results = vec![
        CommandResult::Job { job: snapshot() },
        CommandResult::Control {
            outcome: ControlOutcome::TooLateToCancel,
            job: snapshot(),
        },
        CommandResult::Jobs {
            jobs: vec![snapshot(), snapshot()],
        },
        CommandResult::Details {
            details: JobDetails {
                job: snapshot(),
                segments: vec![Segment {
                    start: 0,
                    end: 1_048_575,
                    received: 65_536,
                }],
            },
        },
        CommandResult::MediaInspection {
            inspection: inspection(),
        },
        CommandResult::LinkInspection {
            inspection: LinkInspection {
                kind: LinkKind::File,
                file_name: Some("ubuntu.iso".into()),
                content_type: Some("application/octet-stream".into()),
                size_bytes: Some(6_000_000_000),
                resumable: true,
                rules: Some(RulesVerdict {
                    matched: Some(rule()),
                    checks: vec![RuleCheck {
                        rule_id: 3,
                        matched: true,
                        reasons: vec!["the file is a .iso".into()],
                    }],
                }),
            },
        },
        CommandResult::Rules {
            rules: vec![rule()],
        },
        CommandResult::QueueStats {
            stats: QueueStats {
                running: 1,
                queued: 2,
                scheduled: 1,
                paused: 0,
                completed: 5,
                failed: 1,
                active_bytes: 10,
                completed_bytes: 20,
                combined_bytes_per_second: 30,
                max_active_downloads: 3,
            },
        },
        CommandResult::Settings {
            view: SettingsView {
                settings: settings(),
                repaired: false,
                max_active_limit: 8,
                max_retry_attempts: 10,
            },
        },
        CommandResult::Removed { job_id: job_id() },
        CommandResult::LinkReviews {
            urls: vec![secret_url()],
        },
        CommandResult::Subscribed {
            position: StreamPosition::Job {
                job_id: job_id(),
                after_seq: 7,
            },
        },
        CommandResult::SnapshotBoundary {
            jobs: vec![snapshot()],
            position: StreamPosition::Queue { after_cursor: 120 },
        },
        CommandResult::EngineStatus {
            status: EngineStatus {
                engine_version: "0.2.0".into(),
                schema_version: SCHEMA_VERSION,
                started_at: at("2026-09-20T11:00:00Z"),
                connected_clients: 2,
                active_jobs: 1,
                queue_cursor: 120,
                queue_read_only: None,
            },
        },
        CommandResult::EngineStatus {
            status: EngineStatus {
                engine_version: "0.2.0".into(),
                schema_version: SCHEMA_VERSION,
                started_at: at("2026-09-20T11:00:00Z"),
                connected_clients: 0,
                active_jobs: 0,
                queue_cursor: 0,
                queue_read_only: Some(
                    ProtocolError::new(
                        ErrorCode::try_from("storage.queue_from_newer_version".to_owned()).unwrap(),
                        ErrorScope::Engine,
                        "Your download list was saved by a newer version of Fetchpath.",
                    )
                    .with_action(Action::UpdateSoftware),
                ),
            },
        },
        CommandResult::AgentPolicies {
            policies: vec![AgentAccess {
                agent: AgentName::try_from("claude-code").unwrap(),
                policy: agent_policy(),
            }],
        },
        CommandResult::Cache {
            cache: CacheView {
                bytes: 3_145_728,
                entries: 2,
                quota_bytes: 2 << 30,
                min_quota_bytes: 256 << 20,
                max_quota_bytes: 256 << 30,
            },
        },
        CommandResult::ShuttingDown,
        CommandResult::CapturesTaken,
    ];
    for result in &results {
        match result {
            CommandResult::Rules { .. }
            | CommandResult::Job { .. }
            | CommandResult::Control { .. }
            | CommandResult::Jobs { .. }
            | CommandResult::Details { .. }
            | CommandResult::MediaInspection { .. }
            | CommandResult::LinkInspection { .. }
            | CommandResult::QueueStats { .. }
            | CommandResult::Settings { .. }
            | CommandResult::Removed { .. }
            | CommandResult::LinkReviews { .. }
            | CommandResult::Subscribed { .. }
            | CommandResult::SnapshotBoundary { .. }
            | CommandResult::EngineStatus { .. }
            | CommandResult::AgentPolicies { .. }
            | CommandResult::Cache { .. }
            | CommandResult::ShuttingDown
            | CommandResult::CapturesTaken => {}
        }
    }
    results
}

fn every_event_payload() -> Vec<EventPayload> {
    let payloads = vec![
        EventPayload::JobCreated { job: snapshot() },
        EventPayload::StateChanged {
            previous: JobState::Running,
            state: JobState::WaitingForSource,
            waiting_reason: Some(WaitingReason::SourceExpired),
        },
        EventPayload::PolicyChanged {
            not_before: Some(at("2026-09-21T08:00:00Z")),
        },
        EventPayload::SourceChanged {
            source_display: "https://files.example.test/a.bin?…".into(),
        },
        EventPayload::MediaChoicesReady {
            inspection: inspection(),
        },
        EventPayload::CheckpointCommitted {
            bytes_checkpointed: 4_194_304,
        },
        EventPayload::IntegrityChanged {
            integrity: IntegrityOutcome::VerifiedExpected,
            observed_sha256: Some("ab".repeat(32)),
        },
        EventPayload::Waiting {
            reason: WaitingReason::MediaSelection,
        },
        EventPayload::Warning {
            message_key: "source.range_ignored".into(),
            message: "The server ignored the range request; restarting from zero.".into(),
        },
        EventPayload::ErrorRecorded { error: error() },
        EventPayload::PublicationCompleted {
            destination: Some("C:\\Downloads\\a.bin".into()),
            observed_sha256: Some("cd".repeat(32)),
        },
        EventPayload::JobRemoved,
    ];
    for payload in &payloads {
        match payload {
            EventPayload::JobCreated { .. }
            | EventPayload::StateChanged { .. }
            | EventPayload::PolicyChanged { .. }
            | EventPayload::SourceChanged { .. }
            | EventPayload::MediaChoicesReady { .. }
            | EventPayload::CheckpointCommitted { .. }
            | EventPayload::IntegrityChanged { .. }
            | EventPayload::Waiting { .. }
            | EventPayload::Warning { .. }
            | EventPayload::ErrorRecorded { .. }
            | EventPayload::PublicationCompleted { .. }
            | EventPayload::JobRemoved => {}
            // Only ever produced by reading a newer engine; covered below.
            EventPayload::Unknown => unreachable!(),
        }
    }
    payloads
}

fn client_id() -> ClientId {
    id("018f9c2a-0d55-74cc-b6c0-7cc8b1c9f221")
}

fn envelope(payload: Command) -> CommandEnvelope {
    CommandEnvelope {
        schema_version: SCHEMA_VERSION,
        client_id: client_id(),
        command_id: id("018f9c2a-2f70-7b42-8d6f-5bcb546e11aa"),
        issued_at: at("2026-09-20T12:00:00Z"),
        expected_revision: Some(3),
        payload,
    }
}

fn event(payload: EventPayload) -> JobEvent {
    JobEvent {
        schema_version: SCHEMA_VERSION,
        job_id: job_id(),
        seq: 8,
        cursor: 121,
        job_revision: 3,
        occurred_at: at("2026-09-20T12:00:04Z"),
        payload,
        correlation: Correlation {
            command_id: Some(id("018f9c2a-2f70-7b42-8d6f-5bcb546e11aa")),
            attempt_id: Some(id("5d0c1a2b-3c4d-4e5f-8a9b-0c1d2e3f4a5b")),
        },
    }
}

fn progress() -> ProgressSample {
    ProgressSample {
        schema_version: SCHEMA_VERSION,
        job_id: job_id(),
        sample_cursor: 41,
        job_revision: 3,
        occurred_at: at("2026-09-20T12:00:04Z"),
        kind: ProgressKind::ProgressSampled,
        public_payload: snapshot().progress,
        correlation: Correlation::default(),
    }
}

fn every_server_message() -> Vec<ServerMessage> {
    let mut messages: Vec<ServerMessage> = every_result()
        .into_iter()
        .map(|result| {
            ServerMessage::Reply(Reply::ok(
                id("018f9c2a-2f70-7b42-8d6f-5bcb546e11aa"),
                result,
            ))
        })
        .collect();
    messages.push(ServerMessage::Reply(Reply::error(None, error())));
    messages.extend(
        every_event_payload()
            .into_iter()
            .map(|payload| ServerMessage::Event(event(payload))),
    );
    messages.push(ServerMessage::Progress(progress()));
    messages
}

fn round_trip<T: Serialize + DeserializeOwned + PartialEq + Debug>(value: &T) -> Value {
    let json = serde_json::to_value(value).unwrap();
    let back: T = serde_json::from_value(json.clone()).unwrap();
    assert_eq!(&back, value);
    assert_eq!(
        serde_json::to_value(&back).unwrap(),
        json,
        "re-serialization changed"
    );
    json
}

#[test]
fn every_command_round_trips_and_every_known_name_has_a_sample() {
    let mut names = BTreeSet::new();
    for command in every_command() {
        let message = envelope(command);
        let json = round_trip(&message);
        assert_eq!(json["payload"]["type"], message.payload.name());
        names.insert(message.payload.name());
        let frame = encode_frame(&message).unwrap();
        assert_eq!(decode_command(&frame[4..]).unwrap(), message);
    }
    let known: BTreeSet<&str> = KNOWN_COMMANDS.iter().copied().collect();
    assert_eq!(names, known, "KNOWN_COMMANDS and the Command enum disagree");
}

#[test]
fn every_server_message_round_trips_through_a_frame() {
    let kinds: BTreeSet<String> = every_event_payload()
        .iter()
        .map(|payload| {
            serde_json::to_value(payload).unwrap()["kind"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect();
    let known: BTreeSet<String> = fetchpath_protocol::message::KNOWN_EVENT_KINDS
        .iter()
        .map(|kind| (*kind).to_owned())
        .collect();
    assert_eq!(kinds, known, "KNOWN_EVENT_KINDS and EventPayload disagree");
    for message in every_server_message() {
        round_trip(&message);
        let frame = encode_frame(&message).unwrap();
        assert_eq!(decode_server_message(&frame[4..]).unwrap(), message);
    }
}

#[test]
fn every_message_matches_the_exported_schema() {
    let schema = protocol_schema();
    for command in every_command() {
        let json = serde_json::to_value(envelope(command)).unwrap();
        validate(&schema, &schema, &json, "$").unwrap_or_else(|error| panic!("{error}\n{json:#}"));
    }
    for message in every_server_message() {
        let json = serde_json::to_value(message).unwrap();
        validate(&schema, &schema, &json, "$").unwrap_or_else(|error| panic!("{error}\n{json:#}"));
    }
    // The validator is not vacuous: a wrong shape is caught.
    let mut wrong = serde_json::to_value(envelope(Command::QueueStats)).unwrap();
    wrong["client_id"] = json!(42);
    assert!(validate(&schema, &schema, &wrong, "$").is_err());
    let mut unknown_state = serde_json::to_value(ServerMessage::Reply(Reply::ok(
        id("018f9c2a-2f70-7b42-8d6f-5bcb546e11aa"),
        CommandResult::Job { job: snapshot() },
    )))
    .unwrap();
    unknown_state["result"]["value"]["job"]["state"] = json!(17);
    assert!(validate(&schema, &schema, &unknown_state, "$").is_err());
}

#[test]
fn the_checked_in_schema_is_current() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("schema/protocol-v1.schema.json");
    let generated = protocol_schema_text();
    if std::env::var_os("FETCHPATH_BLESS_SCHEMA").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &generated).unwrap();
    }
    let checked_in = std::fs::read_to_string(&path)
        .unwrap_or_default()
        .replace("\r\n", "\n");
    assert!(
        checked_in == generated,
        "{} is out of date; regenerate it with FETCHPATH_BLESS_SCHEMA=1 cargo test -p fetchpath-protocol and review the diff",
        path.display()
    );
}

#[test]
fn the_contract_progress_example_is_a_valid_progress_sample() {
    // docs/architecture/JOB-CONTRACT.md §12, verbatim.
    let example = r#"{
      "schema_version": 1,
      "job_id": "018f9c2a-525c-7b9a-986c-b0707def18bb",
      "sample_cursor": 41,
      "job_revision": 3,
      "occurred_at": "2026-09-20T12:00:04Z",
      "kind": "progress_sampled",
      "public_payload": {
        "phase": "receive",
        "bytes_received": 8388608,
        "bytes_checkpointed": 4194304,
        "bytes_verified": 0,
        "bytes_total": null,
        "rate_bytes_per_second": 3145728,
        "eta_seconds": null,
        "confidence": "low"
      },
      "correlation": { "command_id": null, "attempt_id": null }
    }"#;
    let sample: ProgressSample = serde_json::from_str(example).unwrap();
    assert_eq!(sample, progress());
}

#[test]
fn the_contract_create_job_example_decodes() {
    // docs/architecture/JOB-CONTRACT.md §12, the v1 wire form.
    let example = r#"{
      "schema_version": 1,
      "client_id": "018f9c2a-0d55-74cc-b6c0-7cc8b1c9f221",
      "command_id": "018f9c2a-2f70-7b42-8d6f-5bcb546e11aa",
      "issued_at": "2026-09-20T12:00:00Z",
      "payload": {
        "type": "CreateJob",
        "request": {
          "kind": "file",
          "input": { "type": "url", "url": "https://example.test/file.bin" },
          "destination": { "path": "C:\\Users\\person\\Downloads\\file.bin", "conflict": "ask" }
        }
      }
    }"#;
    let envelope = decode_command(example.as_bytes()).unwrap();
    assert_eq!(envelope.client_id, client_id());
    assert!(matches!(
        envelope.payload,
        Command::CreateJob {
            request: JobRequest::File {
                expected_sha256: None,
                ..
            }
        }
    ));
}

fn command_json() -> Value {
    serde_json::to_value(envelope(Command::Pause { job_id: job_id() })).unwrap()
}

#[test]
fn another_major_version_is_refused_with_its_command_id() {
    for (version, expected) in [
        (json!(2), ErrorCode::UNSUPPORTED_VERSION),
        (json!(0), ErrorCode::UNSUPPORTED_VERSION),
        (json!(u64::MAX), ErrorCode::UNSUPPORTED_VERSION),
        (json!("1"), ErrorCode::MALFORMED_MESSAGE),
        (Value::Null, ErrorCode::MALFORMED_MESSAGE),
    ] {
        let mut message = command_json();
        message["schema_version"] = version.clone();
        let rejected = decode_command(&serde_json::to_vec(&message).unwrap()).unwrap_err();
        assert_eq!(rejected.error.code, expected, "{version}");
        assert_eq!(
            rejected.command_id.as_ref().map(CommandId::as_str),
            Some("018f9c2a-2f70-7b42-8d6f-5bcb546e11aa")
        );
    }
    let mut reply = serde_json::to_value(ServerMessage::Progress(progress())).unwrap();
    reply["schema_version"] = json!(2);
    let error = decode_server_message(&serde_json::to_vec(&reply).unwrap()).unwrap_err();
    assert_eq!(error.code, ErrorCode::UNSUPPORTED_VERSION);
    assert_eq!(error.action, Some(Action::UpdateSoftware));
    // A different major may reshape anything else, but still answers with a
    // readable reply.
    let reply = fetchpath_protocol::frame::Rejected {
        command_id: None,
        error: ProtocolError::unsupported_version(2),
    }
    .into_reply();
    assert!(
        matches!(reply.result, ReplyResult::Error(ref e) if e.code == ErrorCode::UNSUPPORTED_VERSION)
    );
}

#[test]
fn malformed_json_and_unknown_commands_are_refused_with_contract_codes() {
    for body in [
        &b"{"[..],
        b"[]",
        b"\"text\"",
        b"\xff\xfe",
        b"{\"schema_version\":1}",
    ] {
        let rejected = decode_command(body).unwrap_err();
        assert_eq!(
            rejected.error.code,
            ErrorCode::MALFORMED_MESSAGE,
            "{body:?}"
        );
    }

    let mut unknown = command_json();
    unknown["payload"] = json!({ "type": "QuarantineJob", "job_id": job_id() });
    let rejected = decode_command(&serde_json::to_vec(&unknown).unwrap()).unwrap_err();
    assert_eq!(rejected.error.code, ErrorCode::UNKNOWN_COMMAND);
    assert!(rejected.command_id.is_some());

    // A known command with a wrong field is malformed, not unknown.
    let mut wrong = command_json();
    wrong["payload"]["job_id"] = json!("not-a-uuid");
    let rejected = decode_command(&serde_json::to_vec(&wrong).unwrap()).unwrap_err();
    assert_eq!(rejected.error.code, ErrorCode::MALFORMED_MESSAGE);
    assert!(rejected.command_id.is_some());

    let mut bad_id = command_json();
    bad_id["command_id"] = json!("NOT-LOWERCASE");
    let rejected = decode_command(&serde_json::to_vec(&bad_id).unwrap()).unwrap_err();
    assert_eq!(rejected.error.code, ErrorCode::MALFORMED_MESSAGE);
    assert_eq!(rejected.command_id, None);
}

#[test]
fn oversize_empty_and_truncated_frames_are_refused_before_allocation() {
    let oversize = ((MAX_FRAME_BYTES + 1) as u32).to_le_bytes();
    match read_frame(&mut Cursor::new(oversize.to_vec())) {
        Err(FrameError::Protocol(error)) => assert_eq!(error.code, ErrorCode::MESSAGE_TOO_LARGE),
        other => panic!("{other:?}"),
    }
    match read_frame(&mut Cursor::new(vec![0, 0, 0, 0])) {
        Err(FrameError::Protocol(error)) => assert_eq!(error.code, ErrorCode::MALFORMED_MESSAGE),
        other => panic!("{other:?}"),
    }
    let mut truncated = encode_frame(&command_json()).unwrap();
    truncated.truncate(truncated.len() - 1);
    assert!(matches!(
        read_frame(&mut Cursor::new(truncated)),
        Err(FrameError::Io(_))
    ));
    assert!(matches!(
        read_frame(&mut Cursor::new(vec![9, 0])),
        Err(FrameError::Io(_))
    ));
    assert!(read_frame(&mut Cursor::new(Vec::new())).unwrap().is_none());

    let big = "x".repeat(MAX_FRAME_BYTES);
    assert_eq!(
        encode_frame(&big).unwrap_err().code,
        ErrorCode::MESSAGE_TOO_LARGE
    );

    let mut decoder = FrameDecoder::new();
    let error = decoder.push(&u32::MAX.to_le_bytes()).unwrap_err();
    assert_eq!(error.code, ErrorCode::MESSAGE_TOO_LARGE);
    assert_eq!(
        decoder.pending(),
        0,
        "an oversize body must not be buffered"
    );
    assert!(
        decoder.push(b"more").is_err(),
        "a failed stream stays failed"
    );
}

#[test]
fn frames_split_anywhere_reassemble_in_order() {
    let messages: Vec<CommandEnvelope> = every_command().into_iter().map(envelope).collect();
    let mut stream = Vec::new();
    for message in &messages {
        write_frame(&mut stream, message).unwrap();
    }
    for chunk in [1, 3, 7, 64, stream.len()] {
        let mut decoder = FrameDecoder::new();
        let mut decoded = Vec::new();
        for piece in stream.chunks(chunk) {
            decoder.push(piece).unwrap();
            while let Some(body) = decoder.next_frame().unwrap() {
                decoded.push(decode_command(&body).unwrap());
            }
        }
        assert_eq!(decoded, messages, "chunk size {chunk}");
        assert_eq!(decoder.pending(), 0);
    }
    let mut reader = Cursor::new(stream);
    let mut count = 0;
    while let Some(body) = read_frame(&mut reader).unwrap() {
        assert_eq!(decode_command(&body).unwrap(), messages[count]);
        count += 1;
    }
    assert_eq!(count, messages.len());
}

#[test]
fn secrets_in_links_never_reach_debug_output() {
    let url = secret_url();
    assert_eq!(url.expose(), SECRET_URL);
    assert_eq!(url.display(), "https://…@files.example.test/a.bin?…");
    for command in every_command() {
        let shown = format!("{:?}", envelope(command));
        for secret in ["hunter2", "signed-secret", "frag", "user:"] {
            assert!(!shown.contains(secret), "{secret} leaked: {shown}");
        }
    }
    assert!(SensitiveUrl::try_from(String::new()).is_err());
    assert!(SensitiveUrl::try_from("https://a.test/\u{0}".to_owned()).is_err());
    assert!(SensitiveUrl::try_from(format!("https://a.test/{}", "a".repeat(9_000))).is_err());
    assert_eq!(
        SensitiveUrl::try_from("https://example.test/plain.bin".to_owned())
            .unwrap()
            .display(),
        "https://example.test/plain.bin"
    );
}

#[test]
fn a_newer_engine_adds_fields_states_and_event_kinds_without_breaking_this_client() {
    let mut reply = serde_json::to_value(ServerMessage::Reply(Reply::ok(
        id("018f9c2a-2f70-7b42-8d6f-5bcb546e11aa"),
        CommandResult::Job { job: snapshot() },
    )))
    .unwrap();
    reply["future_field"] = json!({ "nested": true });
    reply["result"]["value"]["job"]["state"] = json!("quarantined");
    reply["result"]["value"]["job"]["error"]["action"] = json!("scan_in_terminal");
    reply["result"]["value"]["job"]["origin_device"] = json!("peer:laptop");
    let ServerMessage::Reply(reply) =
        decode_server_message(&serde_json::to_vec(&reply).unwrap()).unwrap()
    else {
        panic!("not a reply");
    };
    let CommandResult::Job { job } = reply.into_result().unwrap() else {
        panic!("not a job");
    };
    assert_eq!(job.state, JobState::Unknown);
    assert_eq!(job.error.unwrap().action, Some(Action::Unknown));

    let mut future_event =
        serde_json::to_value(ServerMessage::Event(event(EventPayload::JobRemoved))).unwrap();
    future_event["kind"] = json!("quarantine_requested");
    future_event["public_payload"] = json!({ "device": "peer:laptop", "reason": "scan" });
    let ServerMessage::Event(decoded) =
        decode_server_message(&serde_json::to_vec(&future_event).unwrap()).unwrap()
    else {
        panic!("not an event");
    };
    assert_eq!(decoded.payload, EventPayload::Unknown);
    assert_eq!(
        decoded.seq, 8,
        "an unknown kind still counts in the sequence"
    );
}

#[test]
fn only_queries_skip_the_command_ledger() {
    let queries: BTreeSet<&str> = every_command()
        .iter()
        .filter(|command| !command.is_mutating())
        .map(Command::name)
        .collect();
    let expected: BTreeSet<&str> = [
        "ListJobs",
        "GetJob",
        "JobDetails",
        "InspectMedia",
        "InspectLink",
        "QueueStats",
        "History",
        "GetSettings",
        "TakeLinkReviews",
        "TakeBrowserCaptures",
        "GetAgentPolicies",
        "ListRules",
        "CacheStatus",
        "SubscribeJob",
        "SubscribeQueue",
        "EngineStatus",
    ]
    .into_iter()
    .collect();
    assert_eq!(queries, expected);
}

/// A client written against the trait works with any implementation.
struct Recording {
    sent: std::sync::Mutex<Vec<CommandEnvelope>>,
}

impl EngineClient for Recording {
    fn execute(&self, envelope: &CommandEnvelope) -> Result<CommandResult, ProtocolError> {
        self.sent.lock().unwrap().push(envelope.clone());
        Ok(CommandResult::ShuttingDown)
    }

    fn subscribe(&self, _: &CommandEnvelope) -> Result<Subscription, ProtocolError> {
        struct Empty;
        impl EventStream for Empty {
            fn next_item(&mut self, _: Duration) -> Result<Option<StreamItem>, ProtocolError> {
                Ok(None)
            }
        }
        Ok(Subscription {
            start: CommandResult::Subscribed {
                position: StreamPosition::Queue { after_cursor: 0 },
            },
            events: Box::new(Empty),
        })
    }
}

#[test]
fn engine_client_is_object_safe_and_send_stamps_a_fresh_envelope() {
    let recording = Recording {
        sent: std::sync::Mutex::new(Vec::new()),
    };
    let client: &dyn EngineClient = &recording;
    client.send(&client_id(), Command::EngineShutdown).unwrap();
    client.send(&client_id(), Command::EngineShutdown).unwrap();
    let mut subscription = client
        .subscribe(&envelope(Command::SubscribeQueue { after_cursor: 0 }))
        .unwrap();
    assert!(
        subscription
            .events
            .next_item(Duration::ZERO)
            .unwrap()
            .is_none()
    );
    let sent = recording.sent.lock().unwrap();
    assert_eq!(sent.len(), 2);
    assert_ne!(sent[0].command_id, sent[1].command_id);
    assert_eq!(sent[0].schema_version, SCHEMA_VERSION);
    assert_eq!(sent[0].client_id, client_id());
}

// A small JSON Schema checker for the keywords schemars emits. An unexpected
// keyword fails the test, so the checker cannot silently skip a constraint.
fn validate(root: &Value, schema: &Value, value: &Value, path: &str) -> Result<(), String> {
    let object = match schema {
        Value::Bool(true) => return Ok(()),
        Value::Bool(false) => return Err(format!("{path}: nothing is allowed here")),
        Value::Object(object) => object,
        other => return Err(format!("{path}: bad schema {other}")),
    };
    for (keyword, rule) in object {
        match keyword.as_str() {
            "$ref" => {
                let name = rule
                    .as_str()
                    .and_then(|text| text.strip_prefix("#/$defs/"))
                    .ok_or_else(|| format!("{path}: unsupported $ref {rule}"))?;
                let target = root["$defs"]
                    .get(name)
                    .ok_or_else(|| format!("{path}: missing definition {name}"))?;
                validate(root, target, value, path)?;
            }
            "type" => {
                let allowed: Vec<&str> = match rule {
                    Value::String(one) => vec![one.as_str()],
                    Value::Array(many) => many.iter().filter_map(Value::as_str).collect(),
                    _ => return Err(format!("{path}: bad type {rule}")),
                };
                let matches = allowed.iter().any(|kind| match *kind {
                    "object" => value.is_object(),
                    "array" => value.is_array(),
                    "string" => value.is_string(),
                    "boolean" => value.is_boolean(),
                    "null" => value.is_null(),
                    "integer" => value.is_u64() || value.is_i64(),
                    "number" => value.is_number(),
                    _ => false,
                });
                if !matches {
                    return Err(format!("{path}: {value} is not {allowed:?}"));
                }
            }
            "properties" => {
                if let (Some(properties), Some(actual)) = (rule.as_object(), value.as_object()) {
                    for (name, property) in properties {
                        if let Some(field) = actual.get(name) {
                            validate(root, property, field, &format!("{path}.{name}"))?;
                        }
                    }
                }
            }
            "required" => {
                if let Some(actual) = value.as_object() {
                    for name in rule
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                    {
                        if !actual.contains_key(name) {
                            return Err(format!("{path}: missing {name}"));
                        }
                    }
                }
            }
            "additionalProperties" => {
                if let (Some(actual), Some(properties)) = (
                    value.as_object(),
                    object.get("properties").and_then(Value::as_object),
                ) {
                    for (name, field) in actual {
                        if !properties.contains_key(name) {
                            validate(root, rule, field, &format!("{path}.{name}"))?;
                        }
                    }
                }
            }
            "items" => {
                for (index, item) in value.as_array().into_iter().flatten().enumerate() {
                    validate(root, rule, item, &format!("{path}[{index}]"))?;
                }
            }
            "enum" => {
                if !rule
                    .as_array()
                    .is_some_and(|options| options.contains(value))
                {
                    return Err(format!("{path}: {value} is not one of {rule}"));
                }
            }
            "const" => {
                if rule != value {
                    return Err(format!("{path}: {value} is not {rule}"));
                }
            }
            "oneOf" | "anyOf" => {
                let passing = rule
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|option| validate(root, option, value, path).is_ok())
                    .count();
                if passing == 0 || (keyword == "oneOf" && passing > 1) {
                    return Err(format!(
                        "{path}: {passing} of the {keyword} options match {value}"
                    ));
                }
            }
            "allOf" => {
                for option in rule.as_array().into_iter().flatten() {
                    validate(root, option, value, path)?;
                }
            }
            "minimum" => {
                if let (Some(limit), Some(number)) = (rule.as_f64(), value.as_f64())
                    && number < limit
                {
                    return Err(format!("{path}: {number} < {limit}"));
                }
            }
            "maximum" => {
                if let (Some(limit), Some(number)) = (rule.as_f64(), value.as_f64())
                    && number > limit
                {
                    return Err(format!("{path}: {number} > {limit}"));
                }
            }
            "minLength" | "maxLength" => {
                if let (Some(limit), Some(text)) = (rule.as_u64(), value.as_str()) {
                    let length = text.chars().count() as u64;
                    if (keyword == "minLength" && length < limit)
                        || (keyword == "maxLength" && length > limit)
                    {
                        return Err(format!("{path}: length {length} breaks {keyword} {limit}"));
                    }
                }
            }
            // Annotations, and string formats checked by the types themselves.
            "description" | "title" | "format" | "pattern" | "default" | "$schema" | "$defs"
            | "examples" | "deprecated" => {}
            other if other.starts_with("x-") => {}
            other => return Err(format!("{path}: the checker does not know {other}")),
        }
    }
    Ok(())
}
