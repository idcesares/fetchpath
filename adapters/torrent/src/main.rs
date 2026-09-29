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

fn bind_stage(stage: &Path, fresh: bool, info_hash: &str) -> Result<(), &'static str> {
    let mut name = stage.file_name().ok_or("stage.invalid")?.to_os_string();
    name.push(".infohash");
    let marker = stage.with_file_name(name);
    if fresh {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(marker)
            .map_err(|_| "stage.invalid")?;
        file.write_all(info_hash.as_bytes())
            .map_err(|_| "stage.invalid")?;
    } else if fs::read_to_string(marker).map_err(|_| "stage.invalid")? != info_hash {
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

async fn run(request: Request) -> Result<(u64, u64), &'static str> {
    request.validate()?;
    let destination = PathBuf::from(&request.destination);
    if destination.exists() {
        return Err("destination.conflict");
    }
    let stage = stage_path(&destination, &request.job_id, &request.source)?;
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
    let fresh_stage = !stage.exists();
    if !fresh_stage {
        safe_tree(&stage).map_err(|_| "stage.invalid")?;
    }
    let parent = destination.parent().ok_or("destination.invalid")?;
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
    if destination.exists() {
        return Err("destination.conflict");
    }
    fs::rename(&stage, &destination).map_err(|_| "destination.publish_failed")?;
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
