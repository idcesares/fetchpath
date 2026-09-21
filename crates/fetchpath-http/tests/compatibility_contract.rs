use std::{fs, io, net::TcpListener, path::PathBuf};

use fetchpath_http::{
    Authentication, CompatibilityCapabilities, CompatibilityContext, CredentialSecret,
    TransferError, transfer_compatibility,
};

#[test]
fn credentials_are_redacted_from_debug_output() {
    let context = CompatibilityContext {
        authentication: Some(Authentication::Password {
            username: "fixture-user".into(),
            password: CredentialSecret::new("never-print-this"),
        }),
        ..CompatibilityContext::default()
    };

    let rendered = format!("{context:?}");
    assert!(rendered.contains("fixture-user"));
    assert!(!rendered.contains("never-print-this"));
    assert!(rendered.contains("[REDACTED]"));
}

#[test]
fn packaged_capabilities_report_ftp_ftps_and_sftp() {
    let capabilities = CompatibilityCapabilities::detect();

    assert!(capabilities.ftp);
    assert!(capabilities.ftps);
    assert!(capabilities.sftp);
}

#[test]
fn url_userinfo_is_rejected_before_any_network_io() {
    let error = transfer_compatibility(
        "ftp://fixture-user:never-print-this@127.0.0.1/file.bin",
        &CompatibilityContext::default(),
        0,
        || false,
        |_| Ok::<_, io::Error>(()),
    )
    .expect_err("credentials in URL userinfo must be rejected");

    assert!(matches!(error, TransferError::InvalidUrl(_)));
    assert!(!error.to_string().contains("never-print-this"));
}

#[test]
fn protocol_specific_trust_and_auth_are_required_before_connecting() {
    let sftp_without_host_key = CompatibilityContext {
        authentication: Some(Authentication::Password {
            username: "fixture-user".into(),
            password: CredentialSecret::new("fixture-password"),
        }),
        ..CompatibilityContext::default()
    };
    let error = transfer_compatibility(
        "sftp://127.0.0.1:9/file.bin",
        &sftp_without_host_key,
        0,
        || false,
        |_| Ok::<_, io::Error>(()),
    )
    .expect_err("SFTP without a known-hosts file must fail closed");
    assert!(matches!(error, TransferError::HostKey(_)));

    let ftp_with_ssh_key = CompatibilityContext {
        authentication: Some(Authentication::SshKey {
            username: "fixture-user".into(),
            private_key: PathBuf::from("fixture-key"),
            passphrase: None,
        }),
        ..CompatibilityContext::default()
    };
    let error = transfer_compatibility(
        "ftp://127.0.0.1:9/file.bin",
        &ftp_with_ssh_key,
        0,
        || false,
        |_| Ok::<_, io::Error>(()),
    )
    .expect_err("FTP must reject SSH-key authentication before connecting");
    assert!(matches!(error, TransferError::Authentication(_)));
}

#[test]
fn an_unreachable_sftp_host_is_a_transport_failure_not_a_host_key_failure() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);

    let known_hosts = std::env::temp_dir().join(format!(
        "fetchpath-fp016-known-hosts-{}-{port}",
        std::process::id()
    ));
    fs::write(&known_hosts, []).unwrap();
    let context = CompatibilityContext {
        authentication: Some(Authentication::Password {
            username: "fixture-user".into(),
            password: CredentialSecret::new("fixture-password"),
        }),
        ssh_known_hosts: Some(known_hosts.clone()),
        ..CompatibilityContext::default()
    };

    let error = transfer_compatibility(
        &format!("sftp://127.0.0.1:{port}/file.bin"),
        &context,
        0,
        || false,
        |_| Ok::<_, io::Error>(()),
    )
    .expect_err("an unused loopback port must not be reported as a host-key mismatch");
    fs::remove_file(known_hosts).unwrap();

    assert!(matches!(error, TransferError::Transport(_)));
}
