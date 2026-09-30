use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

const FORMAT_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FaultPoint {
    PayloadWrite,
    PayloadFlush,
    MetadataWrite,
    MetadataFlush,
    MetadataCommit,
    PublicationFence,
    PublicationReconcile,
}

pub trait FaultInjector: Send + Sync {
    fn check(&self, point: FaultPoint) -> io::Result<()>;
}

#[derive(Debug, Default)]
pub struct NoFaults;

impl FaultInjector for NoFaults {
    fn check(&self, _point: FaultPoint) -> io::Result<()> {
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CheckpointPhase {
    Downloading,
    PublicationIntent,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckpointRecord {
    pub generation: u64,
    pub source_key: String,
    pub committed_len: u64,
    pub local_sha256: String,
    pub strong_etag: Option<String>,
    pub expected_total: Option<u64>,
    pub phase: CheckpointPhase,
}

impl CheckpointRecord {
    pub fn downloading(
        source_key: String,
        committed_len: u64,
        local_sha256: String,
        strong_etag: Option<String>,
        expected_total: Option<u64>,
    ) -> Self {
        Self {
            generation: 0,
            source_key,
            committed_len,
            local_sha256,
            strong_etag,
            expected_total,
            phase: CheckpointPhase::Downloading,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PublicationRecovery {
    NotPending,
    Pending,
    Completed,
    Conflict,
}

#[derive(Clone, Debug)]
pub struct CheckpointStore {
    destination: PathBuf,
    staging: PathBuf,
    metadata_prefix: String,
    parent: PathBuf,
    source_key: String,
    /// The generation this store last wrote, so a commit does not scan the
    /// directory for it. `None` until the first commit, and again after the
    /// metadata is removed.
    last_generation: Arc<Mutex<Option<u64>>>,
}

impl CheckpointStore {
    pub fn new(destination: &Path, source_key: &str) -> io::Result<Self> {
        if source_key.is_empty()
            || !source_key
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "source key must be non-empty ASCII letters, digits, '-' or '_'",
            ));
        }
        let parent = destination
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();
        let name = destination
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.is_empty())
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid destination"))?;
        let destination_hash = format!("{:x}", Sha256::digest(name.as_bytes()));
        let source_hash = format!("{:x}", Sha256::digest(source_key.as_bytes()));
        let base = format!(
            ".fetchpath-{}-{}",
            &destination_hash[..24],
            &source_hash[..24]
        );
        Ok(Self {
            destination: destination.to_path_buf(),
            staging: parent.join(format!("{base}.part")),
            metadata_prefix: format!("{base}.checkpoint."),
            parent,
            source_key: source_key.to_owned(),
            last_generation: Arc::new(Mutex::new(None)),
        })
    }

    pub fn destination(&self) -> &Path {
        &self.destination
    }

    pub fn staging(&self) -> &Path {
        &self.staging
    }

    pub fn open_staging(&self) -> io::Result<File> {
        OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&self.staging)
    }

    pub fn latest(&self) -> io::Result<Option<CheckpointRecord>> {
        let mut best = None;
        if !self.parent.exists() {
            return Ok(None);
        }
        for entry in fs::read_dir(&self.parent)? {
            let entry = entry?;
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            let Some(suffix) = name.strip_prefix(&self.metadata_prefix) else {
                continue;
            };
            let Ok(generation) = suffix.parse::<u64>() else {
                continue;
            };
            let Ok(bytes) = fs::read(entry.path()) else {
                continue;
            };
            let Ok(record) = decode_record(&bytes) else {
                continue;
            };
            if record.generation != generation || record.source_key != self.source_key {
                continue;
            }
            if best
                .as_ref()
                .is_none_or(|current: &CheckpointRecord| record.generation > current.generation)
            {
                best = Some(record);
            }
        }
        Ok(best)
    }

    pub fn write_payload(
        &self,
        file: &mut File,
        offset: u64,
        bytes: &[u8],
        faults: &dyn FaultInjector,
    ) -> io::Result<()> {
        faults.check(FaultPoint::PayloadWrite)?;
        file.seek(SeekFrom::Start(offset))?;
        file.write_all(bytes)
    }

    /// Writes `bytes` at `offset` without moving a file cursor, looping until
    /// every byte is written. Several lanes can share one handle this way.
    pub fn write_payload_at(
        &self,
        file: &File,
        offset: u64,
        bytes: &[u8],
        faults: &dyn FaultInjector,
    ) -> io::Result<()> {
        faults.check(FaultPoint::PayloadWrite)?;
        write_all_at(file, offset, bytes)
    }

    pub fn sync_payload(&self, file: &mut File, faults: &dyn FaultInjector) -> io::Result<()> {
        faults.check(FaultPoint::PayloadFlush)?;
        file.flush()?;
        file.sync_all()
    }

    /// Flushes the staging file through a handle of its own. The transfer
    /// keeps writing through another handle meanwhile, avoiding serialization
    /// on the same Windows file object. Filesystem or device contention remains.
    pub fn sync_payload_reopened(&self, faults: &dyn FaultInjector) -> io::Result<()> {
        faults.check(FaultPoint::PayloadFlush)?;
        OpenOptions::new()
            .write(true)
            .open(&self.staging)?
            .sync_all()
    }

    /// [`Self::sync_payload`] for a shared handle.
    pub fn sync_payload_shared(&self, file: &File, faults: &dyn FaultInjector) -> io::Result<()> {
        faults.check(FaultPoint::PayloadFlush)?;
        file.sync_all()
    }

    pub fn commit(
        &self,
        mut record: CheckpointRecord,
        faults: &dyn FaultInjector,
    ) -> io::Result<CheckpointRecord> {
        let remembered = *self
            .last_generation
            .lock()
            .expect("generation lock poisoned");
        let latest_generation = match remembered {
            Some(generation) => generation,
            None => self.latest()?.map_or(0, |current| current.generation),
        };
        record.generation = latest_generation
            .checked_add(1)
            .ok_or_else(|| io::Error::other("checkpoint generation overflow"))?;
        if record.source_key != self.source_key {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "checkpoint source key does not match store",
            ));
        }
        let committed = self
            .parent
            .join(format!("{}{}", self.metadata_prefix, record.generation));
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let temporary = self.parent.join(format!(
            "{}{}.tmp-{}-{nonce}",
            self.metadata_prefix,
            record.generation,
            std::process::id()
        ));
        let bytes = encode_record(&record);
        let result = (|| {
            let mut file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&temporary)?;
            faults.check(FaultPoint::MetadataWrite)?;
            file.write_all(&bytes)?;
            faults.check(FaultPoint::MetadataFlush)?;
            file.flush()?;
            file.sync_all()?;
            drop(file);
            faults.check(FaultPoint::MetadataCommit)?;
            fs::rename(&temporary, &committed)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
            return result.map(|_| record);
        }
        match remembered {
            // The one generation this store wrote before is the only older
            // file to remove; no directory scan.
            Some(previous) => {
                let _ = fs::remove_file(
                    self.parent
                        .join(format!("{}{previous}", self.metadata_prefix)),
                );
            }
            None => self.remove_generations_before(record.generation),
        }
        *self
            .last_generation
            .lock()
            .expect("generation lock poisoned") = Some(record.generation);
        Ok(record)
    }

    pub fn reset(&self) -> io::Result<File> {
        self.remove_metadata()?;
        let file = self.open_staging()?;
        file.set_len(0)?;
        Ok(file)
    }

    pub fn remove_all(&self) -> io::Result<()> {
        match fs::remove_file(&self.staging) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        self.remove_metadata()
    }

    pub fn remove_metadata(&self) -> io::Result<()> {
        *self
            .last_generation
            .lock()
            .expect("generation lock poisoned") = None;
        if !self.parent.exists() {
            return Ok(());
        }
        let mut first_error = None;
        for entry in fs::read_dir(&self.parent)? {
            let entry = entry?;
            let matches = entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.starts_with(&self.metadata_prefix));
            if matches
                && let Err(error) = fs::remove_file(entry.path())
                && error.kind() != io::ErrorKind::NotFound
                && first_error.is_none()
            {
                first_error = Some(error);
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    pub fn validate_staging(&self, record: &CheckpointRecord) -> io::Result<bool> {
        let mut file = match OpenOptions::new()
            .read(true)
            .write(true)
            .open(&self.staging)
        {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error),
        };
        if file.metadata()?.len() < record.committed_len {
            return Ok(false);
        }
        file.set_len(record.committed_len)?;
        Ok(hash_reader(&mut file)? == record.local_sha256)
    }

    pub fn reconcile_publication(
        &self,
        record: &CheckpointRecord,
    ) -> io::Result<PublicationRecovery> {
        if record.phase != CheckpointPhase::PublicationIntent {
            return Ok(PublicationRecovery::NotPending);
        }
        if !self.destination.exists() {
            return Ok(PublicationRecovery::Pending);
        }
        if file_matches(
            &self.destination,
            record.committed_len,
            &record.local_sha256,
        )? {
            self.remove_all()?;
            return Ok(PublicationRecovery::Completed);
        }
        Ok(PublicationRecovery::Conflict)
    }

    pub fn publish(
        &self,
        record: &CheckpointRecord,
        faults: &dyn FaultInjector,
    ) -> io::Result<Option<PathBuf>> {
        if record.phase != CheckpointPhase::PublicationIntent {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "publication requires a committed publication intent",
            ));
        }
        if !self.validate_staging(record)? {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "staging bytes do not match publication intent",
            ));
        }
        faults.check(FaultPoint::PublicationFence)?;
        fs::hard_link(&self.staging, &self.destination)?;
        faults.check(FaultPoint::PublicationReconcile)?;
        let staging_pending = match fs::remove_file(&self.staging) {
            Ok(()) => None,
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(_) => Some(self.staging.clone()),
        };
        if staging_pending.is_none() {
            let _ = self.remove_metadata();
        }
        Ok(staging_pending)
    }

    fn remove_generations_before(&self, generation: u64) {
        let Ok(entries) = fs::read_dir(&self.parent) else {
            return;
        };
        for entry in entries.flatten() {
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            let Some(suffix) = name.strip_prefix(&self.metadata_prefix) else {
                continue;
            };
            if let Ok(candidate) = suffix.parse::<u64>()
                && candidate < generation
            {
                let _ = fs::remove_file(entry.path());
            }
        }
    }
}

/// Writes all of `bytes` at `offset`, looping over short writes.
pub fn write_all_at(file: &File, mut offset: u64, mut bytes: &[u8]) -> io::Result<()> {
    while !bytes.is_empty() {
        #[cfg(windows)]
        let written = std::os::windows::fs::FileExt::seek_write(file, bytes, offset)?;
        #[cfg(unix)]
        let written = std::os::unix::fs::FileExt::write_at(file, bytes, offset)?;
        if written == 0 {
            return Err(io::ErrorKind::WriteZero.into());
        }
        offset += written as u64;
        bytes = &bytes[written..];
    }
    Ok(())
}

/// Fills `buffer` from `offset` without moving a file cursor.
pub fn read_exact_at(file: &File, mut offset: u64, mut buffer: &mut [u8]) -> io::Result<()> {
    while !buffer.is_empty() {
        #[cfg(windows)]
        let read = std::os::windows::fs::FileExt::seek_read(file, buffer, offset)?;
        #[cfg(unix)]
        let read = std::os::unix::fs::FileExt::read_at(file, buffer, offset)?;
        if read == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        offset += read as u64;
        buffer = &mut buffer[read..];
    }
    Ok(())
}

pub fn sha256_file(path: &Path) -> io::Result<String> {
    let mut file = File::open(path)?;
    hash_reader(&mut file)
}

fn file_matches(path: &Path, expected_len: u64, expected_sha256: &str) -> io::Result<bool> {
    let metadata = fs::metadata(path)?;
    Ok(metadata.len() == expected_len && sha256_file(path)? == expected_sha256)
}

fn hash_reader(reader: &mut impl Read) -> io::Result<String> {
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn encode_record(record: &CheckpointRecord) -> Vec<u8> {
    let phase = match record.phase {
        CheckpointPhase::Downloading => "downloading",
        CheckpointPhase::PublicationIntent => "publication_intent",
    };
    let mut body = format!(
        "version={FORMAT_VERSION}\ngeneration={}\nsource_key={}\ncommitted_len={}\nlocal_sha256={}\nstrong_etag={}\nexpected_total={}\nphase={phase}\n",
        record.generation,
        record.source_key,
        record.committed_len,
        record.local_sha256,
        record
            .strong_etag
            .as_deref()
            .map(hex_encode)
            .unwrap_or_default(),
        record
            .expected_total
            .map(|value| value.to_string())
            .unwrap_or_default(),
    );
    let checksum = format!("{:x}", Sha256::digest(body.as_bytes()));
    body.push_str("checksum=");
    body.push_str(&checksum);
    body.push('\n');
    body.into_bytes()
}

fn decode_record(bytes: &[u8]) -> io::Result<CheckpointRecord> {
    let text = std::str::from_utf8(bytes)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "checkpoint is not UTF-8"))?;
    let checksum_marker = "checksum=";
    let checksum_start = text
        .rfind(checksum_marker)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "checkpoint lacks checksum"))?;
    let body = &text[..checksum_start];
    let checksum = text[checksum_start + checksum_marker.len()..].trim();
    let actual = format!("{:x}", Sha256::digest(body.as_bytes()));
    if actual != checksum {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "checkpoint checksum mismatch",
        ));
    }
    let values: BTreeMap<_, _> = body
        .lines()
        .filter_map(|line| line.split_once('='))
        .collect();
    let required = |name: &str| {
        values
            .get(name)
            .copied()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, format!("missing {name}")))
    };
    if required("version")?.parse::<u32>().ok() != Some(FORMAT_VERSION) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "unsupported checkpoint version",
        ));
    }
    let strong_etag = match required("strong_etag")? {
        "" => None,
        encoded => Some(hex_decode(encoded)?),
    };
    let expected_total = match required("expected_total")? {
        "" => None,
        value => Some(parse_u64("expected_total", value)?),
    };
    let phase = match required("phase")? {
        "downloading" => CheckpointPhase::Downloading,
        "publication_intent" => CheckpointPhase::PublicationIntent,
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid checkpoint phase",
            ));
        }
    };
    Ok(CheckpointRecord {
        generation: parse_u64("generation", required("generation")?)?,
        source_key: required("source_key")?.to_owned(),
        committed_len: parse_u64("committed_len", required("committed_len")?)?,
        local_sha256: required("local_sha256")?.to_owned(),
        strong_etag,
        expected_total,
        phase,
    })
}

fn parse_u64(name: &str, value: &str) -> io::Result<u64> {
    value.parse().map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid integer for {name}"),
        )
    })
}

fn hex_encode(value: &str) -> String {
    value
        .as_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn hex_decode(value: &str) -> io::Result<String> {
    if !value.len().is_multiple_of(2) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid hex string",
        ));
    }
    let mut bytes = Vec::with_capacity(value.len() / 2);
    for pair in value.as_bytes().as_chunks::<2>().0 {
        let text = std::str::from_utf8(pair)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid hex string"))?;
        bytes.push(
            u8::from_str_radix(text, 16)
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid hex string"))?,
        );
    }
    String::from_utf8(bytes)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid encoded UTF-8"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct FailOnce(Mutex<Option<FaultPoint>>);

    impl FaultInjector for FailOnce {
        fn check(&self, point: FaultPoint) -> io::Result<()> {
            let mut selected = self.0.lock().unwrap();
            if selected.as_ref() == Some(&point) {
                *selected = None;
                return Err(io::Error::other("injected fault"));
            }
            Ok(())
        }
    }

    fn temp_dir(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "fetchpath-storage-{label}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn record(key: &str, bytes: &[u8]) -> CheckpointRecord {
        CheckpointRecord::downloading(
            key.to_owned(),
            bytes.len() as u64,
            format!("{:x}", Sha256::digest(bytes)),
            Some("\"v1\"".into()),
            Some(bytes.len() as u64),
        )
    }

    #[test]
    fn failed_metadata_stages_never_replace_the_last_committed_generation() {
        for point in [
            FaultPoint::MetadataWrite,
            FaultPoint::MetadataFlush,
            FaultPoint::MetadataCommit,
        ] {
            let dir = temp_dir("metadata-fault");
            let destination = dir.join("file.bin");
            let store = CheckpointStore::new(&destination, "source").unwrap();
            let first = store.commit(record("source", b"abc"), &NoFaults).unwrap();
            let failure = FailOnce(Mutex::new(Some(point)));
            assert!(store.commit(record("source", b"abcdef"), &failure).is_err());
            assert_eq!(store.latest().unwrap(), Some(first));
            fs::remove_dir_all(dir).unwrap();
        }
    }

    #[test]
    fn retained_bytes_must_match_the_committed_digest() {
        let dir = temp_dir("digest");
        let destination = dir.join("file.bin");
        let store = CheckpointStore::new(&destination, "source").unwrap();
        fs::write(store.staging(), b"abcdef").unwrap();
        let committed = store
            .commit(record("source", b"abcdef"), &NoFaults)
            .unwrap();
        assert!(store.validate_staging(&committed).unwrap());
        fs::write(store.staging(), b"abcxef").unwrap();
        assert!(!store.validate_staging(&committed).unwrap());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn publication_after_fence_is_reconciled_on_restart() {
        let dir = temp_dir("publication");
        let destination = dir.join("file.bin");
        let store = CheckpointStore::new(&destination, "source").unwrap();
        let bytes = b"published bytes";
        fs::write(store.staging(), bytes).unwrap();
        let mut intent = record("source", bytes);
        intent.phase = CheckpointPhase::PublicationIntent;
        let intent = store.commit(intent, &NoFaults).unwrap();
        let failure = FailOnce(Mutex::new(Some(FaultPoint::PublicationReconcile)));
        assert!(store.publish(&intent, &failure).is_err());
        assert_eq!(fs::read(&destination).unwrap(), bytes);
        assert_eq!(
            store.reconcile_publication(&intent).unwrap(),
            PublicationRecovery::Completed
        );
        assert!(!store.staging().exists());
        fs::remove_dir_all(dir).unwrap();
    }

    fn metadata_files(dir: &Path) -> usize {
        fs::read_dir(dir)
            .unwrap()
            .flatten()
            .filter(|entry| entry.file_name().to_string_lossy().contains(".checkpoint."))
            .count()
    }

    #[test]
    fn a_store_remembers_its_generation_and_keeps_one_metadata_file() {
        let dir = temp_dir("remembered-generation");
        let destination = dir.join("file.bin");
        let store = CheckpointStore::new(&destination, "source").unwrap();
        for expected in 1..=4 {
            let committed = store.commit(record("source", b"abc"), &NoFaults).unwrap();
            assert_eq!(committed.generation, expected);
            assert_eq!(metadata_files(&dir), 1, "the previous generation is gone");
        }
        assert_eq!(store.latest().unwrap().unwrap().generation, 4);
        // A fresh store finds the newest generation on disk once, then continues.
        let reopened = CheckpointStore::new(&destination, "source").unwrap();
        let next = reopened
            .commit(record("source", b"abcd"), &NoFaults)
            .unwrap();
        assert_eq!(next.generation, 5);
        assert_eq!(metadata_files(&dir), 1);
        // Removing the metadata forgets the generation, so numbering restarts.
        reopened.remove_metadata().unwrap();
        assert_eq!(metadata_files(&dir), 0);
        let restarted = reopened.commit(record("source", b"a"), &NoFaults).unwrap();
        assert_eq!(restarted.generation, 1);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn positional_writes_land_out_of_order_and_read_back() {
        let dir = temp_dir("positional");
        let destination = dir.join("file.bin");
        let store = CheckpointStore::new(&destination, "source").unwrap();
        let file = store.open_staging().unwrap();
        // Later bytes first, then the ones before them, on one shared handle.
        store
            .write_payload_at(&file, 6, b"world", &NoFaults)
            .unwrap();
        store
            .write_payload_at(&file, 0, b"hello ", &NoFaults)
            .unwrap();
        store.sync_payload_shared(&file, &NoFaults).unwrap();
        assert_eq!(fs::read(store.staging()).unwrap(), b"hello world");
        let mut buffer = [0_u8; 5];
        read_exact_at(&file, 6, &mut buffer).unwrap();
        assert_eq!(&buffer, b"world");
        assert!(read_exact_at(&file, 8, &mut [0_u8; 9]).is_err());
        fs::remove_dir_all(dir).unwrap();
    }
}
