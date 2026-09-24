//! Finding the engine, and starting it when it is not running (FP-053).
//!
//! Every client goes through [`attach_or_launch`]: read the endpoint and the
//! secret, connect, and if there is no engine start `fetchpath engine`
//! detached and without a window, then keep trying for a bounded time. Two
//! clients racing may start two engines; the engine's single-owner lock lets
//! one keep running and the other exit, and both clients find the winner.

use crate::error::{ErrorCode, ErrorScope, ProtocolError};
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

    /// The single-owner lock, shared with the desktop while it still owns
    /// the queue itself.
    pub fn lock_path(&self) -> PathBuf {
        self.dir.join("instance.lock")
    }
}

/// Connects to a running engine, or reports `contract.engine_unavailable`.
/// Never starts one.
pub fn attach(home: &EngineHome, limits: Limits) -> Result<PipeEngineClient, ProtocolError> {
    let name = endpoint::read(&home.endpoint_path())?;
    let secret = EngineSecret::load(&home.secret_path())?;
    // Prove it is reachable and genuine now, so the caller learns the
    // outcome here rather than on its first command.
    PipeClient::connect(&name, &secret, limits, Duration::from_secs(2))?;
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

/// Connects to the engine, starting `engine_exe engine` first when none is
/// running, and waits up to `wait` for it.
pub fn attach_or_launch(
    home: &EngineHome,
    engine_exe: &Path,
    limits: Limits,
    wait: Duration,
) -> Result<PipeEngineClient, ProtocolError> {
    match attach(home, limits) {
        Ok(client) => return Ok(client),
        Err(error) if !absent(&error) => return Err(error),
        Err(_) => {}
    }
    start_engine(home, engine_exe)?;
    let deadline = Instant::now() + wait;
    loop {
        match attach(home, limits) {
            Ok(client) => return Ok(client),
            Err(error) if !absent(&error) || Instant::now() >= deadline => {
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
            Err(_) => std::thread::sleep(Duration::from_millis(50)),
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
fn start_engine(home: &EngineHome, engine_exe: &Path) -> Result<(), ProtocolError> {
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
    let spawn = |flags: u32| {
        Command::new(engine_exe)
            .arg("engine")
            .env("FETCHPATH_APP_DATA_DIR", home.dir())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(flags)
            .spawn()
    };
    let base = CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW;
    spawn(base | CREATE_BREAKAWAY_FROM_JOB)
        .or_else(|_| spawn(base))
        .map(drop)
        .map_err(|error| {
            ProtocolError::new(
                ErrorCode::ENGINE_UNAVAILABLE,
                ErrorScope::Engine,
                format!(
                    "The Fetchpath engine could not be started from {}: {error}",
                    engine_exe.display()
                ),
            )
        })
}
