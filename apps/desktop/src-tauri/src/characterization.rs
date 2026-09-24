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
