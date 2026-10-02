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
    assert_eq!(
        parsed.schema_version, 1,
        "the original fixture stays readable after the v2 queue migration"
    );
    assert_eq!(parsed.records.len(), 14);
}

#[test]
fn a_non_ascii_destination_survives_a_save_and_load() {
    let dir = tempfile::tempdir().unwrap();
    let saved = fixture_record(dir.path(), &id(1));
    let restored = QueueRecord::restore(saved, NOW, None, None, Path::new("queue-v1.json"));
    let path = dir.path().join("queue.json");
    let state = QueueState {
        records: vec![restored],
        link_reviews: Vec::new(),
    };
    save_persisted(&path, &state).unwrap();
    let reloaded = load_persisted(&path)
        .unwrap()
        .current()
        .expect("queue reloads");
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
            // Added after 0.1.0 (FP-053); a 0.1.0 file leaves it off.
            start_engine_at_sign_in: false,
            // Added after 0.1.0 (FP-064); a 0.1.0 file has none.
            rules: Vec::new(),
            // Added after 0.1.0 (FP-032); a 0.1.0 file has the default.
            cache_quota_bytes: settings::DEFAULT_CACHE_QUOTA_BYTES,
            density: settings::Density::Comfortable,
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
        Restored {
            record: 1,
            state: "completed",
            action: None,
            retryable: false,
            error: None,
            has_job: false,
        },
        Restored {
            record: 2,
            state: "failed",
            action: Some("retry"),
            retryable: true,
            error: Some("source.transfer_failed: [7] Couldn't connect to server"),
            has_job: false,
        },
        Restored {
            record: 3,
            state: "failed",
            action: Some("choose_new_path"),
            retryable: true,
            error: Some("storage.destination_conflict: a file already exists at the destination"),
            has_job: false,
        },
        Restored {
            record: 4,
            state: "needs_source",
            action: Some("edit_link"),
            retryable: true,
            error: Some(PRIVATE_LINK),
            has_job: false,
        },
        Restored {
            record: 5,
            state: "paused",
            action: None,
            retryable: false,
            error: None,
            has_job: false,
        },
        Restored {
            record: 6,
            state: "needs_source",
            action: Some("edit_link"),
            retryable: true,
            error: Some(PRIVATE_LINK),
            has_job: false,
        },
        Restored {
            record: 7,
            state: "scheduled",
            action: None,
            retryable: false,
            error: Some(RECOVERED),
            has_job: true,
        },
        // Running when Fetchpath stopped, schedule already past: queued again.
        Restored {
            record: 8,
            state: "queued",
            action: None,
            retryable: false,
            error: Some(RECOVERED),
            has_job: true,
        },
        Restored {
            record: 9,
            state: "needs_source",
            action: Some("recapture"),
            retryable: true,
            error: Some(RECAPTURE),
            has_job: false,
        },
        Restored {
            record: 10,
            state: "failed",
            action: Some("check_checksum"),
            retryable: true,
            error: Some(UNREADABLE_CHECKSUM),
            has_job: false,
        },
        Restored {
            record: 11,
            state: "scheduled",
            action: None,
            retryable: false,
            error: Some(RECOVERED),
            has_job: true,
        },
        Restored {
            record: 12,
            state: "completed",
            action: None,
            retryable: false,
            error: None,
            has_job: false,
        },
        Restored {
            record: 13,
            state: "failed",
            action: Some("configure_media_tools"),
            retryable: true,
            error: Some(NO_MEDIA_TOOLS),
            has_job: false,
        },
        Restored {
            record: 14,
            state: "failed",
            action: Some("retry"),
            retryable: true,
            error: Some("source.transfer_failed: [7] Couldn't connect to server"),
            has_job: false,
        },
    ];

    let queue = fixture_queue(dir.path());
    assert_eq!(queue.records.len(), table.len());
    for (saved, expected) in queue.records.into_iter().zip(&table) {
        assert_eq!(saved.id, id(expected.record), "fixture order");
        let saved_view = saved.view.clone();
        let restored =
            QueueRecord::restore(saved, NOW, Some(&store), None, Path::new("queue-v1.json"));
        let row = format!("record {}", expected.record);
        assert_eq!(restored.view.state, expected.state, "{row}: state");
        assert_eq!(
            restored.view.action.as_deref(),
            expected.action,
            "{row}: action"
        );
        assert_eq!(
            restored.view.retryable, expected.retryable,
            "{row}: retryable"
        );
        assert_eq!(
            restored.view.error.as_deref(),
            expected.error,
            "{row}: error"
        );
        assert_eq!(restored.job.is_some(), expected.has_job, "{row}: job");
        // Everything else about the row is carried over untouched.
        assert_eq!(
            restored.view.bytes_received, saved_view.bytes_received,
            "{row}: bytes"
        );
        assert_eq!(
            restored.view.total_bytes, saved_view.total_bytes,
            "{row}: total"
        );
        assert_eq!(
            restored.view.expected_sha256, saved_view.expected_sha256,
            "{row}: checksum"
        );
        assert_eq!(restored.view.kind, saved_view.kind, "{row}: kind");
        assert_eq!(restored.attempt, saved_view.attempt, "{row}: attempt");
        assert_eq!(
            restored.rate.bytes_per_second(),
            None,
            "{row}: no rate after restart"
        );
        assert_eq!(
            restored.retry_at_ms, None,
            "{row}: no retry pending after restart"
        );
    }
}

#[test]
fn a_private_link_is_never_restored_as_a_live_source() {
    let dir = tempfile::tempdir().unwrap();
    for record in [4, 6] {
        let restored = QueueRecord::restore(
            fixture_record(dir.path(), &id(record)),
            NOW,
            None,
            None,
            &dir.path().join("queue-v1.json"),
        );
        assert_eq!(restored.live_url, None, "record {record}");
        assert_eq!(restored.restart_url, None, "record {record}");
    }
    let public = QueueRecord::restore(
        fixture_record(dir.path(), &id(7)),
        NOW,
        None,
        None,
        &dir.path().join("queue-v1.json"),
    );
    assert_eq!(
        public.live_url.as_deref(),
        Some("https://example.test/files/scheduled.bin")
    );
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
    assert!(
        !path.with_extension("json.bak").exists(),
        "first save has nothing to back up"
    );
    save_persisted(&path, &state).unwrap();
    assert!(path.exists());
    assert!(
        path.with_extension("json.bak").exists(),
        "second save keeps the previous file"
    );
    assert!(!path.with_extension("json.new").exists());
}

#[test]
fn a_corrupt_queue_file_falls_back_to_its_backup() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("queue.json");
    fs::write(&path, b"{\"schemaVersion\":1,\"records\":[").unwrap();
    fs::write(path.with_extension("json.bak"), queue_fixture(dir.path())).unwrap();
    let loaded = load_persisted(&path)
        .unwrap()
        .current()
        .expect("backup loads");
    assert_eq!(loaded.records.len(), 14);
}

#[test]
fn a_corrupt_queue_and_backup_start_an_empty_queue_without_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("queue.json");
    fs::write(&path, b"not json").unwrap();
    fs::write(path.with_extension("json.bak"), b"also not json").unwrap();
    assert!(load_persisted(&path).unwrap().current().is_none());
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
    let loaded = load_persisted(&path).unwrap().current().expect("loads");
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
    let loaded = load_persisted(&path).unwrap().current().expect("loads");
    let record = &loaded.records[11];
    assert_eq!(record.id, id(12));
    assert_eq!(record.credential_ref, None);
    assert_eq!(record.media_variant_id, None);
    assert_eq!(record.view.total_bytes, None);
    assert_eq!(record.view.attempt, 0);
    assert_eq!(
        record.view.kind, "file",
        "a record without a kind reads as a file"
    );
    assert_eq!(record.view.quality_label, None);
}

/// Finding F1, fixed by FP-070: a queue written by a newer schema version
/// used to be treated as no queue at all, moved to the backup by the first
/// save and deleted by the second. Now it is shown without being changed,
/// nothing starts, and no save touches the file or its backup.
#[test]
fn finding_f1_a_queue_from_a_newer_schema_is_kept_byte_for_byte() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("queue.json");
    let backup = path.with_extension("json.bak");
    let mut value = fixture_value(dir.path());
    value["schemaVersion"] = Value::from(QUEUE_SCHEMA_VERSION + 1);
    write_queue(&path, &value);
    fs::write(&backup, b"an older save, also kept").unwrap();
    let original = fs::read(&path).unwrap();

    let saved_states: std::collections::BTreeMap<String, String> = value["records"]
        .as_array()
        .unwrap()
        .iter()
        .map(|record| {
            (
                record["id"].as_str().unwrap().to_owned(),
                record["view"]["state"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    for _ in 0..3 {
        let jobs = Session::load(path.clone(), 4).unwrap();
        assert!(jobs.read_only().unwrap().contains("newer version"));
        let shown = jobs.list().unwrap();
        assert_eq!(shown.len(), 14, "the newer build's jobs are shown");
        let shown_states: std::collections::BTreeMap<String, String> = shown
            .iter()
            .map(|job| (job.job_id.clone(), job.state.clone()))
            .collect();
        assert_eq!(
            shown_states, saved_states,
            "states as the newer build saved them"
        );
        assert!(
            shown.iter().all(|job| job.bytes_per_second.is_none()),
            "nothing is running"
        );
        assert!(!jobs.has_own_work(), "the engine may leave when idle");
        let mut state = jobs.inner.lock().unwrap();
        assert!(
            jobs.save_locked(&mut state).is_ok(),
            "a routine save is a no-op"
        );
        let mut durable = jobs.durable.lock().unwrap();
        assert!(
            jobs.write_locked(&state, &mut durable).is_err(),
            "a write is refused outright"
        );
    }
    assert_eq!(fs::read(&path).unwrap(), original, "queue untouched");
    assert_eq!(fs::read(&backup).unwrap(), b"an older save, also kept");
    assert!(!path.with_extension("json.new").exists());
    assert!(
        !dir.path().join(ENGINE_FILE).exists(),
        "no engine journal written"
    );
}

/// A queue whose newer format cannot be read at all is still not replaced,
/// and a newer backup is not passed over for nothing.
#[test]
fn an_unreadable_newer_queue_and_a_newer_backup_are_kept_too() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("queue.json");
    let unreadable = format!(
        "{{\"schemaVersion\": {}, \"jobs\": \"a shape this build does not know\"}}",
        QUEUE_SCHEMA_VERSION + 1
    );
    fs::write(&path, &unreadable).unwrap();
    let jobs = Session::load(path.clone(), 4).unwrap();
    assert!(jobs.read_only().is_some());
    assert!(jobs.list().unwrap().is_empty());
    assert_eq!(fs::read_to_string(&path).unwrap(), unreadable);

    // Main file lost, newer backup left: still read-only, still kept.
    fs::remove_file(&path).unwrap();
    let backup = path.with_extension("json.bak");
    fs::write(&backup, &unreadable).unwrap();
    let jobs = Session::load(path.clone(), 4).unwrap();
    assert!(jobs.read_only().is_some());
    jobs.list().unwrap();
    assert!(!path.exists(), "no new queue written in its place");
    assert_eq!(fs::read_to_string(&backup).unwrap(), unreadable);
}

/// Review finding: a build from before FP-070 could leave the newer list as
/// the backup under a queue of its own. The newer backup still decides, so
/// the next save cannot delete it.
#[test]
fn a_newer_backup_behind_a_current_queue_is_kept() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("queue.json");
    let backup = path.with_extension("json.bak");
    let mut newer = fixture_value(dir.path());
    newer["schemaVersion"] = Value::from(QUEUE_SCHEMA_VERSION + 1);
    write_queue(&backup, &newer);
    fs::write(&path, b"{\"schemaVersion\": 1, \"records\": []}").unwrap();
    let (main_before, backup_before) = (fs::read(&path).unwrap(), fs::read(&backup).unwrap());

    let jobs = Session::load(path.clone(), 4).unwrap();
    assert!(jobs.read_only().is_some());
    assert_eq!(jobs.list().unwrap().len(), 14, "the newer list is shown");
    assert_eq!(fs::read(&path).unwrap(), main_before);
    assert_eq!(fs::read(&backup).unwrap(), backup_before);
}

/// A version written in a form this build never uses can only come from a
/// later build; it is kept rather than read as a corrupt file and replaced.
#[test]
fn a_version_this_build_cannot_read_is_treated_as_newer() {
    for version in ["\"2\"", "2.5", "18446744073709551616", "-1"] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("queue.json");
        let text = format!("{{\"schemaVersion\": {version}, \"records\": []}}");
        fs::write(&path, &text).unwrap();
        let jobs = Session::load(path.clone(), 4).unwrap();
        let reason = jobs.read_only().unwrap_or_else(|| panic!("{version}"));
        assert!(!reason.contains("format"), "no number to show: {reason}");
        jobs.list().unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), text, "{version}");
        assert!(!path.with_extension("json.bak").exists(), "{version}");
    }
    // Missing, null or 0 is a damaged file, as before: started empty.
    for version in ["null", "0"] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("queue.json");
        fs::write(
            &path,
            format!("{{\"schemaVersion\": {version}, \"records\": []}}"),
        )
        .unwrap();
        assert!(
            Session::load(path, 4).unwrap().read_only().is_none(),
            "{version}"
        );
    }
}

#[test]
fn each_failure_maps_to_the_step_a_person_or_the_queue_takes_next() {
    let table = [
        (
            "integrity.checksum_mismatch: expected aa, received bb",
            "check_checksum",
        ),
        (UNREADABLE_CHECKSUM, "check_checksum"),
        (
            "media.source_expired: the page link expired",
            "refresh_source",
        ),
        ("media.unknown_variant: 137+140", "refresh_source"),
        (
            "media.helper_unavailable: yt-dlp not found",
            "configure_media_tools",
        ),
        ("storage.destination_conflict: exists", "choose_new_path"),
        // The media adapter's own spelling: a bare name, read as media.<name>.
        (
            "source_expired: This media session expired.",
            "refresh_source",
        ),
        (
            "helper_unavailable: Media tools are unavailable.",
            "configure_media_tools",
        ),
        (
            "destination_conflict: A file already exists at this destination.",
            "choose_new_path",
        ),
        // Finding F3, fixed (FP-051): words elsewhere in a message no longer
        // choose the action. A message without a code offers a retry; the
        // session attaches its own action where it writes such a message.
        ("A file already exists at the destination.", "retry"),
        ("input.invalid_url: not http", "edit_link"),
        ("input.invalid_destination: reserved name", "edit_link"),
        ("source.transfer_failed: HTTP status 404", "edit_link"),
        ("source.transfer_failed: HTTP status 503", "retry"),
        ("source.transfer_failed: [28] Timeout was reached", "retry"),
        // A checksum problem outranks the HTTP status in the same message.
        (
            "integrity.checksum_mismatch after HTTP status 404",
            "check_checksum",
        ),
    ];
    for (error, action) in table {
        assert_eq!(action_for_error(error), action, "{error}");
    }
}

/// Finding F2, fixed (FP-051): an unrecognized or internal failure still
/// offers a person a retry, but the queue never retries it by itself, as the
/// job contract (§9) requires.
#[test]
fn unrecognized_and_internal_errors_offer_a_retry_but_are_never_retried_automatically() {
    for error in [
        "",
        "internal.unknown: something new",
        "internal.metadata_failure: journal",
        "Something went wrong.",
    ] {
        assert_eq!(action_for_error(error), "retry", "{error:?}");
        assert!(
            !retried_automatically(&error_code(error)),
            "{error:?} must not be retried by the queue"
        );
    }
    let dir = tempfile::tempdir().unwrap();
    let jobs = Session::in_memory(3);
    let mut unknown = failed_record(dir.path(), "unknown.bin", "retry", 0);
    unknown.view.error = Some("internal.metadata_failure: journal".into());
    let mut uncoded = failed_record(dir.path(), "uncoded.bin", "retry", 0);
    uncoded.view.error = Some("Something went wrong.".into());
    let mut state = QueueState {
        records: vec![
            unknown,
            uncoded,
            failed_record(dir.path(), "transport.bin", "retry", 0),
        ],
        link_reviews: Vec::new(),
    };
    jobs.schedule_automatic_retries(&mut state, &Settings::default());
    assert_eq!(state.records[0].view.state, "failed");
    assert_eq!(state.records[1].view.state, "failed");
    assert_eq!(
        state.records[2].view.state, "scheduled",
        "transport failures still are"
    );
}

#[test]
fn a_failure_code_is_read_from_the_start_of_its_message_only() {
    let table = [
        ("input.invalid_url: only http", "input.invalid_url"),
        (
            "source.transfer_failed: HTTP status 404",
            "source.transfer_failed",
        ),
        ("storage.failed at C:\\x: denied", "storage.failed"),
        (
            "integrity.checksum_mismatch after HTTP status 404",
            "integrity.checksum_mismatch",
        ),
        (
            "helper_crash: The media helper stopped.",
            "media.helper_crash",
        ),
        ("  source_expired: expired", "media.source_expired"),
        // A code later in the message is not this failure's code.
        ("Something failed: input.invalid_url", "internal.unknown"),
        ("cancelled", "internal.unknown"),
        ("Could not start this download (x).", "internal.unknown"),
        ("", "internal.unknown"),
    ];
    for (error, code) in table {
        assert_eq!(error_code(error), code, "{error:?}");
    }
    // Only failed rows carry a code; the session's own messages take theirs
    // from the action it attached.
    let dir = tempfile::tempdir().unwrap();
    let mut view = QueueRecord::new(
        "https://example.test/a.bin".into(),
        dir.path().join("a.bin"),
        None,
    )
    .view;
    view.state = "queued".into();
    view.error = Some("source.transfer_failed: x".into());
    assert_eq!(failure_code(&view), None);
    view.state = "failed".into();
    assert_eq!(
        failure_code(&view).as_deref(),
        Some("source.transfer_failed")
    );
    view.error = Some("A file already exists at this destination.".into());
    view.action = Some("choose_new_path".into());
    assert_eq!(
        failure_code(&view).as_deref(),
        Some("storage.destination_conflict")
    );
}

#[test]
fn a_batch_that_fails_on_one_link_queues_none_of_them() {
    let dir = tempfile::tempdir().unwrap();
    let jobs = Session::in_memory(1);
    let draft = |url: &str, name: &str| JobDraft {
        url: url.into(),
        destination: dir.path().join(name).display().to_string(),
        not_before_ms: None,
        checksum: None,
    };
    let error = jobs
        .enqueue(vec![
            draft("http://127.0.0.1:9/first.bin", "first.bin"),
            draft("ftp://example.test/second.bin", "second.bin"),
        ])
        .unwrap_err();
    assert!(error.contains("HTTP"), "{error}");
    assert!(
        jobs.list().unwrap().is_empty(),
        "the valid first link was queued anyway"
    );
}

#[test]
fn only_failed_rows_get_a_recovery_action() {
    let dir = tempfile::tempdir().unwrap();
    let mut view = QueueRecord::new(
        "https://example.test/a.bin".into(),
        dir.path().join("a.bin"),
        None,
    )
    .view;
    for state in [
        "queued",
        "scheduled",
        "running",
        "paused",
        "cancelling",
        "completed",
        "cancelled",
        "needs_source",
    ] {
        view.state = state.into();
        view.error = Some("source.transfer_failed: HTTP status 404".into());
        assert_eq!(recovery_action(&view), (None, false), "{state}");
    }
    view.state = "failed".into();
    view.error = None;
    assert_eq!(recovery_action(&view), (Some("retry".into()), true));
}

fn failed_record(dir: &Path, name: &str, action: &str, attempt: u32) -> QueueRecord {
    let mut record = QueueRecord::new(format!("https://example.test/{name}"), dir.join(name), None);
    record.view.state = "failed".into();
    record.view.error = Some("source.transfer_failed: [7] Couldn't connect to server".into());
    record.view.action = Some(action.into());
    record.view.retryable = true;
    record.attempt = attempt;
    record.view.attempt = attempt;
    record
}

#[test]
fn only_plain_transport_failures_are_retried_automatically_and_on_a_backoff() {
    let dir = tempfile::tempdir().unwrap();
    let jobs = Session::in_memory(3);
    let settings = Settings::default(); // auto retry on, 3 attempts, 15 s base
    let mut state = QueueState {
        records: vec![
            failed_record(dir.path(), "transport.bin", "retry", 0),
            failed_record(dir.path(), "second.bin", "retry", 1),
            failed_record(dir.path(), "exhausted.bin", "retry", 3),
            failed_record(dir.path(), "checksum.bin", "check_checksum", 0),
            failed_record(dir.path(), "link.bin", "edit_link", 0),
            failed_record(dir.path(), "conflict.bin", "choose_new_path", 0),
            failed_record(dir.path(), "expired.bin", "refresh_source", 0),
            failed_record(dir.path(), "tools.bin", "configure_media_tools", 0),
            failed_record(dir.path(), "present.bin", "retry", 0),
        ],
        link_reviews: Vec::new(),
    };
    // A file already at the destination is never retried over.
    fs::write(dir.path().join("present.bin"), b"x").unwrap();

    let before = now_ms();
    jobs.schedule_automatic_retries(&mut state, &settings);
    let after = now_ms();

    let first = &state.records[0];
    assert_eq!(first.view.state, "scheduled");
    assert_eq!(first.attempt, 1);
    assert_eq!(first.view.attempt, 1);
    assert_eq!(first.view.action, None);
    assert!(!first.view.retryable);
    assert_eq!(
        first.view.error.as_deref(),
        Some("Retrying automatically (attempt 1 of 3).")
    );
    let due = first.retry_at_ms.expect("retry is due later");
    assert!(
        (before + 15_000..=after + 15_000).contains(&due),
        "first retry waits 15 s"
    );
    assert_eq!(first.not_before_ms, Some(due));
    assert!(first.job.is_some(), "a fresh job is prepared");

    let second = &state.records[1];
    assert_eq!(second.view.state, "scheduled");
    assert_eq!(second.attempt, 2);
    let due = second.retry_at_ms.unwrap();
    assert!(
        (before + 30_000..=after + 30_000).contains(&due),
        "second retry waits 30 s"
    );

    for record in &state.records[2..] {
        assert_eq!(record.view.state, "failed", "{}", record.display_url);
        assert_eq!(record.retry_at_ms, None, "{}", record.display_url);
    }
}

#[test]
fn a_transport_failure_restored_after_a_restart_is_retried_automatically() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("queue.json");
    let mut value = fixture_value(dir.path());
    // Only the two transport failures: no other row may start a transfer.
    let keep = [id(2), id(14)];
    value["records"]
        .as_array_mut()
        .unwrap()
        .retain(|record| keep.contains(&record["id"].as_str().unwrap().to_string()));
    write_queue(&path, &value);

    let jobs = Session::load(path, 1).unwrap();
    let rows = jobs.list().unwrap();
    let row = |n: u8| rows.iter().find(|row| row.job_id == id(n)).unwrap();
    assert_eq!(row(2).state, "scheduled");
    assert_eq!(row(2).attempt, 1);
    assert_eq!(
        row(14).state,
        "failed",
        "attempts already exhausted before the restart"
    );
    assert_eq!(row(14).attempt, 3);
}

#[test]
fn the_rate_is_smoothed_ignores_short_intervals_and_withdraws_when_stalled() {
    let mut rate = RateEstimate::default();
    rate.observe(0, 0);
    assert_eq!(
        rate.bytes_per_second(),
        None,
        "a first sample is only a baseline"
    );
    rate.observe(100_000, 200);
    assert_eq!(rate.bytes_per_second(), None, "under 400 ms is ignored");
    rate.observe(100_000, 1_000);
    assert_eq!(rate.bytes_per_second(), Some(100_000));
    rate.observe(300_000, 2_000);
    assert_eq!(
        rate.bytes_per_second(),
        Some(130_000),
        "0.3 × 200000 + 0.7 × 100000"
    );
    rate.observe(300_000, 3_000);
    assert_eq!(
        rate.bytes_per_second(),
        Some(91_000),
        "no progress decays, truncated"
    );
    rate.observe(300_000, 8_000);
    assert_eq!(
        rate.bytes_per_second(),
        None,
        "5 s without progress withdraws the rate"
    );
    rate.observe(50_000, 9_000);
    assert_eq!(
        rate.bytes_per_second(),
        None,
        "a lower offset restarts the estimate"
    );
    rate.observe(100_000, 10_000);
    assert_eq!(
        rate.bytes_per_second(),
        None,
        "a first estimate waits for 64 KiB or 5 s (FP-074)"
    );
    rate.observe(150_000, 10_500);
    assert_eq!(rate.bytes_per_second(), Some(66_666));
    rate.clear();
    assert_eq!(rate.bytes_per_second(), None);
}

#[test]
fn a_rate_below_one_byte_per_second_is_not_shown() {
    let mut rate = RateEstimate::default();
    rate.observe(0, 0);
    rate.observe(1, 5_000); // 0.2 B/s, measured once 5 s have passed
    assert_eq!(rate.bytes_per_second(), None);
}

#[test]
fn remaining_time_needs_a_total_and_a_rate_and_rounds_up() {
    let mut rate = RateEstimate::default();
    assert_eq!(rate.eta_seconds(0, Some(1_000_000)), None, "no rate yet");
    rate.observe(0, 0);
    rate.observe(100_000, 1_000);
    assert_eq!(rate.eta_seconds(100_000, None), None, "no total");
    assert_eq!(
        rate.eta_seconds(100_000, Some(100_000)),
        None,
        "nothing remaining"
    );
    assert_eq!(
        rate.eta_seconds(200_000, Some(100_000)),
        None,
        "received beyond total"
    );
    assert_eq!(
        rate.eta_seconds(100_000, Some(200_001)),
        Some(2),
        "100001 bytes at 100000 B/s"
    );
}

#[test]
fn shown_links_never_carry_queries_fragments_or_user_info() {
    let table = [
        ("https://example.test/a.zip", "https://example.test/a.zip"),
        (
            "https://example.test/a.zip?token=s",
            "https://example.test/a.zip?…",
        ),
        (
            "https://example.test/a.zip#part",
            "https://example.test/a.zip?…",
        ),
        (
            "https://user:pw@example.test/a.zip",
            "https://…@example.test/a.zip?…",
        ),
        (
            "https://user@example.test/a.zip?x=1",
            "https://…@example.test/a.zip?…",
        ),
        ("example.test/a.zip", "example.test/a.zip"),
    ];
    for (url, shown) in table {
        assert_eq!(display_url(url), shown, "{url}");
    }
    assert_eq!(
        restartable_url("https://example.test/a.zip").as_deref(),
        Some("https://example.test/a.zip")
    );
    assert_eq!(restartable_url("https://example.test/a.zip?token=s"), None);
    assert_eq!(restartable_url("https://example.test/a.zip#part"), None);
}

fn keys(value: &Value) -> Vec<String> {
    let mut keys: Vec<String> = value.as_object().unwrap().keys().cloned().collect();
    keys.sort();
    keys
}

#[test]
fn the_interface_reads_these_exact_field_names() {
    let dir = tempfile::tempdir().unwrap();
    let mut view = QueueRecord::new(
        "https://example.test/a.bin".into(),
        dir.path().join("a.bin"),
        None,
    )
    .view;
    assert_eq!(
        keys(&serde_json::to_value(&view).unwrap()),
        [
            "action",
            "attempt",
            "bytesReceived",
            "cleanupPending",
            "createdAtMs",
            "destination",
            "error",
            "finishedAtMs",
            "jobId",
            "kind",
            "notBeforeMs",
            "observedSha256",
            "qualityLabel",
            "retryable",
            "source",
            "state",
            "totalBytes",
        ],
        "optional fields are omitted while empty"
    );
    view.bytes_per_second = Some(1);
    view.eta_seconds = Some(1);
    view.expected_sha256 = Some("0".repeat(64));
    let with_optional = keys(&serde_json::to_value(&view).unwrap());
    for key in ["bytesPerSecond", "etaSeconds", "expectedSha256"] {
        assert!(with_optional.contains(&key.to_string()), "{key}");
    }

    assert_eq!(
        keys(&serde_json::to_value(QueueStats::default()).unwrap()),
        [
            "activeBytes",
            "combinedBytesPerSecond",
            "completed",
            "completedBytes",
            "failed",
            "maxActiveDownloads",
            "paused",
            "queued",
            "running",
            "scheduled",
        ]
    );
    let segment = SegmentView {
        start: 0,
        end: 9,
        received: 5,
    };
    assert_eq!(
        keys(&serde_json::to_value(&segment).unwrap()),
        ["end", "received", "start"]
    );
    let details = JobDetails {
        job: view.clone(),
        segments: vec![segment],
    };
    assert_eq!(
        keys(&serde_json::to_value(&details).unwrap()),
        ["job", "segments"]
    );
    let cancel = CancelResponse {
        outcome: "accepted",
        job: view,
    };
    assert_eq!(
        keys(&serde_json::to_value(&cancel).unwrap()),
        ["job", "outcome"]
    );
}
