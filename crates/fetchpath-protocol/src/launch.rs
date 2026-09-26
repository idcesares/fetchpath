//! Finding the engine, and starting it when it is not running (FP-053).
//!
//! Every client goes through [`attach_or_launch`]: read the endpoint and the
//! secret, connect, and if there is no engine start `fetchpath engine`
//! detached and without a window, then keep trying for a bounded time. Two
//! clients racing may start two engines; the engine's single-owner lock lets
//! one keep running and the other exit, and both clients find the winner.

use crate::command::{Command, CommandEnvelope};
use crate::error::{ErrorCode, ErrorScope, ProtocolError};
use crate::ids::ClientId;
use crate::pipe::{EngineSecret, Limits, PipeClient, PipeEngineClient, endpoint};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// How long a client waits for an engine it started.
pub const LAUNCH_WAIT: Duration = Duration::from_secs(10);

/// The folder the engine keeps its state in.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EngineHome {
    dir: PathBuf,
}

impl EngineHome {
    /// `FETCHPATH_APP_DATA_DIR` when set, otherwise the desktop's folder,
    /// `%APPDATA%\app.fetchpath.desktop`.
    pub fn from_env() -> Result<Self, ProtocolError> {
        if let Some(dir) = std::env::var_os("FETCHPATH_APP_DATA_DIR") {
            return Ok(Self::at(PathBuf::from(dir)));
        }
        let roaming = std::env::var_os("APPDATA").ok_or_else(|| {
            ProtocolError::new(
                ErrorCode::try_from("internal.data_folder_unavailable".to_owned()).expect("valid"),
                ErrorScope::Engine,
                "The Windows application data folder is not set.",
            )
        })?;
        Ok(Self::at(
            PathBuf::from(roaming).join("app.fetchpath.desktop"),
        ))
    }

    pub fn at(dir: PathBuf) -> Self {
        Self { dir }
    }

    /// Whether this is the person's own folder, `%APPDATA%\app.fetchpath.desktop`,
    /// however it was named. A client names it explicitly to the engine it
    /// starts, so the variable being set says nothing about which folder it is.
    pub fn is_default(&self) -> bool {
        std::env::var_os("APPDATA").is_some_and(|roaming| {
            let default = PathBuf::from(roaming).join("app.fetchpath.desktop");
            let normal = |path: &Path| {
                path.to_string_lossy()
                    .trim_end_matches(['\\', '/'])
                    .replace('/', "\\")
                    .to_lowercase()
            };
            normal(&default) == normal(&self.dir)
        })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn queue_path(&self) -> PathBuf {
        self.dir.join("queue-v1.json")
    }

    pub fn secret_path(&self) -> PathBuf {
        self.dir.join("engine-secret-v1.bin")
    }

    pub fn endpoint_path(&self) -> PathBuf {
        self.dir.join("engine-endpoint-v1")
    }

    /// The single-owner lock. Only the engine opens it; clients never do
    /// (FP-055).
    pub fn lock_path(&self) -> PathBuf {
        self.dir.join("instance.lock")
    }

    /// Present while setup replaces or removes Fetchpath's files (FP-057).
    /// The installer deletes it when it finishes; the name is repeated in
    /// `installer-hooks.nsh`.
    pub fn update_hold_path(&self) -> PathBuf {
        self.dir.join("engine-update-hold-v1")
    }

    /// Whether setup is holding engines off. A hold older than
    /// [`UPDATE_HOLD_LIMIT`] is ignored, so an interrupted setup cannot keep
    /// Fetchpath from starting for long.
    pub fn update_held(&self) -> bool {
        std::fs::metadata(self.update_hold_path())
            .and_then(|meta| meta.modified())
            .is_ok_and(|written| match written.elapsed() {
                Ok(age) => age < UPDATE_HOLD_LIMIT,
                // Written "in the future": the clock moved back. Honored only
                // as far as the limit, so a large step cannot block for hours.
                Err(ahead) => ahead.duration() < UPDATE_HOLD_LIMIT,
            })
    }

    /// Starts holding engines off. Written, not just created, so that a
    /// second setup refreshes the time.
    pub fn hold_for_update(&self) -> std::io::Result<()> {
        std::fs::write(self.update_hold_path(), b"setup is replacing Fetchpath\n")
    }
}

/// How long an update hold is honored after it was written.
pub const UPDATE_HOLD_LIMIT: Duration = Duration::from_secs(10 * 60);

fn updating() -> ProtocolError {
    ProtocolError::new(
        ErrorCode::ENGINE_UNAVAILABLE,
        ErrorScope::Engine,
        "Fetchpath is being updated or removed. Try again when setup has finished.",
    )
}

/// Connects to a running engine, or reports `contract.engine_unavailable`.
/// Never starts one.
pub fn attach(home: &EngineHome, limits: Limits) -> Result<PipeEngineClient, ProtocolError> {
    let name = endpoint::read(&home.endpoint_path())?;
    let secret = EngineSecret::load(&home.secret_path())?;
    // Prove it is reachable, genuine and serving now, so the caller learns
    // the outcome here rather than on its first command. An engine that is
    // stopping still completes a handshake, but answers this as unavailable.
    let probe = PipeClient::connect(&name, &secret, limits, Duration::from_secs(2))?;
    probe.call(
        &CommandEnvelope::new(ClientId::random(), Command::EngineStatus),
        Duration::from_secs(5),
    )?;
    Ok(PipeEngineClient::new(
        name,
        EngineSecret::load(&home.secret_path())?,
        limits,
    ))
}

fn absent(error: &ProtocolError) -> bool {
    matches!(
        error.code.as_str(),
        "contract.engine_unavailable" | "auth.engine_secret_missing"
    )
}

/// An engine that is starting or stopping can drop a connection mid
/// handshake; that is worth another try, not an answer.
fn transient(error: &ProtocolError) -> bool {
    matches!(
        error.code.as_str(),
        "auth.handshake_failed" | "contract.connection_lost" | "contract.connection_timed_out"
    )
}

/// How often a waiting client starts the engine again while none answers,
/// for the case where the one it started found a stopping engine still
/// holding the lock and left.
const RELAUNCH_EVERY: Duration = Duration::from_secs(1);

/// Connects to the engine, starting `engine_exe engine` first when none is
/// running, and waits up to `wait` for it.
pub fn attach_or_launch(
    home: &EngineHome,
    engine_exe: &Path,
    limits: Limits,
    wait: Duration,
) -> Result<PipeEngineClient, ProtocolError> {
    let deadline = Instant::now() + wait;
    let mut launched: Option<Instant> = None;
    loop {
        match attach(home, limits) {
            Ok(client) => return Ok(client),
            Err(error) if transient(&error) && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(50));
                continue;
            }
            // Setup is replacing the files an engine would run from.
            Err(error) if absent(&error) && home.update_held() => return Err(updating()),
            Err(error) if absent(&error) && Instant::now() < deadline => {
                if launched.is_none_or(|at| at.elapsed() >= RELAUNCH_EVERY) {
                    start_engine(home, engine_exe)?;
                    launched = Some(Instant::now());
                }
            }
            Err(error) => {
                return Err(if absent(&error) {
                    ProtocolError::new(
                        ErrorCode::ENGINE_UNAVAILABLE,
                        ErrorScope::Engine,
                        format!(
                            "The Fetchpath engine did not start within {} seconds.",
                            wait.as_secs()
                        ),
                    )
                } else {
                    error
                });
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// What [`nudge_for_browser`] achieved. The capture is safe in the inbox in
/// every case; this only says how soon it reaches the queue.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Nudged {
    /// A running engine took the inbox in.
    Taken,
    /// An engine was started; it takes the inbox in as it starts.
    Started,
    /// Setup holds engines off; the next engine takes it in.
    Held,
}

/// The browser host's handoff (FP-056): as the `browser` principal, asks a
/// running engine to take in the captures waiting in the inbox, or starts
/// `engine_exe engine`, which takes them in before it serves. Bounded by
/// `wait`, since the browser is waiting for the host's answer.
pub fn nudge_for_browser(
    home: &EngineHome,
    engine_exe: &Path,
    limits: Limits,
    wait: Duration,
) -> Result<Nudged, ProtocolError> {
    let deadline = Instant::now() + wait;
    let mut launched: Option<Instant> = None;
    loop {
        let attempt = endpoint::read(&home.endpoint_path())
            .and_then(|name| Ok((name, EngineSecret::load(&home.secret_path())?)))
            .and_then(|(name, secret)| {
                let pipe = PipeClient::connect_as(
                    &name,
                    &secret,
                    limits,
                    Duration::from_secs(1),
                    &crate::principal::Principal::Browser,
                )?;
                pipe.call(
                    &CommandEnvelope::new(ClientId::random(), Command::TakeBrowserCaptures),
                    Duration::from_secs(2),
                )
            });
        match attempt {
            Ok(_) => return Ok(Nudged::Taken),
            // An engine from before FP-056 takes the inbox in on its next tick.
            Err(error) if error.code.as_str() == "contract.unknown_command" => {
                return Ok(Nudged::Taken);
            }
            Err(error) if (absent(&error) || transient(&error)) && home.update_held() => {
                return Ok(Nudged::Held);
            }
            Err(error) if absent(&error) || transient(&error) => {
                if Instant::now() >= deadline {
                    return match launched {
                        Some(_) => Ok(Nudged::Started),
                        None => Err(error),
                    };
                }
                if absent(&error) && launched.is_none_or(|at| at.elapsed() >= RELAUNCH_EVERY) {
                    start_engine(home, engine_exe)?;
                    launched = Some(Instant::now());
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(error) => return Err(error),
        }
    }
}

/// Asks a running engine to stop, whatever protocol version it speaks, and
/// waits up to `wait` for its pipe to go away. `Ok` when none was running.
pub fn request_restart(
    home: &EngineHome,
    limits: Limits,
    wait: Duration,
) -> Result<(), ProtocolError> {
    let name = match endpoint::read(&home.endpoint_path()) {
        Ok(name) => name,
        Err(error) if absent(&error) => return Ok(()),
        Err(error) => return Err(error),
    };
    let secret = match EngineSecret::load(&home.secret_path()) {
        Ok(secret) => secret,
        Err(error) if absent(&error) => return Ok(()),
        Err(error) => return Err(error),
    };
    match PipeClient::request_restart(&name, &secret, limits, Duration::from_secs(2)) {
        Ok(()) => {}
        Err(error) if absent(&error) => return Ok(()),
        Err(error) => return Err(error),
    }
    let deadline = Instant::now() + wait;
    while Instant::now() < deadline {
        match PipeClient::connect(&name, &secret, limits, Duration::from_millis(200)) {
            Err(error) if absent(&error) => return Ok(()),
            _ => std::thread::sleep(Duration::from_millis(50)),
        }
    }
    Err(ProtocolError::new(
        ErrorCode::ENGINE_UNAVAILABLE,
        ErrorScope::Engine,
        "The Fetchpath engine did not stop in time.",
    ))
}

/// Starts `engine_exe engine` detached, with no console window, outside the
/// caller's job object where that is allowed, and with the same data folder.
///
/// It inherits no handles. `std::process::Command` would pass on every
/// inheritable handle the caller holds, such as the pipe a script reads the
/// caller's output from, and the engine would keep that pipe open until it
/// exits, so the script would wait out the engine's idle grace.
fn start_engine(home: &EngineHome, engine_exe: &Path) -> Result<(), ProtocolError> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{
        CREATE_BREAKAWAY_FROM_JOB, CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW,
        CREATE_UNICODE_ENVIRONMENT, CreateProcessW, PROCESS_INFORMATION, STARTUPINFOW,
    };
    let failed = |reason: String| {
        ProtocolError::new(
            ErrorCode::ENGINE_UNAVAILABLE,
            ErrorScope::Engine,
            format!(
                "The Fetchpath engine could not be started from {}: {reason}",
                engine_exe.display()
            ),
        )
    };
    let path = engine_exe.as_os_str();
    if path.encode_wide().any(|unit| unit == u16::from(b'"')) {
        return Err(failed("the path contains a quotation mark".into()));
    }
    let application: Vec<u16> = path.encode_wide().chain([0]).collect();
    let command_line: Vec<u16> = "\""
        .encode_utf16()
        .chain(path.encode_wide())
        .chain("\" engine".encode_utf16())
        .chain([0])
        .collect();
    // Not the caller's directory, which the engine would hold open.
    let folder: Option<Vec<u16>> = engine_exe
        .parent()
        .map(|folder| folder.as_os_str().encode_wide().chain([0]).collect());
    let environment = environment_block(home.dir());

    let spawn = |flags: u32| -> std::io::Result<()> {
        let mut line = command_line.clone();
        // SAFETY: every pointer is to a live, NUL-terminated buffer owned
        // above (the environment block ends in two NULs); `line` is a
        // private copy CreateProcessW may write to; the structures are
        // zero-initialized plain data with `cb` set; both returned handles
        // are closed once, only when the call succeeded.
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
                flags | CREATE_UNICODE_ENVIRONMENT,
                environment.as_ptr().cast(),
                folder
                    .as_ref()
                    .map_or(std::ptr::null(), |folder| folder.as_ptr()),
                &startup,
                &mut info,
            );
            if created == 0 {
                return Err(std::io::Error::last_os_error());
            }
            CloseHandle(info.hThread);
            CloseHandle(info.hProcess);
        }
        Ok(())
    };
    let base = CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW;
    spawn(base | CREATE_BREAKAWAY_FROM_JOB)
        .or_else(|_| spawn(base))
        .map_err(|error| failed(error.to_string()))
}

/// This process's environment with `FETCHPATH_APP_DATA_DIR` set to `dir`,
/// as a Unicode environment block: `NAME=value` entries sorted by name
/// without regard to case, each ending in a NUL, then one more NUL.
fn environment_block(dir: &Path) -> Vec<u16> {
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStrExt;
    const KEY: &str = "FETCHPATH_APP_DATA_DIR";
    let mut entries: Vec<(OsString, OsString)> = std::env::vars_os()
        .filter(|(name, _)| !name.eq_ignore_ascii_case(KEY))
        .collect();
    entries.push((KEY.into(), dir.as_os_str().to_owned()));
    entries.sort_by_key(|(name, _)| name.to_string_lossy().to_uppercase());
    let mut block = Vec::new();
    for (name, value) in &entries {
        block.extend(name.encode_wide());
        block.push(u16::from(b'='));
        block.extend(value.encode_wide());
        block.push(0);
    }
    block.push(0);
    block
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A client always names the folder to the engine it starts, so the
    /// default folder must be recognized by path, however it is spelled
    /// (FP-057: sign-in start was never written for a launched engine).
    #[test]
    fn the_default_folder_is_recognized_by_path() {
        let roaming = PathBuf::from(std::env::var_os("APPDATA").expect("APPDATA is set"));
        let spelled = format!(
            r"{}\APP.fetchpath.Desktop\",
            roaming.display().to_string().to_uppercase()
        );
        assert!(EngineHome::at(PathBuf::from(spelled)).is_default());
        assert!(!EngineHome::at(roaming.join("app.fetchpath.desktop").join("other")).is_default());
        assert!(!EngineHome::at(std::env::temp_dir().join("app.fetchpath.desktop")).is_default());
    }
}
