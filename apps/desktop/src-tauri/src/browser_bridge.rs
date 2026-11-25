//! The native messaging host the browser extension talks to. It validates
//! a capture, stores it in the browser inbox, hands it to the engine and
//! wakes the app.

use fetchpath_browser_inbox::{BridgeStore, CaptureRequest, SCHEMA_VERSION, storage_reason};
use serde::{Deserialize, Serialize};
use std::io::{self, Read, Write};

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
                    nudge_engine();
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

/// How long the browser waits on the engine for a capture, at most. The
/// capture is already safe in the inbox; this only decides how soon it shows.
const NUDGE_WAIT: std::time::Duration = std::time::Duration::from_secs(3);

/// Hands the capture to the engine as the `browser` principal (FP-056): a
/// running engine takes the inbox in at once; otherwise the `fetchpath.exe`
/// beside this host is started and takes it in as it starts. No window is
/// needed. A failure changes nothing: the capture waits in the inbox for the
/// next engine, which is what the extension was told.
fn nudge_engine() {
    use fetchpath_protocol::launch::{self, EngineHome};
    use fetchpath_protocol::pipe::Limits;
    let Some(engine) = std::env::current_exe()
        .ok()
        .and_then(|host| host.parent().map(|dir| dir.join("fetchpath.exe")))
    else {
        return;
    };
    if let Ok(home) = EngineHome::from_env() {
        let _ = launch::nudge_for_browser(&home, &engine, Limits::default(), NUDGE_WAIT);
    }
}

/// Starts Fetchpath, or brings it forward when it is already running (a
/// second instance activates the first and exits), so a capture is seen at
/// once instead of waiting in the inbox until the next launch.
///
/// Uses `CreateProcessW` with `bInheritHandles = 0` so the desktop app (and
/// its WebView2 children) never inherit Chrome's native-messaging stdin/stdout
/// pipes, which would keep the host's output pipe open after this host exits.
/// The browser runs this host inside a job object that may end the host's
/// children with it, so the app is started outside the job where the job
/// allows that, and inside it otherwise.
#[cfg(windows)]
fn spawn_detached_no_inherit(app: &std::path::Path, extra_args: &str) -> bool {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{
        CREATE_BREAKAWAY_FROM_JOB, CREATE_NEW_PROCESS_GROUP, CreateProcessW, DETACHED_PROCESS,
        PROCESS_INFORMATION, STARTUPINFOW,
    };
    let path = app.as_os_str();
    if path.encode_wide().any(|unit| unit == u16::from(b'"')) {
        return false;
    }
    let application: Vec<u16> = path.encode_wide().chain([0]).collect();
    let mut command_line: Vec<u16> = "\""
        .encode_utf16()
        .chain(path.encode_wide())
        .chain("\"".encode_utf16())
        .collect();
    if !extra_args.is_empty() {
        command_line.push(u16::from(b' '));
        command_line.extend(extra_args.encode_utf16());
    }
    command_line.push(0);
    let folder: Option<Vec<u16>> = app
        .parent()
        .map(|dir| dir.as_os_str().encode_wide().chain([0]).collect());
    let spawn = |flags: u32| -> bool {
        let mut line = command_line.clone();
        // SAFETY: `application`, `line`, and `folder` are live NUL-terminated
        // UTF-16 buffers; `line` is a private mutable copy; `startup` and
        // `info` are zeroed POD structs with `cb` initialized; `bInheritHandles`
        // is 0; both returned handles are closed once on success.
        unsafe {
            let mut startup: STARTUPINFOW = std::mem::zeroed();
            startup.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
            let mut info: PROCESS_INFORMATION = std::mem::zeroed();
            let created = CreateProcessW(
                application.as_ptr(),
                line.as_mut_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                0,
                flags,
                std::ptr::null(),
                folder.as_ref().map_or(std::ptr::null(), |dir| dir.as_ptr()),
                &startup,
                &mut info,
            );
            if created == 0 {
                return false;
            }
            CloseHandle(info.hThread);
            CloseHandle(info.hProcess);
            true
        }
    };
    let base = DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP;
    spawn(base | CREATE_BREAKAWAY_FROM_JOB) || spawn(base)
}

fn launch_desktop() {
    #[cfg(windows)]
    {
        let Some(app) = std::env::current_exe()
            .ok()
            .and_then(|host| host.parent().map(|dir| dir.join("fetchpath-desktop.exe")))
            .filter(|app| app.is_file())
        else {
            return;
        };
        let _ = spawn_detached_no_inherit(&app, "");
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

    #[cfg(windows)]
    #[test]
    fn detached_desktop_launch_does_not_inherit_pipe_handles() {
        use std::os::windows::io::FromRawHandle;
        use std::time::{Duration, Instant};
        use windows_sys::Win32::Foundation::{
            CloseHandle, HANDLE, HANDLE_FLAG_INHERIT, SetHandleInformation,
        };
        use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
        use windows_sys::Win32::System::Pipes::CreatePipe;

        let mut read_handle: HANDLE = std::ptr::null_mut();
        let mut write_handle: HANDLE = std::ptr::null_mut();
        let sa = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: std::ptr::null_mut(),
            bInheritHandle: 1,
        };
        // SAFETY: `CreatePipe` writes valid handles when returning non-zero;
        // `SetHandleInformation` marks the write end inheritable (matching
        // Chrome's native-messaging stdout pipe). Ownership of `read_handle`
        // moves into `File::from_raw_handle` and `write_handle` is closed once.
        let mut reader = unsafe {
            assert_ne!(CreatePipe(&mut read_handle, &mut write_handle, &sa, 0), 0);
            assert_ne!(
                SetHandleInformation(write_handle, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT),
                0
            );
            std::fs::File::from_raw_handle(read_handle)
        };

        let windir = std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into());
        let ping = std::path::PathBuf::from(windir).join(r"System32\PING.EXE");
        assert!(spawn_detached_no_inherit(&ping, "-n 4 127.0.0.1"));

        // SAFETY: `write_handle` is valid and closed exactly once here.
        unsafe {
            CloseHandle(write_handle);
        }

        let started = Instant::now();
        let mut buf = Vec::new();
        reader.read_to_end(&mut buf).unwrap();
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "detached child held the inheritable pipe handle open for {:?}",
            started.elapsed()
        );
    }
}
