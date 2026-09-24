//! Where a client finds the running engine's pipe.
//!
//! The engine picks a fresh pipe name every run ([`PipeName::fresh`]) and
//! writes it here after it has claimed the pipe. The file has the same
//! protection as the secret: owner-only, and unreadable by lower-integrity
//! processes. A stale file after the engine stops is harmless: connecting to
//! a name that no longer exists reports the engine as not running.

use super::auth::private_file_sddl;
use super::{ENGINE_PIPE_PREFIX, PipeName, connection_error, ffi};
use crate::error::ProtocolError;
use std::io::{self, Read, Write};
use std::path::Path;

/// Longest endpoint file accepted.
const MAX_ENDPOINT_BYTES: u64 = 512;

/// Records `name` as the engine's pipe, replacing any earlier record in one
/// rename so a reader sees the old name or the new one, never a mix.
pub fn publish(path: &Path, name: &PipeName, user_sid: &str) -> Result<(), ProtocolError> {
    let unwritable = |error: io::Error| {
        connection_error(
            "internal.endpoint_unwritable",
            format!("The engine's endpoint could not be recorded: {error}"),
        )
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(unwritable)?;
    }
    let staging = path.with_extension("new");
    match std::fs::remove_file(&staging) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(unwritable(error)),
    }
    let security =
        ffi::SecurityDescriptor::from_sddl(&private_file_sddl(user_sid)).map_err(unwritable)?;
    let handle = ffi::create_new_file(&staging, &security).map_err(unwritable)?;
    let written = {
        use std::os::windows::io::FromRawHandle;
        let raw = handle.raw();
        std::mem::forget(handle);
        // SAFETY: ownership of the handle moves into the File.
        let mut file = unsafe { std::fs::File::from_raw_handle(raw) };
        file.write_all(name.as_str().as_bytes())
            .and_then(|()| file.sync_all())
    };
    // The staging file keeps its own protection when renamed into place.
    written
        .and_then(|()| std::fs::rename(&staging, path))
        .map_err(|error| {
            let _ = std::fs::remove_file(&staging);
            unwritable(error)
        })
}

/// Reads the pipe name a running engine published.
pub fn read(path: &Path) -> Result<PipeName, ProtocolError> {
    let unavailable = |detail: String| {
        connection_error(
            "contract.engine_unavailable",
            format!("Fetchpath could not find its engine: {detail}."),
        )
    };
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Err(unavailable("the engine has not started".into()));
        }
        Err(error) => return Err(unavailable(error.to_string())),
    };
    let mut text = String::new();
    file.take(MAX_ENDPOINT_BYTES + 1)
        .read_to_string(&mut text)
        .map_err(|error| unavailable(error.to_string()))?;
    let valid = text.len() as u64 <= MAX_ENDPOINT_BYTES
        && text.starts_with(ENGINE_PIPE_PREFIX)
        && text[ENGINE_PIPE_PREFIX.len()..]
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-');
    if !valid {
        return Err(unavailable("its endpoint record is damaged".into()));
    }
    Ok(PipeName::from_published(text))
}
