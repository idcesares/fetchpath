//! Showing a download in its folder: File Explorer opens the folder with
//! the file selected, as "Show in folder" does elsewhere in Windows.

use crate::client::{self, Engine};
use std::path::{Path, PathBuf};

/// `fetchpath folder JOB`: shows a download in File Explorer.
pub fn run(args: &[String]) -> i32 {
    let [reference] = args else {
        eprintln!("usage: fetchpath folder JOB");
        return crate::download::EXIT_USAGE;
    };
    let job = match Engine::connect()
        .and_then(|engine| engine.resolve(std::slice::from_ref(reference)))
    {
        Ok(mut jobs) => jobs.remove(0),
        Err(error) => return client::fail(&error, false),
    };
    match show_job(&job) {
        Ok(said) => {
            println!("{said}");
            0
        }
        Err(message) => {
            eprintln!("fetchpath: {message}");
            client::EXIT_ENGINE
        }
    }
}

/// Shows a job's file, if it has a place on disk yet.
pub fn show_job(job: &fetchpath_protocol::JobSnapshot) -> Result<String, String> {
    match job.destination.as_deref() {
        Some(path) => show(
            Path::new(path),
            job.state == fetchpath_protocol::JobState::Completed,
        ),
        None => Err(format!(
            "{} has no file yet; choose its format first.",
            client::name(job)
        )),
    }
}

/// What there is to show for a download's path.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Place {
    /// The file exists: open its folder with it selected.
    File(PathBuf),
    /// The file is not there (yet, or any more): open the folder alone.
    Folder(PathBuf),
}

/// Decides what to show for `path`, or says why there is nothing.
pub fn place(path: &Path) -> Result<Place, String> {
    if path.is_file() {
        return Ok(Place::File(path.to_path_buf()));
    }
    match path.parent() {
        Some(folder) if folder.is_dir() => Ok(Place::Folder(folder.to_path_buf())),
        _ => Err(format!("{} and its folder do not exist.", path.display())),
    }
}

/// Opens File Explorer at the download, and says what it showed.
/// `saved` says the download finished, so a missing file was moved or
/// deleted rather than not written yet.
pub fn show(path: &Path, saved: bool) -> Result<String, String> {
    let place = place(path)?;
    let target = match &place {
        Place::File(file) => file,
        Place::Folder(folder) => folder,
    };
    open(target)?;
    Ok(match place {
        Place::File(file) => format!("Showing {} in File Explorer.", file.display()),
        Place::Folder(folder) => format!(
            "{}; opened {} in File Explorer.",
            if saved {
                "The file is no longer there (moved or deleted)"
            } else {
                "The file is not there yet"
            },
            folder.display()
        ),
    })
}

/// Opens a folder, or a file's folder with the file selected.
#[cfg(windows)]
fn open(target: &Path) -> Result<(), String> {
    if target.is_dir() {
        return explore(target);
    }
    match (select(target), target.parent()) {
        // The shell could not name the file (a path it cannot parse, such
        // as a very long one): its folder is still worth opening.
        (Err(_), Some(folder)) if folder.is_dir() => explore(folder),
        (result, _) => result,
    }
}

/// Opens a folder with the system's own File Explorer.
#[cfg(windows)]
fn explore(folder: &Path) -> Result<(), String> {
    let explorer = std::env::var_os("SystemRoot")
        .map(|root| Path::new(&root).join("explorer.exe"))
        .filter(|path| path.is_file())
        .unwrap_or_else(|| PathBuf::from("explorer.exe"));
    // Explorer reports failure in its exit code even when it opens the
    // folder, so only starting it is checked.
    std::process::Command::new(explorer)
        .arg(folder)
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("File Explorer could not be started: {error}"))
}

/// Asks the shell to open the file's folder with the file selected.
#[cfg(windows)]
fn select(file: &Path) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::System::Com::{
        COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize,
    };
    use windows_sys::Win32::UI::Shell::{ILCreateFromPathW, ILFree, SHOpenFolderAndSelectItems};

    let mut wide: Vec<u16> = file.as_os_str().encode_wide().collect();
    // An embedded NUL would silently name a shorter path.
    if wide.contains(&0) {
        return Err(format!("Windows could not find {}.", file.display()));
    }
    wide.push(0);
    // SAFETY: `wide` is NUL-terminated and outlives ILCreateFromPathW, which
    // copies it into a new ID list; that list is read by the shell call and
    // then freed exactly once. COM is released only if this call started it.
    unsafe {
        let initialised = CoInitializeEx(std::ptr::null(), COINIT_APARTMENTTHREADED as u32);
        let list = ILCreateFromPathW(wide.as_ptr());
        let result = if list.is_null() {
            Err(format!("Windows could not find {}.", file.display()))
        } else {
            // With no child items, the list names the file itself, and its
            // folder opens with it selected.
            let status = SHOpenFolderAndSelectItems(list, 0, std::ptr::null(), 0);
            ILFree(list);
            if status >= 0 {
                Ok(())
            } else {
                Err(format!("File Explorer could not show {}.", file.display()))
            }
        };
        if initialised >= 0 {
            CoUninitialize();
        }
        result
    }
}

#[cfg(not(windows))]
fn open(_target: &Path) -> Result<(), String> {
    Err("Showing a file in its folder is only available on Windows.".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_is_selected_and_a_missing_file_opens_its_folder() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a b ü.zip");
        std::fs::write(&file, b"x").unwrap();
        assert_eq!(place(&file), Ok(Place::File(file.clone())));
        let later = dir.path().join("not yet.iso");
        assert_eq!(place(&later), Ok(Place::Folder(dir.path().to_path_buf())));
        assert!(place(&dir.path().join("gone").join("x.bin")).is_err());
    }
}
