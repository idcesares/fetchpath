//! Paired devices through the engine (FP-033): two engines on loopback pair
//! with a code, a wrong code pairs nothing, sharing is off until turned on,
//! unpairing takes effect, and an agent can reach none of it.

use fetchpath_protocol::command::Command;
use fetchpath_protocol::message::CommandResult;
use fetchpath_protocol::model::{LanView, PairingState};
use fetchpath_protocol::principal::Principal;
use fetchpath_protocol::{ClientId, EngineClient, ProtocolError};
use fetchpath_session::Session;
use fetchpath_session::engine::{Engine, InProcessClient};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

fn engine(dir: &Path) -> Arc<Engine> {
    let session = Session::load_with_browser(dir.join("queue-v1.json"), 3, None).unwrap();
    session.use_cache(dir.join("cache"));
    session.use_lan(dir.join("lan"), dir.join("cache"));
    Engine::new(Arc::new(session))
}

fn send(client: &InProcessClient, command: Command) -> Result<CommandResult, ProtocolError> {
    client.send(&ClientId::random(), command)
}

fn lan(client: &InProcessClient, command: Command) -> LanView {
    match send(client, command).unwrap() {
        CommandResult::Lan { lan } => lan,
        other => panic!("{other:?}"),
    }
}

fn settle(client: &InProcessClient) -> LanView {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let view = lan(client, Command::LanStatus);
        let waiting = view
            .pairing
            .as_ref()
            .is_some_and(|pairing| pairing.state == PairingState::Waiting);
        if !waiting || Instant::now() > deadline {
            return view;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn two_engines_pair_with_a_code_and_the_person_controls_sharing() {
    // SAFETY: this test binary runs no other test that reads the variable.
    unsafe { std::env::set_var("FETCHPATH_LAN_BIND", "127.0.0.1") };
    let (a_dir, b_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (a_engine, b_engine) = (engine(a_dir.path()), engine(b_dir.path()));
    let a = InProcessClient::manual(Arc::clone(&a_engine));
    let b = InProcessClient::manual(Arc::clone(&b_engine));

    let agent = InProcessClient::manual(Arc::clone(&a_engine))
        .with_principal(Principal::try_from("agent:helper").unwrap());
    for command in [
        Command::LanStatus,
        Command::SetLanSharing { enabled: true },
        Command::StartPairing,
    ] {
        assert_eq!(
            send(&agent, command).unwrap_err().code.as_str(),
            "policy.not_permitted"
        );
    }

    let first = lan(&a, Command::LanStatus);
    assert!(!first.sharing, "sharing starts off");
    assert_eq!(first.serving, None);
    assert!(first.devices.is_empty());

    // A wrong code pairs nothing, and spends the code.
    let shown = lan(&a, Command::StartPairing);
    let pairing = shown.pairing.expect("a code is shown");
    assert_eq!(pairing.state, PairingState::Waiting);
    let refused = send(
        &b,
        Command::JoinPairing {
            address: pairing.address.clone(),
            code: "00000-00000".into(),
            label: None,
        },
    )
    .unwrap_err();
    assert!(
        refused.message.contains("did not match"),
        "{}",
        refused.message
    );
    let after = settle(&a);
    assert_eq!(after.pairing.unwrap().state, PairingState::Failed);
    assert!(after.devices.is_empty());
    assert!(lan(&b, Command::LanStatus).devices.is_empty());

    // The right code pairs both ways, and each sees the other's fingerprint.
    let shown = lan(&a, Command::StartPairing);
    let pairing = shown.pairing.unwrap();
    let joined = match send(
        &b,
        Command::JoinPairing {
            address: pairing.address,
            code: pairing.code.unwrap(),
            label: Some("Desk".into()),
        },
    )
    .unwrap()
    {
        CommandResult::Joined { device } => device,
        other => panic!("{other:?}"),
    };
    assert_eq!(joined.fingerprint, shown.fingerprint);
    let a_view = settle(&a);
    let paired = a_view.pairing.unwrap();
    assert_eq!(paired.state, PairingState::Paired);
    assert_eq!(paired.code, None, "a used code is no longer shown");
    let b_view = lan(&b, Command::LanStatus);
    assert_eq!(paired.device.unwrap().fingerprint, b_view.fingerprint);
    assert_eq!(a_view.devices.len(), 1);
    assert_eq!(b_view.devices[0].label, "Desk");

    let on = lan(&a, Command::SetLanSharing { enabled: true });
    assert!(on.sharing);
    assert!(on.serving.is_some(), "{:?}", on.problem);
    let key = on.devices[0].key.clone();
    assert!(lan(&a, Command::Unpair { key }).devices.is_empty());
    let off = lan(&a, Command::SetLanSharing { enabled: false });
    assert!(!off.sharing);
    assert_eq!(off.serving, None);
}
