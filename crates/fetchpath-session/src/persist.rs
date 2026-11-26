//! Atomic JSON persistence for `queue.json` and `engine-v1.json`.

use crate::durable::{DurableEngine, RecordDurable};
use crate::types::{Approval, JobSnapshot, TorrentPolicy, is_false};
use crate::{ENGINE_SCHEMA_VERSION, QUEUE_SCHEMA_VERSION, QueueState};
use fetchpath_protocol::principal::Principal;
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::Path;
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PersistedQueue {
    pub(crate) schema_version: u32,
    pub(crate) records: Vec<PersistedRecord>,
    /// The engine file generation and event cursor this queue committed
    /// (FP-051). Absent from a 0.1.0 file, which therefore reads and writes
    /// back unchanged.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub(crate) engine_generation: u64,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub(crate) engine_cursor: u64,
}

pub(crate) fn is_zero(value: &u64) -> bool {
    *value == 0
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PersistedRecord {
    pub(crate) id: String,
    pub(crate) restart_url: Option<String>,
    #[serde(default)]
    pub(crate) credential_ref: Option<String>,
    pub(crate) display_url: String,
    pub(crate) destination: String,
    pub(crate) not_before_ms: Option<u64>,
    pub(crate) created_at_ms: u64,
    pub(crate) finished_at_ms: Option<u64>,
    #[serde(default)]
    pub(crate) media_variant_id: Option<String>,
    #[serde(default)]
    pub(crate) media_quality: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) torrent_policy: Option<TorrentPolicy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) torrent_metadata_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub(crate) torrent_auto: bool,
    #[serde(flatten, default)]
    pub(crate) durable: RecordDurable,
    /// Absent for the person's own jobs, so a 0.1.0 file writes back
    /// unchanged.
    #[serde(default, skip_serializing_if = "Principal::is_user")]
    pub(crate) principal: Principal,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) approval: Option<Approval>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub(crate) size_approved: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub(crate) folder_approved: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub(crate) public_origin: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub(crate) provenance_marked: bool,
    pub(crate) view: JobSnapshot,
}

#[cfg(test)]
pub(crate) fn save_persisted(path: &Path, state: &QueueState) -> io::Result<()> {
    write_queue(path, state, 0, 0)
}

/// The ledger and event log's file, beside the queue.
pub(crate) const ENGINE_FILE: &str = "engine-v1.json";

/// Reads the engine file. A missing or unreadable one starts empty: it only
/// holds replayable history and the recent command ledger, never a job.
pub(crate) fn load_engine(path: &Path) -> DurableEngine {
    File::open(path)
        .ok()
        .and_then(|file| serde_json::from_reader::<_, DurableEngine>(io::BufReader::new(file)).ok())
        .filter(|engine| engine.schema_version == ENGINE_SCHEMA_VERSION)
        .unwrap_or_default()
}

/// Writes JSON through a synced temporary file and a rename.
pub(crate) fn write_json_atomically(path: &Path, value: &impl Serialize) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension("json.new");
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&temporary)?;
    // Serialized first and written once, so a failed write is an error
    // rather than a short file that is synced and renamed into place.
    file.write_all(&serde_json::to_vec(value)?)?;
    file.sync_all()?;
    drop(file);
    fs::rename(&temporary, path)
}

pub(crate) fn write_queue(
    path: &Path,
    state: &QueueState,
    engine_generation: u64,
    engine_cursor: u64,
) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let queue = PersistedQueue {
        schema_version: QUEUE_SCHEMA_VERSION,
        records: state
            .records
            .iter()
            .map(|record| PersistedRecord {
                id: record.id.clone(),
                restart_url: record.restart_url.clone(),
                credential_ref: record.credential_ref.clone(),
                display_url: record.display_url.clone(),
                destination: record.destination.display().to_string(),
                not_before_ms: record.not_before_ms,
                created_at_ms: record.created_at_ms,
                finished_at_ms: record.finished_at_ms,
                media_variant_id: record.media_variant_id.clone(),
                media_quality: record.media_quality.clone(),
                torrent_policy: record.torrent_policy,
                torrent_metadata_sha256: record.torrent_metadata_sha256.clone(),
                torrent_auto: record.torrent_auto,
                durable: record.durable.clone(),
                principal: record.principal.clone(),
                approval: record.approval.clone(),
                size_approved: record.size_approved,
                folder_approved: record.folder_approved,
                public_origin: record.public_origin,
                provenance_marked: record.provenance_marked,
                view: record.view.clone(),
            })
            .collect(),
        engine_generation,
        engine_cursor,
    };
    let temporary = path.with_extension("json.new");
    let backup = path.with_extension("json.bak");
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&temporary)?;
    // One write: straight into the file, serde's small writes cost a system
    // call each, about a millisecond per job on a long queue.
    let mut bytes = serde_json::to_vec_pretty(&queue)?;
    bytes.push(b'\n');
    file.write_all(&bytes)?;
    file.sync_all()?;
    drop(file);

    let _ = fs::remove_file(&backup);
    if path.exists() {
        fs::rename(path, &backup)?;
    }
    if let Err(error) = fs::rename(&temporary, path) {
        let _ = fs::rename(&backup, path);
        return Err(error);
    }
    Ok(())
}

/// What the queue file holds.
pub(crate) enum Loaded {
    Current(PersistedQueue),
    /// Written by a newer Fetchpath. Its records, when they can still be
    /// read, are only shown.
    Newer {
        version: Option<u64>,
        queue: Option<PersistedQueue>,
    },
    Missing,
}

#[cfg(test)]
impl Loaded {
    pub(crate) fn current(self) -> Option<PersistedQueue> {
        match self {
            Self::Current(queue) => Some(queue),
            _ => None,
        }
    }
}

/// What a queue file says about its format.
enum Version {
    Current,
    /// Newer than this build; the number when it is one this build can read.
    Newer(Option<u64>),
    /// Not a queue this build or a later one wrote: missing, corrupt, or 0.
    Unreadable,
}

fn version_of(text: &[u8]) -> Version {
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(text) else {
        return Version::Unreadable;
    };
    match value.get("schemaVersion") {
        None | Some(serde_json::Value::Null) => Version::Unreadable,
        Some(version) => match version.as_u64() {
            Some(number) if (1..=u64::from(QUEUE_SCHEMA_VERSION)).contains(&number) => {
                Version::Current
            }
            Some(number) if number > u64::from(QUEUE_SCHEMA_VERSION) => {
                Version::Newer(Some(number))
            }
            Some(_) => Version::Unreadable,
            // A version written in a form this build does not use ("2", 2.5,
            // or beyond u64) can only come from a later build.
            None => Version::Newer(None),
        },
    }
}

/// Reads the queue, falling back to the backup when the queue is missing or
/// unreadable. A file from a newer build, in either place, decides (finding
/// F1): the session is then read-only, so neither file is ever replaced,
/// including a newer backup behind a queue an older build wrote over it.
pub(crate) fn load_persisted(path: &Path) -> io::Result<Loaded> {
    let read = |candidate: &Path| match fs::read(candidate) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    };
    let files = [read(path)?, read(&path.with_extension("json.bak"))?];
    for text in files.iter().flatten() {
        if let Version::Newer(version) = version_of(text) {
            return Ok(Loaded::Newer {
                version,
                queue: serde_json::from_slice::<PersistedQueue>(text).ok(),
            });
        }
    }
    for text in files.iter().flatten() {
        if let Version::Current = version_of(text)
            && let Ok(queue) = serde_json::from_slice::<PersistedQueue>(text)
        {
            return Ok(Loaded::Current(queue));
        }
    }
    Ok(Loaded::Missing)
}

pub(crate) fn newer_queue_explanation(version: Option<u64>) -> String {
    let format = version.map_or(String::new(), |version| format!(" (format {version})"));
    format!(
        "Your download list was saved by a newer version of Fetchpath{format}. \
         This version shows it without changing it and starts nothing. \
         Install the newer version to keep using it; the list is kept as it is."
    )
}
