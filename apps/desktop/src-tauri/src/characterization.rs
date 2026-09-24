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
