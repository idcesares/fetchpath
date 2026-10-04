use fetchpath_torrent::{
    Event, MAX_DOWNLOAD_BPS, MAX_PEERS, MAX_REQUEST_BYTES, MAX_UPLOAD_BPS, Request,
};
use futures_util::StreamExt;
use librqbit::{AddTorrent, AddTorrentOptions, Session, SessionOptions, limits::LimitsConfig};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::{self, Read, Write};
use std::num::NonZeroU32;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

fn emit(event: Event) {
    if let Ok(line) = serde_json::to_string(&event) {
        let _ = writeln!(io::stdout().lock(), "{line}");
    }
}

fn stage_path(destination: &Path, job_id: &str, source: &str) -> Result<PathBuf, &'static str> {
    if !destination.is_absolute() || destination.file_name().is_none() {
        return Err("destination.invalid");
    }
    let name = destination.file_name().ok_or("destination.invalid")?;
    let mut stage_name = name.to_os_string();
    let source_hash = format!("{:x}", Sha256::digest(source.as_bytes()));
    stage_name.push(format!(".fetchpath-{job_id}-{}.part", &source_hash[..32]));
    Ok(destination.with_file_name(stage_name))
}

/// Automatic mode stages inside the root under a name that does not depend on the
/// final folder name, which is unknown until metadata arrives.
fn auto_stage_path(root: &Path, job_id: &str, source: &str) -> Result<PathBuf, &'static str> {
    if !root.is_absolute() {
        return Err("destination.invalid");
    }
    let source_hash = format!("{:x}", Sha256::digest(source.as_bytes()));
    Ok(root.join(format!(".fetchpath-{job_id}-{}.part", &source_hash[..32])))
}

/// The torrent's own name when it is a safe single folder name, otherwise a neutral
/// name from the info hash.
fn auto_folder_name(info_name: Option<&str>, info_hash_hex: &str) -> String {
    match info_name {
        Some(name) if name.len() <= 200 && safe_name(name.as_bytes()) => name.to_owned(),
        _ => format!("Torrent {}", &info_hash_hex[..8.min(info_hash_hex.len())]),
    }
}

/// Move a directory atomically without replacing an existing destination.
#[cfg(windows)]
fn move_noreplace(stage: &Path, destination: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::MoveFileExW;
    let wide = |path: &Path| -> io::Result<Vec<u16>> {
        let mut value: Vec<_> = path.as_os_str().encode_wide().collect();
        if value.contains(&0) {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "invalid path"));
        }
        value.push(0);
        Ok(value)
    };
    let stage = wide(stage)?;
    let destination = wide(destination)?;
    // SAFETY: both pointers address NUL-terminated buffers valid for this call.
    // Flags zero neither replaces destinations nor falls back to a copy/delete.
    if unsafe { MoveFileExW(stage.as_ptr(), destination.as_ptr(), 0) } == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(not(windows))]
fn move_noreplace(stage: &Path, destination: &Path) -> io::Result<()> {
    // The supported helper platform is Windows. Keep exclusive claims on other
    // platforms, where a directory rename can replace our empty claim.
    fs::create_dir(destination)?;
    let result = fs::rename(stage, destination);
    if result.is_err() {
        let _ = fs::remove_dir(destination);
    }
    result
}

/// Moves the finished stage to `root/name`, then `name (2)`, `name (3)`, and so on.
/// Existing content is never replaced or merged. The durable record precedes
/// the atomic no-replace move, so recovery can identify a published folder.
fn publish_auto(
    stage: &Path,
    root: &Path,
    name: &str,
    record: impl Fn(&Path) -> Result<(), &'static str>,
) -> Result<PathBuf, &'static str> {
    for attempt in 1..=99u32 {
        let candidate = if attempt == 1 {
            root.join(name)
        } else {
            root.join(format!("{name} ({attempt})"))
        };
        record(&candidate)?;
        match move_noreplace(stage, &candidate) {
            Ok(()) => return Ok(candidate),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(_) => return Err("destination.publish_failed"),
        }
    }
    Err("destination.conflict")
}

fn marker_path(stage: &Path) -> Result<PathBuf, &'static str> {
    let mut name = stage.file_name().ok_or("stage.invalid")?.to_os_string();
    name.push(".infohash");
    Ok(stage.with_file_name(name))
}

/// What a finished publication left in the marker: the folder, then
/// the received and total bytes, after the info hash line.
fn recorded_publication(
    stage: &Path,
    destination: &Path,
    auto_name: bool,
) -> Option<(PathBuf, u64, u64)> {
    let text = fs::read_to_string(marker_path(stage).ok()?).ok()?;
    let mut lines = text.lines().skip(1);
    let folder = PathBuf::from(lines.next()?);
    let received = lines.next()?.parse().ok()?;
    let total = lines.next()?.parse().ok()?;
    let matches_destination = if auto_name {
        folder.parent() == Some(destination)
    } else {
        folder == destination
    };
    (matches_destination && folder.is_dir()).then_some((folder, received, total))
}

fn bind_stage(stage: &Path, fresh: bool, info_hash: &str) -> Result<(), &'static str> {
    let marker = marker_path(stage)?;
    if fresh {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(marker)
            .map_err(|_| "stage.invalid")?;
        file.write_all(info_hash.as_bytes())
            .map_err(|_| "stage.invalid")?;
    } else if fs::read_to_string(marker)
        .map_err(|_| "stage.invalid")?
        .lines()
        .next()
        != Some(info_hash)
    {
        return Err("stage.invalid");
    }
    Ok(())
}

fn safe_tree(root: &Path) -> io::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if fs::symlink_metadata(root)?.file_attributes() & 0x400 != 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unsafe directory",
            ));
        }
    }
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "unsafe path"));
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            if fs::symlink_metadata(entry.path())?.file_attributes() & 0x400 != 0 {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "unsafe path"));
            }
        }
        if kind.is_dir() {
            safe_tree(&entry.path())?;
        } else if !kind.is_file() {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "unsafe file"));
        }
    }
    Ok(())
}

fn safe_name(segment: &[u8]) -> bool {
    if segment.is_empty()
        || segment == b"."
        || segment == b".."
        || segment
            .last()
            .is_some_and(|last| *last == b'.' || *last == b' ')
        || segment.iter().any(|byte| {
            matches!(
                byte,
                b'/' | b'\\' | b':' | b'<' | b'>' | b'"' | b'|' | b'?' | b'*' | 0
            )
        })
    {
        return false;
    }
    // Control characters, and the console device names `CON` does not cover.
    if segment.iter().any(|byte| *byte < 0x20 || *byte == 0x7f)
        || segment.eq_ignore_ascii_case(b"CONIN$")
        || segment.eq_ignore_ascii_case(b"CONOUT$")
    {
        return false;
    }
    let stem = segment
        .split(|byte| *byte == b'.')
        .next()
        .unwrap_or(segment);
    let stem = String::from_utf8_lossy(stem).to_ascii_uppercase();
    !matches!(
        stem.as_str(),
        "CON"
            | "PRN"
            | "AUX"
            | "NUL"
            | "COM1"
            | "COM2"
            | "COM3"
            | "COM4"
            | "COM5"
            | "COM6"
            | "COM7"
            | "COM8"
            | "COM9"
            | "LPT1"
            | "LPT2"
            | "LPT3"
            | "LPT4"
            | "LPT5"
            | "LPT6"
            | "LPT7"
            | "LPT8"
            | "LPT9"
    )
}

/// Record publication intent, then atomically move without replacing anything.
fn publish_explicit(
    stage: &Path,
    destination: &Path,
    record: impl FnOnce(&Path) -> Result<(), &'static str>,
) -> Result<(), &'static str> {
    record(destination)?;
    move_noreplace(stage, destination).map_err(|error| {
        if error.kind() == io::ErrorKind::AlreadyExists {
            "destination.conflict"
        } else {
            "destination.publish_failed"
        }
    })
}

fn record_publication(
    stage: &Path,
    folder: &Path,
    received: u64,
    total: u64,
) -> Result<(), &'static str> {
    let marker = marker_path(stage)?;
    let text = fs::read_to_string(&marker).map_err(|_| "stage.invalid")?;
    let hash = text.lines().next().ok_or("stage.invalid")?;
    let mut next = marker.clone().into_os_string();
    next.push(".next");
    let next = PathBuf::from(next);
    let mut file = fs::File::create(&next).map_err(|_| "stage.invalid")?;
    write!(file, "{hash}\n{}\n{received}\n{total}", folder.display())
        .and_then(|_| file.sync_all())
        .and_then(|_| fs::rename(&next, &marker))
        .map_err(|_| "stage.invalid")
}

async fn run(request: Request) -> Result<(u64, u64), &'static str> {
    request.validate()?;
    let destination = PathBuf::from(&request.destination);
    let stage = if request.auto_name {
        auto_stage_path(&destination, &request.job_id, &request.source)?
    } else {
        stage_path(&destination, &request.job_id, &request.source)?
    };
    if stage.exists()
        && (!stage.is_dir()
            || stage
                .symlink_metadata()
                .map_err(|_| "stage.invalid")?
                .file_type()
                .is_symlink())
    {
        return Err("stage.invalid");
    }
    if !stage.exists()
        && let Some((folder, received, total)) =
            recorded_publication(&stage, &destination, request.auto_name)
    {
        // An earlier run published but the engine never recorded it.
        // Keep the evidence until the engine durably acknowledges completion.
        emit(Event::Published {
            path: folder.display().to_string(),
        });
        return Ok((received, total));
    }
    if !request.auto_name && destination.exists() {
        return Err("destination.conflict");
    }
    let fresh_stage = !stage.exists();
    if !fresh_stage {
        safe_tree(&stage).map_err(|_| "stage.invalid")?;
    }
    let parent = if request.auto_name {
        destination.as_path()
    } else {
        destination.parent().ok_or("destination.invalid")?
    };
    fs::create_dir_all(parent).map_err(|_| "destination.unavailable")?;
    fs::create_dir_all(&stage).map_err(|_| "stage.unavailable")?;

    let options = SessionOptions {
        dht: request.discover_peers.then(|| librqbit::DhtSessionConfig {
            persistence: None,
            ..Default::default()
        }),
        disable_trackers: !request.discover_peers,
        disable_local_service_discovery: true,
        listen: None,
        peer_limit: Some(MAX_PEERS),
        disable_upload: !request.upload,
        ratelimits: LimitsConfig {
            download_bps: NonZeroU32::new(MAX_DOWNLOAD_BPS),
            upload_bps: NonZeroU32::new(MAX_UPLOAD_BPS),
        },
        ..Default::default()
    };
    let session = Session::new_with_opts(stage.clone(), options)
        .await
        .map_err(|_| "torrent.engine_unavailable")?;
    let metadata = if let (Some(path), Some(expected_hash)) = (
        request.metadata_path.as_deref(),
        request.metadata_sha256.as_deref(),
    ) {
        let path = Path::new(path);
        let attributes = fs::symlink_metadata(path).map_err(|_| "torrent.metadata_unavailable")?;
        if !attributes.file_type().is_file()
            || attributes.file_type().is_symlink()
            || attributes.len() > 4 * 1024 * 1024
        {
            return Err("torrent.metadata_invalid");
        }
        let bytes = fs::read(path).map_err(|_| "torrent.metadata_unavailable")?;
        if bytes.len() > 4 * 1024 * 1024 || format!("{:x}", Sha256::digest(&bytes)) != expected_hash
        {
            return Err("torrent.metadata_invalid");
        }
        Some(bytes)
    } else if request.source.starts_with("https://") {
        let response = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|_| "torrent.metadata_unavailable")?
            .get(&request.source)
            .send()
            .await
            .map_err(|_| "torrent.metadata_unavailable")?;
        if !response.status().is_success()
            || response
                .content_length()
                .is_some_and(|len| len > 4 * 1024 * 1024)
        {
            return Err("torrent.metadata_invalid");
        }
        let mut bytes = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| "torrent.metadata_unavailable")?;
            if bytes.len().saturating_add(chunk.len()) > 4 * 1024 * 1024 {
                return Err("torrent.metadata_invalid");
            }
            bytes.extend_from_slice(&chunk);
        }
        Some(bytes)
    } else {
        None
    };
    let listed = session
        .add_torrent(
            match metadata {
                Some(bytes) => AddTorrent::from_bytes(bytes),
                None => AddTorrent::from_url(&request.source),
            },
            Some(AddTorrentOptions {
                list_only: true,
                peer_limit: Some(MAX_PEERS),
                ..Default::default()
            }),
        )
        .await
        .map_err(|_| "torrent.metadata_invalid")?;
    let librqbit::AddTorrentResponse::ListOnly(list) = listed else {
        return Err("torrent.metadata_invalid");
    };
    let identity = format!("{:x}", Sha256::digest(list.info_hash.0));
    let auto_name = request
        .auto_name
        .then(|| auto_folder_name(list.info.name().as_deref(), &list.info_hash.as_string()));
    bind_stage(&stage, fresh_stage, &identity)?;
    for file in list.info.iter_file_details() {
        let components: Vec<_> = file.filename.iter_components_bytes().collect();
        if components.is_empty() || components.iter().any(|segment| !safe_name(segment)) {
            return Err("torrent.path_invalid");
        }
    }
    if request.max_bytes.is_some_and(|limit| {
        list.info
            .iter_file_lengths()
            .try_fold(0u64, |total, len| total.checked_add(len))
            .is_none_or(|total| total > limit)
    }) {
        return Err("size_limit");
    }
    if stage
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err("stage.invalid");
    }
    let handle = session
        .add_torrent(
            AddTorrent::from_bytes(list.torrent_bytes),
            Some(AddTorrentOptions {
                overwrite: true,
                peer_limit: Some(MAX_PEERS),
                ..Default::default()
            }),
        )
        .await
        .map_err(|_| "torrent.start_failed")?
        .into_handle()
        .ok_or("torrent.start_failed")?;
    loop {
        let stats = handle.stats();
        emit(Event::Progress {
            received: stats.progress_bytes,
            total: stats.total_bytes,
        });
        if stats.error.is_some() {
            return Err("torrent.transfer_failed");
        }
        if request
            .max_bytes
            .is_some_and(|limit| stats.progress_bytes > limit)
        {
            return Err("size_limit");
        }
        if stats.finished {
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    handle
        .wait_until_completed()
        .await
        .map_err(|_| "torrent.verification_failed")?;
    let stats = handle.stats();
    session.stop().await;
    safe_tree(&stage).map_err(|_| "torrent.path_invalid")?;
    let record =
        |folder: &Path| record_publication(&stage, folder, stats.progress_bytes, stats.total_bytes);
    let published = if let Some(name) = auto_name {
        publish_auto(&stage, &destination, &name, record)?
    } else {
        publish_explicit(&stage, &destination, record)?;
        destination
    };
    emit(Event::Published {
        path: published.display().to_string(),
    });
    // The engine removes the marker only after its completion save succeeds.
    Ok((stats.progress_bytes, stats.total_bytes))
}

#[tokio::main]
async fn main() {
    let mut input = Vec::new();
    let read = io::stdin()
        .take((MAX_REQUEST_BYTES + 1) as u64)
        .read_to_end(&mut input);
    let result = match read {
        Ok(_) if input.len() <= MAX_REQUEST_BYTES => serde_json::from_slice::<Request>(&input)
            .map_err(|_| "request.invalid")
            .and_then(|request| Ok(request)),
        _ => Err("request.invalid"),
    };
    let result = match result {
        Ok(request) => run(request).await,
        Err(code) => Err(code),
    };
    match result {
        Ok((received, total)) => emit(Event::Completed { received, total }),
        Err(code) => {
            emit(Event::Failed { code: code.into() });
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn staging_is_bound_to_the_source_and_stable_for_retry() {
        let destination = Path::new("C:\\downloads\\album");
        let job_id = "01234567-89ab-cdef-0123-456789abcdef";
        let a = stage_path(destination, job_id, "magnet:?xt=urn:btih:aaaa").unwrap();
        let a_retry = stage_path(destination, job_id, "magnet:?xt=urn:btih:aaaa").unwrap();
        let b = stage_path(destination, job_id, "magnet:?xt=urn:btih:bbbb").unwrap();
        assert_eq!(a, a_retry);
        assert_ne!(a, b, "another torrent must not reuse staged files");
    }

    #[test]
    fn automatic_staging_is_stable_bound_and_independent_of_the_name() {
        let root = Path::new(r"C:\downloads");
        let job_id = "01234567-89ab-cdef-0123-456789abcdef";
        let a = auto_stage_path(root, job_id, "magnet:?xt=urn:btih:aaaa").unwrap();
        assert_eq!(
            a,
            auto_stage_path(root, job_id, "magnet:?xt=urn:btih:aaaa").unwrap()
        );
        assert_ne!(
            a,
            auto_stage_path(root, job_id, "magnet:?xt=urn:btih:bbbb").unwrap()
        );
        assert_eq!(a.parent(), Some(root));
        auto_stage_path(Path::new("downloads"), job_id, "x").unwrap_err();
    }

    #[test]
    fn automatic_names_fall_back_when_the_torrent_name_is_hazardous() {
        let hash = "0123456789abcdef0123456789abcdef01234567";
        assert_eq!(auto_folder_name(Some("Album"), hash), "Album");
        for bad in [
            "..", ".", "CON", "a/b", r"a\b", "name.", "name ", "", "C:evil", "a\nb", "a\u{1}b",
            "a\u{7f}", "CONIN$", "conout$",
        ] {
            assert_eq!(
                auto_folder_name(Some(bad), hash),
                "Torrent 01234567",
                "{bad}"
            );
        }
        assert_eq!(auto_folder_name(None, hash), "Torrent 01234567");
        assert_eq!(
            auto_folder_name(Some(&"a".repeat(201)), hash),
            "Torrent 01234567"
        );
    }

    #[test]
    fn automatic_publication_never_replaces_existing_content() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        fs::create_dir(root.join("Album")).unwrap();
        fs::write(root.join("Album").join("keep.txt"), b"mine").unwrap();
        fs::create_dir(root.join("Album (2)")).unwrap();
        let stage = root.join(".stage.part");
        fs::create_dir(&stage).unwrap();
        fs::write(stage.join("new.bin"), b"new").unwrap();
        let published = publish_auto(&stage, root, "Album", |_| Ok(())).unwrap();
        assert_eq!(published, root.join("Album (3)"));
        assert!(published.join("new.bin").is_file());
        assert!(!stage.exists());
        assert_eq!(
            fs::read(root.join("Album").join("keep.txt")).unwrap(),
            b"mine"
        );
        assert!(!root.join("Album (2)").join("new.bin").exists());
    }

    #[test]
    fn an_existing_empty_folder_is_skipped_and_kept() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        fs::create_dir(root.join("Album")).unwrap();
        let stage = root.join(".stage.part");
        fs::create_dir(&stage).unwrap();
        fs::write(stage.join("new.bin"), b"new").unwrap();
        let published = publish_auto(&stage, root, "Album", |_| Ok(())).unwrap();
        assert_eq!(published, root.join("Album (2)"));
        assert!(root.join("Album").is_dir());
        assert_eq!(fs::read_dir(root.join("Album")).unwrap().count(), 0);
    }

    #[test]
    fn a_rerun_after_publication_finds_the_recorded_folder() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let stage = root.join(".stage.part");
        fs::create_dir(&stage).unwrap();
        bind_stage(&stage, true, "hash-a").unwrap();
        let marker = marker_path(&stage).unwrap();
        let published = publish_auto(&stage, root, "Album", |folder| {
            let mut file = fs::OpenOptions::new().append(true).open(&marker).unwrap();
            write!(file, "\n{}\n5\n7", folder.display()).unwrap();
            Ok(())
        })
        .unwrap();
        // The engine never recorded it: the stage is gone, the marker remains.
        let (folder, received, total) = recorded_publication(&stage, root, true).unwrap();
        assert_eq!((folder, received, total), (published, 5, 7));
        // Anywhere else than the root, the record is not trusted.
        assert!(recorded_publication(&stage, &root.join("other"), true).is_none());
    }

    #[tokio::test]
    async fn completion_replays_until_durable_ack_for_both_destination_modes() {
        for auto_name in [true, false] {
            let temp = tempfile::tempdir().unwrap();
            let destination = if auto_name {
                temp.path().to_owned()
            } else {
                temp.path().join("Album")
            };
            let mut request = fetchpath_torrent::request(
                "magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567".into(),
                destination.clone(),
                "01234567-89ab-cdef-0123-456789abcdef".into(),
                true,
                false,
            );
            request.auto_name = auto_name;
            let stage = if auto_name {
                auto_stage_path(&destination, &request.job_id, &request.source).unwrap()
            } else {
                stage_path(&destination, &request.job_id, &request.source).unwrap()
            };
            fs::create_dir(&stage).unwrap();
            fs::write(stage.join("new.bin"), b"new").unwrap();
            bind_stage(&stage, true, "hash-a").unwrap();
            let record = |folder: &Path| record_publication(&stage, folder, 3, 3);
            let published = if auto_name {
                publish_auto(&stage, &destination, "Album", record).unwrap()
            } else {
                publish_explicit(&stage, &destination, record).unwrap();
                destination.clone()
            };
            // Crash after rename, then crash again after recovery's completion
            // event but before the engine's durable save. Both restarts replay.
            for _ in 0..2 {
                assert_eq!(run(request.clone()).await.unwrap(), (3, 3));
                assert!(marker_path(&stage).unwrap().is_file());
                assert_eq!(fs::read(published.join("new.bin")).unwrap(), b"new");
                assert!(!temp.path().join("Album (2)").exists());
            }
        }
    }

    #[test]
    fn explicit_publication_preserves_a_folder_created_after_the_initial_check() {
        let temp = tempfile::tempdir().unwrap();
        let stage = temp.path().join(".stage.part");
        let destination = temp.path().join("Album");
        fs::create_dir(&stage).unwrap();
        fs::write(stage.join("new.bin"), b"new").unwrap();
        assert!(!destination.exists());
        // Another writer wins after the request's check, before publication.
        let competing_destination = destination.clone();
        std::thread::spawn(move || fs::create_dir(competing_destination).unwrap())
            .join()
            .unwrap();
        assert_eq!(
            publish_explicit(&stage, &destination, |_| Ok(())),
            Err("destination.conflict")
        );
        assert_eq!(fs::read_dir(&destination).unwrap().count(), 0);
        assert!(stage.join("new.bin").is_file());
    }

    #[test]
    fn explicit_publication_records_before_moving_and_preserves_competing_content() {
        let temp = tempfile::tempdir().unwrap();
        let stage = temp.path().join(".stage.part");
        let destination = temp.path().join("Album");
        fs::create_dir(&stage).unwrap();
        fs::write(stage.join("new.bin"), b"new").unwrap();
        assert_eq!(
            publish_explicit(&stage, &destination, |folder| {
                assert!(stage.join("new.bin").is_file());
                assert!(!folder.exists());
                fs::create_dir(folder).unwrap();
                fs::write(folder.join("keep.txt"), b"mine").unwrap();
                Ok(())
            }),
            Err("destination.conflict")
        );
        assert_eq!(fs::read(destination.join("keep.txt")).unwrap(), b"mine");
        assert!(stage.join("new.bin").is_file());
    }

    #[test]
    fn staged_files_require_the_same_torrent_identity() {
        let temp = tempfile::tempdir().unwrap();
        let stage = temp.path().join("download.part");
        fs::create_dir(&stage).unwrap();
        bind_stage(&stage, false, "hash-a").unwrap_err();
        bind_stage(&stage, true, "hash-a").unwrap();
        bind_stage(&stage, false, "hash-a").unwrap();
        assert!(bind_stage(&stage, false, "hash-b").is_err());
    }
}
