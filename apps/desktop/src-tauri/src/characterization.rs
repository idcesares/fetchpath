//! Characterization of the desktop queue before it moves into
//! `fetchpath-session` (FP-048, FP-049).
//!
//! These tests pin what the queue does today: the files a 0.1.0 install
//! leaves on disk, how each saved state comes back after a restart, which
//! failures are retried automatically, how rates are estimated, and the JSON
//! field names the interface reads. They pass on the unchanged code; after the
//! move they must pass unchanged. A test named `finding_…` pins behavior we
//! believe is wrong (see docs/development/ENGINE-SESSION.md); fixing it is
//! separate work that updates the test on purpose.

use super::*;
use serde_json::Value;

const QUEUE_FIXTURE: &str = include_str!("../tests/fixtures/queue-0.1.0.json");
const SETTINGS_FIXTURE: &str = include_str!("../tests/fixtures/settings-0.1.0.json");

/// A fixed "now" for restore, after every fixture timestamp except `FAR`.
const NOW: u64 = 1_800_000_000_000;
/// 1 January 2100: a schedule that never falls due during a test.
const FAR: u64 = 4_102_444_800_000;

fn queue_fixture(dir: &Path) -> String {
    let escaped = dir.display().to_string().replace('\\', "\\\\");
    QUEUE_FIXTURE.replace("{DIR}", &escaped)
}

fn fixture_queue(dir: &Path) -> PersistedQueue {
    serde_json::from_str(&queue_fixture(dir)).expect("fixture parses as a 0.1.0 queue")
}

fn fixture_record(dir: &Path, id: &str) -> PersistedRecord {
    fixture_queue(dir)
        .records
        .into_iter()
        .find(|record| record.id == id)
        .unwrap_or_else(|| panic!("fixture has no record {id}"))
}

fn id(n: u8) -> String {
    format!("00000000-0000-4000-8000-{n:012}")
}

#[test]
fn the_0_1_0_queue_file_is_exactly_what_the_queue_writes() {
    let dir = tempfile::tempdir().unwrap();
    let text = queue_fixture(dir.path());
    let as_written: Value = serde_json::from_str(&text).unwrap();
    let parsed: PersistedQueue = serde_json::from_str(&text).unwrap();
    // Re-serializing through the production types reproduces every field and
    // value: nothing in the file is ignored, and nothing the writer emits is
    // missing from it.
    assert_eq!(serde_json::to_value(&parsed).unwrap(), as_written);
    assert_eq!(parsed.schema_version, QUEUE_SCHEMA_VERSION);
    assert_eq!(parsed.records.len(), 14);
}

#[test]
fn a_non_ascii_destination_survives_a_save_and_load() {
    let dir = tempfile::tempdir().unwrap();
    let saved = fixture_record(dir.path(), &id(1));
    let restored = QueueRecord::restore(saved, NOW, None, None);
    let path = dir.path().join("queue.json");
    let state = QueueState {
        records: vec![restored],
        link_reviews: Vec::new(),
    };
    save_persisted(&path, &state).unwrap();
    let reloaded = load_persisted(&path).unwrap().expect("queue reloads");
    assert_eq!(
        reloaded.records[0].destination,
        dir.path().join("résumé ✓.pdf").display().to_string()
    );
}

#[test]
fn the_0_1_0_settings_file_loads_every_choice_and_writes_back_identically() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings-v1.json");
    fs::write(&path, SETTINGS_FIXTURE).unwrap();

    let loaded = settings::load(&path);
    assert!(!loaded.repaired, "a valid 0.1.0 file is not a repair");
    assert_eq!(
        loaded.settings,
        Settings {
            max_active_downloads: 5,
            default_destination_dir: Some(r"C:\Downloads\Fetchpath".into()),
            auto_retry: false,
            auto_retry_max_attempts: 5,
            auto_retry_base_delay_seconds: 30,
            close_to_tray: false,
            power_mode: true,
            media_tools_dir: None,
            confirm_remove_completed: false,
            theme: settings::Theme::Dark,
            onboarding_completed: true,
        }
    );

    settings::save(&path, &loaded.settings).unwrap();
    let written: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    let original: Value = serde_json::from_str(SETTINGS_FIXTURE).unwrap();
    assert_eq!(written, original);
}

/// One row of the restore table: what a saved record becomes on load.
struct Restored {
    record: u8,
    state: &'static str,
    action: Option<&'static str>,
    retryable: bool,
    error: Option<&'static str>,
    has_job: bool,
}

const PRIVATE_LINK: &str = "Paste a refreshed link because private query values were not saved.";
const RECAPTURE: &str =
    "Send this download from the browser again because its protected context is unavailable.";
const RECOVERED: &str = "Recovered after Fetchpath restarted.";
const NO_MEDIA_TOOLS: &str = "Media tools are unavailable. Configure them to retry this download.";

#[test]
fn every_saved_state_comes_back_after_a_restart_as_it_did_in_0_1_0() {
    let dir = tempfile::tempdir().unwrap();
    // A browser store with no secrets: a captured download's protected
    // context is gone, as after the secret expired or was removed.
    let store = BridgeStore::new(dir.path().to_path_buf());
    let table = [
        Restored { record: 1, state: "completed", action: None, retryable: false, error: None, has_job: false },
        Restored { record: 2, state: "failed", action: Some("retry"), retryable: true, error: Some("source.transfer_failed: [7] Couldn't connect to server"), has_job: false },
        Restored { record: 3, state: "failed", action: Some("choose_new_path"), retryable: true, error: Some("storage.destination_conflict: a file already exists at the destination"), has_job: false },
        Restored { record: 4, state: "needs_source", action: Some("edit_link"), retryable: true, error: Some(PRIVATE_LINK), has_job: false },
        Restored { record: 5, state: "paused", action: None, retryable: false, error: None, has_job: false },
        Restored { record: 6, state: "needs_source", action: Some("edit_link"), retryable: true, error: Some(PRIVATE_LINK), has_job: false },
        Restored { record: 7, state: "scheduled", action: None, retryable: false, error: Some(RECOVERED), has_job: true },
        // Running when Fetchpath stopped, schedule already past: queued again.
        Restored { record: 8, state: "queued", action: None, retryable: false, error: Some(RECOVERED), has_job: true },
        Restored { record: 9, state: "needs_source", action: Some("recapture"), retryable: true, error: Some(RECAPTURE), has_job: false },
        Restored { record: 10, state: "failed", action: Some("check_checksum"), retryable: true, error: Some(UNREADABLE_CHECKSUM), has_job: false },
        Restored { record: 11, state: "scheduled", action: None, retryable: false, error: Some(RECOVERED), has_job: true },
        Restored { record: 12, state: "completed", action: None, retryable: false, error: None, has_job: false },
        Restored { record: 13, state: "failed", action: Some("configure_media_tools"), retryable: true, error: Some(NO_MEDIA_TOOLS), has_job: false },
        Restored { record: 14, state: "failed", action: Some("retry"), retryable: true, error: Some("source.transfer_failed: [7] Couldn't connect to server"), has_job: false },
    ];

    let queue = fixture_queue(dir.path());
    assert_eq!(queue.records.len(), table.len());
    for (saved, expected) in queue.records.into_iter().zip(&table) {
        assert_eq!(saved.id, id(expected.record), "fixture order");
        let saved_view = saved.view.clone();
        let restored = QueueRecord::restore(saved, NOW, Some(&store), None);
        let row = format!("record {}", expected.record);
        assert_eq!(restored.view.state, expected.state, "{row}: state");
        assert_eq!(restored.view.action.as_deref(), expected.action, "{row}: action");
        assert_eq!(restored.view.retryable, expected.retryable, "{row}: retryable");
        assert_eq!(restored.view.error.as_deref(), expected.error, "{row}: error");
        assert_eq!(restored.job.is_some(), expected.has_job, "{row}: job");
        // Everything else about the row is carried over untouched.
        assert_eq!(restored.view.bytes_received, saved_view.bytes_received, "{row}: bytes");
        assert_eq!(restored.view.total_bytes, saved_view.total_bytes, "{row}: total");
        assert_eq!(restored.view.expected_sha256, saved_view.expected_sha256, "{row}: checksum");
        assert_eq!(restored.view.kind, saved_view.kind, "{row}: kind");
        assert_eq!(restored.attempt, saved_view.attempt, "{row}: attempt");
        assert_eq!(restored.rate.bytes_per_second(), None, "{row}: no rate after restart");
        assert_eq!(restored.retry_at_ms, None, "{row}: no retry pending after restart");
    }
}

#[test]
fn a_private_link_is_never_restored_as_a_live_source() {
    let dir = tempfile::tempdir().unwrap();
    for record in [4, 6] {
        let restored = QueueRecord::restore(fixture_record(dir.path(), &id(record)), NOW, None, None);
        assert_eq!(restored.live_url, None, "record {record}");
        assert_eq!(restored.restart_url, None, "record {record}");
    }
    let public = QueueRecord::restore(fixture_record(dir.path(), &id(7)), NOW, None, None);
    assert_eq!(public.live_url.as_deref(), Some("https://example.test/files/scheduled.bin"));
    assert_eq!(public.not_before_ms, Some(FAR));
}

fn write_queue(path: &Path, value: &Value) {
    fs::write(path, serde_json::to_vec_pretty(value).unwrap()).unwrap();
}

fn fixture_value(dir: &Path) -> Value {
    serde_json::from_str(&queue_fixture(dir)).unwrap()
}

#[test]
fn saving_replaces_the_file_through_a_backup_and_leaves_no_temporary() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("queue.json");
    let state = QueueState::default();
    save_persisted(&path, &state).unwrap();
    assert!(!path.with_extension("json.bak").exists(), "first save has nothing to back up");
    save_persisted(&path, &state).unwrap();
    assert!(path.exists());
    assert!(path.with_extension("json.bak").exists(), "second save keeps the previous file");
    assert!(!path.with_extension("json.new").exists());
}

#[test]
fn a_corrupt_queue_file_falls_back_to_its_backup() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("queue.json");
    fs::write(&path, b"{\"schemaVersion\":1,\"records\":[").unwrap();
    fs::write(path.with_extension("json.bak"), queue_fixture(dir.path())).unwrap();
    let loaded = load_persisted(&path).unwrap().expect("backup loads");
    assert_eq!(loaded.records.len(), 14);
}

#[test]
fn a_corrupt_queue_and_backup_start_an_empty_queue_without_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("queue.json");
    fs::write(&path, b"not json").unwrap();
    fs::write(path.with_extension("json.bak"), b"also not json").unwrap();
    assert!(load_persisted(&path).unwrap().is_none());
}

#[test]
fn unknown_fields_from_a_newer_build_are_ignored_and_known_ones_kept() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("queue.json");
    let mut value = fixture_value(dir.path());
    value["futureQueueField"] = Value::from(true);
    value["records"][0]["futureRecordField"] = Value::from("x");
    value["records"][0]["view"]["futureViewField"] = Value::from(7);
    write_queue(&path, &value);
    let loaded = load_persisted(&path).unwrap().expect("loads");
    assert_eq!(loaded.records.len(), 14);
    assert_eq!(loaded.records[0].view.state, "completed");
}

#[test]
fn fields_added_after_the_first_queue_format_default_when_missing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("queue.json");
    let mut value = fixture_value(dir.path());
    let record = value["records"][11].as_object_mut().unwrap(); // record 12, media
    for key in ["credentialRef", "mediaVariantId", "mediaQuality"] {
        record.remove(key);
    }
    let view = record["view"].as_object_mut().unwrap();
    for key in ["totalBytes", "attempt", "kind", "qualityLabel"] {
        view.remove(key);
    }
    write_queue(&path, &value);
    let loaded = load_persisted(&path).unwrap().expect("loads");
    let record = &loaded.records[11];
    assert_eq!(record.id, id(12));
    assert_eq!(record.credential_ref, None);
    assert_eq!(record.media_variant_id, None);
    assert_eq!(record.view.total_bytes, None);
    assert_eq!(record.view.attempt, 0);
    assert_eq!(record.view.kind, "file", "a record without a kind reads as a file");
    assert_eq!(record.view.quality_label, None);
}

/// Finding F1: a queue written by a newer schema version is treated as no
/// queue at all. The first save moves it to the backup and the second save
/// deletes it, so opening an older build after a newer one loses the queue.
#[test]
fn finding_f1_a_queue_from_a_newer_schema_is_lost_after_two_saves() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("queue.json");
    let mut value = fixture_value(dir.path());
    value["schemaVersion"] = Value::from(QUEUE_SCHEMA_VERSION + 1);
    write_queue(&path, &value);

    let jobs = DesktopJobs::load(path.clone(), 1).unwrap();
    assert!(jobs.list().unwrap().is_empty(), "newer queue is not shown");
    let backup = fs::read_to_string(path.with_extension("json.bak")).unwrap();
    assert!(backup.contains("\"schemaVersion\": 2"), "first save keeps it as the backup");

    jobs.list().unwrap();
    let backup = fs::read_to_string(path.with_extension("json.bak")).unwrap();
    assert!(!backup.contains("\"schemaVersion\": 2"), "second save has deleted it");
}
