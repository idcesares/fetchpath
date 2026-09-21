use std::{fs, io, path::PathBuf, time::Duration};

use fetchpath_http::{
    Authentication, CompatibilityContext, CredentialSecret, TransferError, transfer_compatibility,
};
use serde_json::Value;

fn fixture() -> Value {
    let path = std::env::var("FETCHPATH_COMPATIBILITY_FIXTURE")
        .expect("run through tests/compatibility/run.ps1");
    serde_json::from_slice(&fs::read(path).expect("read fixture state"))
        .expect("parse fixture state")
}

fn value(state: &Value, key: &str) -> String {
    state[key].as_str().expect("fixture string").to_owned()
}

fn password_context(state: &Value) -> CompatibilityContext {
    CompatibilityContext {
        authentication: Some(Authentication::Password {
            username: value(state, "username"),
            password: CredentialSecret::new(value(state, "password")),
        }),
        timeout: Some(Duration::from_secs(5)),
        ..CompatibilityContext::default()
    }
}

fn download(
    url: &str,
    context: &CompatibilityContext,
    expected: &[u8],
    start: usize,
) -> Result<Vec<u8>, TransferError> {
    let mut output = expected[..start].to_vec();
    let report = transfer_compatibility(
        url,
        context,
        start as u64,
        || false,
        |chunk| {
            if chunk.offset != output.len() as u64 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "non-sequential offset",
                ));
            }
            output.extend_from_slice(chunk.bytes);
            Ok(())
        },
    )?;
    assert_eq!(report.resumed_from, start as u64);
    assert_eq!(report.transferred_bytes, (expected.len() - start) as u64);
    assert_eq!(report.total_bytes, expected.len() as u64);
    Ok(output)
}

#[test]
#[ignore = "requires real protocol fixtures; run tests/compatibility/run.ps1"]
fn ftp_auth_unicode_path_and_resume_are_real() {
    let state = fixture();
    let expected = fs::read(value(&state, "payload_path")).unwrap();
    let context = password_context(&state);
    let url = value(&state, "ftp_url");

    assert_eq!(download(&url, &context, &expected, 0).unwrap(), expected);
    assert_eq!(
        download(&url, &context, &expected, 65_537).unwrap(),
        expected
    );

    let wrong = CompatibilityContext {
        authentication: Some(Authentication::Password {
            username: value(&state, "username"),
            password: CredentialSecret::new("not-the-password"),
        }),
        ..context
    };
    let error = download(&url, &wrong, &expected, 0).unwrap_err();
    assert!(matches!(error, TransferError::Authentication(_)));
    assert!(!error.to_string().contains("not-the-password"));
}

#[test]
#[ignore = "requires real protocol fixtures; run tests/compatibility/run.ps1"]
fn ftps_requires_a_trusted_certificate_and_resumes() {
    let state = fixture();
    let expected = fs::read(value(&state, "payload_path")).unwrap();
    let url = value(&state, "ftps_url");
    let untrusted = password_context(&state);
    let error = download(&url, &untrusted, &expected, 0).unwrap_err();
    assert!(matches!(error, TransferError::Certificate(_)));

    let mut trusted = untrusted;
    trusted.tls_ca_pem = Some(fs::read(value(&state, "ca_path")).unwrap());
    assert_eq!(download(&url, &trusted, &expected, 0).unwrap(), expected);
    assert_eq!(
        download(&url, &trusted, &expected, 65_537).unwrap(),
        expected
    );
}

#[test]
#[ignore = "requires real protocol fixtures; run tests/compatibility/run.ps1"]
fn sftp_checks_host_key_supports_both_auth_modes_and_resumes() {
    let state = fixture();
    let expected = fs::read(value(&state, "payload_path")).unwrap();
    let url = value(&state, "sftp_url");
    let mut password = password_context(&state);
    password.ssh_known_hosts = Some(PathBuf::from(value(&state, "known_hosts")));
    assert_eq!(download(&url, &password, &expected, 0).unwrap(), expected);
    assert_eq!(
        download(&url, &password, &expected, 65_537).unwrap(),
        expected
    );

    let wrong_password = CompatibilityContext {
        authentication: Some(Authentication::Password {
            username: value(&state, "username"),
            password: CredentialSecret::new("not-the-password"),
        }),
        ..password.clone()
    };
    let error = download(&url, &wrong_password, &expected, 0).unwrap_err();
    assert!(matches!(error, TransferError::Authentication(_)));
    assert!(!error.to_string().contains("not-the-password"));

    let key = CompatibilityContext {
        authentication: Some(Authentication::SshKey {
            username: value(&state, "username"),
            private_key: PathBuf::from(value(&state, "private_key")),
            passphrase: None,
        }),
        ..password.clone()
    };
    assert_eq!(download(&url, &key, &expected, 0).unwrap(), expected);

    let wrong_host = CompatibilityContext {
        ssh_known_hosts: Some(PathBuf::from(value(&state, "wrong_known_hosts"))),
        ..password
    };
    let error = download(&url, &wrong_host, &expected, 0).unwrap_err();
    assert!(matches!(error, TransferError::HostKey(_)));
}
