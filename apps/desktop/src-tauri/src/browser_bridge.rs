//! The native messaging host the browser extension talks to. It validates
//! a capture, stores it in the session's browser inbox and wakes the app.

use fetchpath_session::browser_inbox::storage_reason;
pub use fetchpath_session::browser_inbox::{
    BridgeStore, BrowserCookie, CaptureRequest, InboxRecord, SCHEMA_VERSION, SecretEnvelope,
};
use serde::{Deserialize, Serialize};
use std::io::{self, Read, Write};

pub const HOST_NAME: &str = "com.fetchpath.browser";
const MAX_MESSAGE_BYTES: usize = 1024 * 1024;
const CHROMIUM_EXTENSION_ID: &str = "lfikhkjdpjcjaboanknaabncpkbgoele";
const FIREFOX_EXTENSION_ID: &str = "browser@fetchpath.app";

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum HostRequest {
    Probe { schema_version: u32 },
    Capture(CaptureRequest),
}

#[derive(Clone, Debug, Serialize)]
struct HostResponse {
    schema_version: u32,
    #[serde(rename = "type")]
    response_type: &'static str,
    accepted: bool,
    capture_id: Option<String>,
    deduplicated: bool,
    reason: Option<String>,
}

fn caller_allowed(args: &[String]) -> bool {
    let chromium = format!("chrome-extension://{CHROMIUM_EXTENSION_ID}/");
    args.iter()
        .any(|arg| arg == &chromium || arg == FIREFOX_EXTENSION_ID)
}

pub fn run_native_host() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let response = if !caller_allowed(&args) {
        rejected(None, "bridge.caller_not_allowed")
    } else {
        match read_message(io::stdin())
            .and_then(|bytes| serde_json::from_slice::<HostRequest>(&bytes).map_err(invalid_data))
        {
            Ok(HostRequest::Probe { schema_version }) if schema_version == SCHEMA_VERSION => {
                accepted(None, false, "probe_ack")
            }
            Ok(HostRequest::Probe { .. }) => rejected(None, "contract.unsupported_version"),
            Ok(HostRequest::Capture(request)) => match BridgeStore::default_for_user()
                .map_err(storage_reason)
                .and_then(|store| store.accept(&request))
            {
                Ok(deduplicated) => {
                    launch_desktop();
                    accepted(Some(request.capture_id), deduplicated, "capture_ack")
                }
                Err(reason) => rejected(Some(request.capture_id), &reason),
            },
            Err(error) => rejected(None, &format!("bridge.invalid_message:{error}")),
        }
    };
    let _ = write_message(io::stdout(), &response);
}

/// Starts Fetchpath, or brings it forward when it is already running (a
/// second instance activates the first and exits), so a capture is seen at
/// once instead of waiting in the inbox until the next launch.
///
/// The browser runs this host inside a job object that may end the host's
/// children with it, so the app is started outside the job where the job
/// allows that, and inside it otherwise.
fn launch_desktop() {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        use std::process::{Command, Stdio};
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
        let Some(app) = std::env::current_exe()
            .ok()
            .and_then(|host| host.parent().map(|dir| dir.join("fetchpath-desktop.exe")))
            .filter(|app| app.is_file())
        else {
            return;
        };
        let spawn = |flags: u32| {
            Command::new(&app)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .creation_flags(flags)
                .spawn()
        };
        let base = DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP;
        if spawn(base | CREATE_BREAKAWAY_FROM_JOB).is_err() {
            let _ = spawn(base);
        }
    }
}

fn accepted(
    capture_id: Option<String>,
    deduplicated: bool,
    response_type: &'static str,
) -> HostResponse {
    HostResponse {
        schema_version: SCHEMA_VERSION,
        response_type,
        accepted: true,
        capture_id,
        deduplicated,
        reason: None,
    }
}

fn rejected(capture_id: Option<String>, reason: &str) -> HostResponse {
    HostResponse {
        schema_version: SCHEMA_VERSION,
        response_type: "capture_ack",
        accepted: false,
        capture_id,
        deduplicated: false,
        reason: Some(reason.to_owned()),
    }
}

fn read_message(mut input: impl Read) -> io::Result<Vec<u8>> {
    let mut length = [0_u8; 4];
    input.read_exact(&mut length)?;
    let length = u32::from_ne_bytes(length) as usize;
    if length == 0 || length > MAX_MESSAGE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "message size is invalid",
        ));
    }
    let mut message = vec![0; length];
    input.read_exact(&mut message)?;
    Ok(message)
}

fn write_message(mut output: impl Write, response: &HostResponse) -> io::Result<()> {
    let message = serde_json::to_vec(response).map_err(invalid_data)?;
    output.write_all(&(message.len() as u32).to_ne_bytes())?;
    output.write_all(&message)?;
    output.flush()
}

fn invalid_data(error: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caller_and_frame_boundaries_are_strict() {
        assert!(caller_allowed(&[format!(
            "chrome-extension://{CHROMIUM_EXTENSION_ID}/"
        )]));
        assert!(caller_allowed(&[FIREFOX_EXTENSION_ID.into()]));
        assert!(!caller_allowed(&[
            "chrome-extension://not-fetchpath/".into()
        ]));

        let oversized = ((MAX_MESSAGE_BYTES as u32) + 1).to_ne_bytes();
        assert_eq!(
            read_message(oversized.as_slice()).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }
}
