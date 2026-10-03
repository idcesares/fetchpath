//! Instance identity (contract D6, FP-101 gate G1): an engine with an
//! identity refuses a change that does not name it and any command naming
//! another instance, and keeps its id when the person renames it.

use fetchpath_protocol::command::{Command, CommandEnvelope};
use fetchpath_protocol::message::CommandResult;
use fetchpath_protocol::model::EngineStatus;
use fetchpath_protocol::{ClientId, EngineClient, InstanceId, ProtocolError};
use fetchpath_session::Session;
use fetchpath_session::engine::{Engine, InProcessClient};
use std::path::Path;
use std::sync::Arc;

fn open(dir: &Path, instance: &InstanceId) -> InProcessClient {
    let session = Arc::new(Session::load_with_browser(dir.join("queue-v1.json"), 3, None).unwrap());
    InProcessClient::manual(Engine::with_instance(session, instance.clone()))
}

fn run(
    engine: &InProcessClient,
    instance: Option<&InstanceId>,
    command: Command,
) -> Result<CommandResult, ProtocolError> {
    let mut envelope = CommandEnvelope::new(ClientId::random(), command);
    envelope.expected_instance_id = instance.cloned();
    engine.execute(&envelope)
}

fn status(engine: &InProcessClient) -> EngineStatus {
    match run(engine, None, Command::EngineStatus).unwrap() {
        CommandResult::EngineStatus { status } => status,
        other => panic!("{other:?}"),
    }
}

fn renamed(engine: &InProcessClient, instance: &InstanceId, name: &str) {
    let mut settings = match run(engine, None, Command::GetSettings).unwrap() {
        CommandResult::Settings { view } => view.settings,
        other => panic!("{other:?}"),
    };
    settings.instance_name = Some(name.to_owned());
    run(engine, Some(instance), Command::UpdateSettings { settings }).unwrap();
}

/// A change that always succeeds: the current settings, saved unchanged.
fn change(engine: &InProcessClient) -> Command {
    match run(engine, None, Command::GetSettings).unwrap() {
        CommandResult::Settings { view } => Command::UpdateSettings {
            settings: view.settings,
        },
        other => panic!("{other:?}"),
    }
}

fn refused_as_wrong_instance(result: Result<CommandResult, ProtocolError>) {
    let error = result.unwrap_err();
    assert_eq!(error.code.as_str(), "contract.wrong_instance");
    assert_eq!(
        error.action,
        Some(fetchpath_protocol::Action::SelectInstance)
    );
}

#[test]
fn a_change_must_name_this_instance() {
    let dir = tempfile::tempdir().unwrap();
    let own = InstanceId::random();
    let engine = open(dir.path(), &own);

    refused_as_wrong_instance(run(&engine, None, change(&engine)));
    refused_as_wrong_instance(run(&engine, Some(&InstanceId::random()), change(&engine)));
    run(&engine, Some(&own), change(&engine)).unwrap();
}

#[test]
fn sharing_and_pairing_changes_must_name_it_too() {
    let dir = tempfile::tempdir().unwrap();
    let engine = open(dir.path(), &InstanceId::random());
    refused_as_wrong_instance(run(&engine, None, Command::SetLanSharing { enabled: true }));
    refused_as_wrong_instance(run(&engine, None, Command::StartPairing));
}

#[test]
fn reads_may_leave_it_out_but_not_name_another() {
    let dir = tempfile::tempdir().unwrap();
    let own = InstanceId::random();
    let engine = open(dir.path(), &own);

    run(&engine, None, Command::QueueStats).unwrap();
    run(&engine, Some(&own), Command::QueueStats).unwrap();
    refused_as_wrong_instance(run(
        &engine,
        Some(&InstanceId::random()),
        Command::QueueStats,
    ));
}

#[test]
fn status_reports_the_instance_and_a_rename_keeps_its_id() {
    let dir = tempfile::tempdir().unwrap();
    let own = InstanceId::random();
    let engine = open(dir.path(), &own);

    let before = status(&engine)
        .instance
        .expect("an engine with an identity");
    assert_eq!(before.id, own);
    assert!(!before.name.is_empty(), "the computer's name by default");

    // Control and text-reordering characters never reach a confirmation.
    renamed(&engine, &own, "  Studio\u{202E} PC\n ");
    let after = status(&engine).instance.unwrap();
    assert_eq!(after.id, own);
    assert_eq!(after.name, "Studio PC");

    // The name is kept with the settings: a restarted engine reports it.
    drop(engine);
    let engine = open(dir.path(), &own);
    assert_eq!(status(&engine).instance.unwrap().name, "Studio PC");

    // Blank returns to the computer's name.
    renamed(&engine, &own, "   ");
    assert_eq!(status(&engine).instance.unwrap().name, before.name);
}

#[test]
fn an_engine_without_an_identity_accepts_unnamed_changes() {
    let dir = tempfile::tempdir().unwrap();
    let session =
        Arc::new(Session::load_with_browser(dir.path().join("queue-v1.json"), 3, None).unwrap());
    let engine = InProcessClient::manual(Engine::new(session));
    run(&engine, None, change(&engine)).unwrap();
    assert_eq!(status(&engine).instance, None);
}
