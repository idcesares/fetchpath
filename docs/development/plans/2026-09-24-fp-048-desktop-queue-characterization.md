# FP-048 Desktop queue characterization Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Pin the desktop queue's current on-disk formats and decision logic with tests that pass on the unchanged code, so FP-049 can move the queue into `fetchpath-session` and prove nothing changed.

**Architecture:** One new test-only module, `apps/desktop/src-tauri/src/characterization.rs`, declared `#[cfg(test)] mod characterization;` in `lib.rs`. As a child of the crate root it can reach the private queue types (`QueueRecord`, `PersistedQueue`, `RateEstimate`, `action_for_error` …), and in FP-049 it moves into the session crate with the code it pins. Golden inputs are checked-in 0.1.0-shaped files under `apps/desktop/src-tauri/tests/fixtures/`. No production code changes.

**Tech Stack:** Rust 2024 edition workspace, `serde_json`, `tempfile` (already a dev-dependency), `cargo test -p fetchpath-desktop --locked`.

**Spec:** `docs/architecture/specs/2026-09-24-engine-platform-design.md` (§9 step 1, §10 first bullet); task contract: `node tools/tasks.mjs show FP-048`.

## Global Constraints

- No behavior change: `apps/desktop/src-tauri/src/lib.rs` gains only the one `mod` line; `settings.rs` is untouched.
- Tests are deterministic: no network, no dependency on media tools being installed, no reliance on wall-clock except bounded windows.
- Fixtures contain no secrets, real private URLs or personal paths; destinations use a `{DIR}` placeholder replaced by a temporary directory.
- Characterization pins what the code does today, including behavior we think is wrong. Such tests carry `finding_` in their name and are listed in the task record; fixing them is separate work that updates the test deliberately.
- Every test name states the behavior in plain words, matching the existing style (`a_checksum_mismatch_saves_nothing_and_is_never_retried_automatically`).

## Review Focus

- A queue file carrying fields this build does not know (written by a newer build with the same schema) → it loads and keeps the known fields. Pinned in Task 3.
- A queue file missing fields that later versions added with `#[serde(default)]` (`credentialRef`, `mediaVariantId`, `mediaQuality`, `totalBytes`, `attempt`, `kind`, `qualityLabel`) → it loads with defaults and `kind` = `file`. Pinned in Task 3.
- A destination with non-ASCII characters → survives the round trip unchanged. Pinned in Task 1 via fixture record 01.
- A restored queued job whose scheduled time is already past → `queued`, not `scheduled`. Pinned in Task 2 via record 08.
- A queue written by a newer schema version → today it is discarded and then deleted by the second save (finding F1). Pinned in Task 3 so FP-049 cannot change it silently.

---

### Task 1: Fixtures, module and golden file shapes

**Files:**
- Create: `apps/desktop/src-tauri/tests/fixtures/queue-0.1.0.json`
- Create: `apps/desktop/src-tauri/tests/fixtures/settings-0.1.0.json`
- Create: `apps/desktop/src-tauri/src/characterization.rs`
- Modify: `apps/desktop/src-tauri/src/lib.rs` (add the `mod` line after `mod tests` at the end of the file)
- Modify: `docs/tasks/backlog.json` (FP-048 `files` gains the module and fixtures)

**Interfaces:**
- Produces (used by Tasks 2–5, all inside `characterization.rs`):
  - `const QUEUE_FIXTURE: &str` and `const SETTINGS_FIXTURE: &str` (`include_str!`)
  - `fn queue_fixture(dir: &Path) -> String` — fixture text with `{DIR}` replaced by `dir` (JSON-escaped)
  - `fn fixture_queue(dir: &Path) -> PersistedQueue`
  - `fn fixture_record(dir: &Path, id: &str) -> PersistedRecord`
  - `const NOW: u64 = 1_800_000_000_000;` `const FAR: u64 = 4_102_444_800_000;`

- [ ] **Step 1: Create the branch and record ownership**

```bash
git switch -c fp-048-characterize-desktop-queue
```

In `docs/tasks/backlog.json`, FP-048 is already `in_progress`/`lead`. Set its `files` to:

```json
"files": [
  "apps/desktop/src-tauri/src/characterization.rs",
  "apps/desktop/src-tauri/src/lib.rs",
  "apps/desktop/src-tauri/tests/fixtures",
  "docs/development/ENGINE-SESSION.md",
  "docs/development/README.md"
]
```

- [ ] **Step 2: Write the queue fixture**

`apps/desktop/src-tauri/tests/fixtures/queue-0.1.0.json` — every field the 0.1.0 writer emits (fields with `skip_serializing_if` appear only when set):

```json
{
  "schemaVersion": 1,
  "records": [
    {
      "id": "00000000-0000-4000-8000-000000000001",
      "restartUrl": "https://example.test/files/r%C3%A9sum%C3%A9.pdf",
      "credentialRef": null,
      "displayUrl": "https://example.test/files/r%C3%A9sum%C3%A9.pdf",
      "destination": "{DIR}\\résumé ✓.pdf",
      "notBeforeMs": null,
      "createdAtMs": 1790000000000,
      "finishedAtMs": 1790000060000,
      "mediaVariantId": null,
      "mediaQuality": null,
      "view": {
        "jobId": "00000000-0000-4000-8000-000000000001",
        "source": "https://example.test/files/r%C3%A9sum%C3%A9.pdf",
        "state": "completed",
        "bytesReceived": 1048576,
        "totalBytes": 1048576,
        "attempt": 0,
        "destination": "{DIR}\\résumé ✓.pdf",
        "observedSha256": "f616fddc4f999bd2b1f22fd8447eee9ecdbba01e70f43278c6021cea91af3cf4",
        "cleanupPending": false,
        "error": null,
        "action": null,
        "retryable": false,
        "createdAtMs": 1790000000000,
        "notBeforeMs": null,
        "finishedAtMs": 1790000060000,
        "kind": "file",
        "qualityLabel": null
      }
    },
    {
      "id": "00000000-0000-4000-8000-000000000002",
      "restartUrl": "https://example.test/files/transport.bin",
      "credentialRef": null,
      "displayUrl": "https://example.test/files/transport.bin",
      "destination": "{DIR}\\transport.bin",
      "notBeforeMs": null,
      "createdAtMs": 1790000000000,
      "finishedAtMs": 1790000010000,
      "mediaVariantId": null,
      "mediaQuality": null,
      "view": {
        "jobId": "00000000-0000-4000-8000-000000000002",
        "source": "https://example.test/files/transport.bin",
        "state": "failed",
        "bytesReceived": 0,
        "totalBytes": null,
        "attempt": 0,
        "destination": null,
        "observedSha256": null,
        "cleanupPending": false,
        "error": "source.transfer_failed: [7] Couldn't connect to server",
        "action": "retry",
        "retryable": true,
        "createdAtMs": 1790000000000,
        "notBeforeMs": null,
        "finishedAtMs": 1790000010000,
        "kind": "file",
        "qualityLabel": null
      }
    },
    {
      "id": "00000000-0000-4000-8000-000000000003",
      "restartUrl": "https://example.test/files/conflict.bin",
      "credentialRef": null,
      "displayUrl": "https://example.test/files/conflict.bin",
      "destination": "{DIR}\\conflict.bin",
      "notBeforeMs": null,
      "createdAtMs": 1790000000000,
      "finishedAtMs": 1790000010000,
      "mediaVariantId": null,
      "mediaQuality": null,
      "view": {
        "jobId": "00000000-0000-4000-8000-000000000003",
        "source": "https://example.test/files/conflict.bin",
        "state": "failed",
        "bytesReceived": 0,
        "totalBytes": null,
        "attempt": 0,
        "destination": null,
        "observedSha256": null,
        "cleanupPending": false,
        "error": "storage.destination_conflict: a file already exists at the destination",
        "action": "choose_new_path",
        "retryable": true,
        "createdAtMs": 1790000000000,
        "notBeforeMs": null,
        "finishedAtMs": 1790000010000,
        "kind": "file",
        "qualityLabel": null
      }
    },
    {
      "id": "00000000-0000-4000-8000-000000000004",
      "restartUrl": null,
      "credentialRef": null,
      "displayUrl": "https://example.test/files/private.bin?…",
      "destination": "{DIR}\\private.bin",
      "notBeforeMs": null,
      "createdAtMs": 1790000000000,
      "finishedAtMs": 1790000010000,
      "mediaVariantId": null,
      "mediaQuality": null,
      "view": {
        "jobId": "00000000-0000-4000-8000-000000000004",
        "source": "https://example.test/files/private.bin?…",
        "state": "cancelled",
        "bytesReceived": 4096,
        "totalBytes": null,
        "attempt": 0,
        "destination": null,
        "observedSha256": null,
        "cleanupPending": false,
        "error": null,
        "action": null,
        "retryable": false,
        "createdAtMs": 1790000000000,
        "notBeforeMs": null,
        "finishedAtMs": 1790000010000,
        "kind": "file",
        "qualityLabel": null
      }
    },
    {
      "id": "00000000-0000-4000-8000-000000000005",
      "restartUrl": "https://example.test/files/paused.bin",
      "credentialRef": null,
      "displayUrl": "https://example.test/files/paused.bin",
      "destination": "{DIR}\\paused.bin",
      "notBeforeMs": null,
      "createdAtMs": 1790000000000,
      "finishedAtMs": null,
      "mediaVariantId": null,
      "mediaQuality": null,
      "view": {
        "jobId": "00000000-0000-4000-8000-000000000005",
        "source": "https://example.test/files/paused.bin",
        "state": "paused",
        "bytesReceived": 524288,
        "totalBytes": 1048576,
        "attempt": 0,
        "destination": null,
        "observedSha256": null,
        "cleanupPending": false,
        "error": null,
        "action": null,
        "retryable": false,
        "createdAtMs": 1790000000000,
        "notBeforeMs": null,
        "finishedAtMs": null,
        "kind": "file",
        "qualityLabel": null
      }
    },
    {
      "id": "00000000-0000-4000-8000-000000000006",
      "restartUrl": null,
      "credentialRef": null,
      "displayUrl": "https://example.test/files/paused-private.bin?…",
      "destination": "{DIR}\\paused-private.bin",
      "notBeforeMs": null,
      "createdAtMs": 1790000000000,
      "finishedAtMs": null,
      "mediaVariantId": null,
      "mediaQuality": null,
      "view": {
        "jobId": "00000000-0000-4000-8000-000000000006",
        "source": "https://example.test/files/paused-private.bin?…",
        "state": "paused",
        "bytesReceived": 1024,
        "totalBytes": null,
        "attempt": 0,
        "destination": null,
        "observedSha256": null,
        "cleanupPending": false,
        "error": null,
        "action": null,
        "retryable": false,
        "createdAtMs": 1790000000000,
        "notBeforeMs": null,
        "finishedAtMs": null,
        "kind": "file",
        "qualityLabel": null
      }
    },
    {
      "id": "00000000-0000-4000-8000-000000000007",
      "restartUrl": "https://example.test/files/scheduled.bin",
      "credentialRef": null,
      "displayUrl": "https://example.test/files/scheduled.bin",
      "destination": "{DIR}\\scheduled.bin",
      "notBeforeMs": 4102444800000,
      "createdAtMs": 1790000000000,
      "finishedAtMs": null,
      "mediaVariantId": null,
      "mediaQuality": null,
      "view": {
        "jobId": "00000000-0000-4000-8000-000000000007",
        "source": "https://example.test/files/scheduled.bin",
        "state": "scheduled",
        "bytesReceived": 0,
        "totalBytes": null,
        "attempt": 0,
        "destination": null,
        "observedSha256": null,
        "cleanupPending": false,
        "error": null,
        "action": null,
        "retryable": false,
        "createdAtMs": 1790000000000,
        "notBeforeMs": 4102444800000,
        "finishedAtMs": null,
        "kind": "file",
        "qualityLabel": null
      }
    },
    {
      "id": "00000000-0000-4000-8000-000000000008",
      "restartUrl": "https://example.test/files/running.bin",
      "credentialRef": null,
      "displayUrl": "https://example.test/files/running.bin",
      "destination": "{DIR}\\running.bin",
      "notBeforeMs": 1790000000000,
      "createdAtMs": 1790000000000,
      "finishedAtMs": null,
      "mediaVariantId": null,
      "mediaQuality": null,
      "view": {
        "jobId": "00000000-0000-4000-8000-000000000008",
        "source": "https://example.test/files/running.bin",
        "state": "running",
        "bytesReceived": 65536,
        "totalBytes": 1048576,
        "attempt": 0,
        "destination": null,
        "observedSha256": null,
        "cleanupPending": false,
        "error": null,
        "action": null,
        "retryable": false,
        "createdAtMs": 1790000000000,
        "notBeforeMs": 1790000000000,
        "finishedAtMs": null,
        "kind": "file",
        "qualityLabel": null
      }
    },
    {
      "id": "00000000-0000-4000-8000-000000000009",
      "restartUrl": null,
      "credentialRef": "browser-capture-0009",
      "displayUrl": "https://example.test/account/export.zip",
      "destination": "{DIR}\\export.zip",
      "notBeforeMs": null,
      "createdAtMs": 1790000000000,
      "finishedAtMs": null,
      "mediaVariantId": null,
      "mediaQuality": null,
      "view": {
        "jobId": "00000000-0000-4000-8000-000000000009",
        "source": "https://example.test/account/export.zip",
        "state": "running",
        "bytesReceived": 1024,
        "totalBytes": null,
        "attempt": 0,
        "destination": null,
        "observedSha256": null,
        "cleanupPending": false,
        "error": null,
        "action": null,
        "retryable": false,
        "createdAtMs": 1790000000000,
        "notBeforeMs": null,
        "finishedAtMs": null,
        "kind": "file",
        "qualityLabel": null
      }
    },
    {
      "id": "00000000-0000-4000-8000-000000000010",
      "restartUrl": "https://example.test/files/bad-checksum.bin",
      "credentialRef": null,
      "displayUrl": "https://example.test/files/bad-checksum.bin",
      "destination": "{DIR}\\bad-checksum.bin",
      "notBeforeMs": 4102444800000,
      "createdAtMs": 1790000000000,
      "finishedAtMs": null,
      "mediaVariantId": null,
      "mediaQuality": null,
      "view": {
        "jobId": "00000000-0000-4000-8000-000000000010",
        "source": "https://example.test/files/bad-checksum.bin",
        "state": "scheduled",
        "bytesReceived": 0,
        "totalBytes": null,
        "attempt": 0,
        "destination": null,
        "observedSha256": null,
        "expectedSha256": "not-a-sha256",
        "cleanupPending": false,
        "error": null,
        "action": null,
        "retryable": false,
        "createdAtMs": 1790000000000,
        "notBeforeMs": 4102444800000,
        "finishedAtMs": null,
        "kind": "file",
        "qualityLabel": null
      }
    },
    {
      "id": "00000000-0000-4000-8000-000000000011",
      "restartUrl": "https://example.test/files/good-checksum.bin",
      "credentialRef": null,
      "displayUrl": "https://example.test/files/good-checksum.bin",
      "destination": "{DIR}\\good-checksum.bin",
      "notBeforeMs": 4102444800000,
      "createdAtMs": 1790000000000,
      "finishedAtMs": null,
      "mediaVariantId": null,
      "mediaQuality": null,
      "view": {
        "jobId": "00000000-0000-4000-8000-000000000011",
        "source": "https://example.test/files/good-checksum.bin",
        "state": "scheduled",
        "bytesReceived": 0,
        "totalBytes": null,
        "attempt": 0,
        "destination": null,
        "observedSha256": null,
        "expectedSha256": "f616fddc4f999bd2b1f22fd8447eee9ecdbba01e70f43278c6021cea91af3cf4",
        "cleanupPending": false,
        "error": null,
        "action": null,
        "retryable": false,
        "createdAtMs": 1790000000000,
        "notBeforeMs": 4102444800000,
        "finishedAtMs": null,
        "kind": "file",
        "qualityLabel": null
      }
    },
    {
      "id": "00000000-0000-4000-8000-000000000012",
      "restartUrl": "https://video.example.test/watch/abc",
      "credentialRef": null,
      "displayUrl": "https://video.example.test/watch/abc",
      "destination": "{DIR}\\talk.mp4",
      "notBeforeMs": null,
      "createdAtMs": 1790000000000,
      "finishedAtMs": 1790000300000,
      "mediaVariantId": "137+140",
      "mediaQuality": "1080p",
      "view": {
        "jobId": "00000000-0000-4000-8000-000000000012",
        "source": "https://video.example.test/watch/abc",
        "state": "completed",
        "bytesReceived": 52428800,
        "totalBytes": null,
        "attempt": 0,
        "destination": "{DIR}\\talk.mp4",
        "observedSha256": "0000000000000000000000000000000000000000000000000000000000000012",
        "cleanupPending": false,
        "error": null,
        "action": null,
        "retryable": false,
        "createdAtMs": 1790000000000,
        "notBeforeMs": null,
        "finishedAtMs": 1790000300000,
        "kind": "media",
        "qualityLabel": "1080p"
      }
    },
    {
      "id": "00000000-0000-4000-8000-000000000013",
      "restartUrl": "https://video.example.test/watch/def",
      "credentialRef": null,
      "displayUrl": "https://video.example.test/watch/def",
      "destination": "{DIR}\\later.mp4",
      "notBeforeMs": 4102444800000,
      "createdAtMs": 1790000000000,
      "finishedAtMs": null,
      "mediaVariantId": "22",
      "mediaQuality": "720p",
      "view": {
        "jobId": "00000000-0000-4000-8000-000000000013",
        "source": "https://video.example.test/watch/def",
        "state": "scheduled",
        "bytesReceived": 0,
        "totalBytes": null,
        "attempt": 0,
        "destination": null,
        "observedSha256": null,
        "cleanupPending": false,
        "error": null,
        "action": null,
        "retryable": false,
        "createdAtMs": 1790000000000,
        "notBeforeMs": 4102444800000,
        "finishedAtMs": null,
        "kind": "media",
        "qualityLabel": "720p"
      }
    },
    {
      "id": "00000000-0000-4000-8000-000000000014",
      "restartUrl": "https://example.test/files/exhausted.bin",
      "credentialRef": null,
      "displayUrl": "https://example.test/files/exhausted.bin",
      "destination": "{DIR}\\exhausted.bin",
      "notBeforeMs": null,
      "createdAtMs": 1790000000000,
      "finishedAtMs": 1790000090000,
      "mediaVariantId": null,
      "mediaQuality": null,
      "view": {
        "jobId": "00000000-0000-4000-8000-000000000014",
        "source": "https://example.test/files/exhausted.bin",
        "state": "failed",
        "bytesReceived": 0,
        "totalBytes": null,
        "attempt": 3,
        "destination": null,
        "observedSha256": null,
        "cleanupPending": false,
        "error": "source.transfer_failed: [7] Couldn't connect to server",
        "action": "retry",
        "retryable": true,
        "createdAtMs": 1790000000000,
        "notBeforeMs": null,
        "finishedAtMs": 1790000090000,
        "kind": "file",
        "qualityLabel": null
      }
    }
  ]
}
```

- [ ] **Step 3: Write the settings fixture**

`apps/desktop/src-tauri/tests/fixtures/settings-0.1.0.json` — every field set away from its default:

```json
{
  "schemaVersion": 1,
  "maxActiveDownloads": 5,
  "defaultDestinationDir": "C:\\Downloads\\Fetchpath",
  "autoRetry": false,
  "autoRetryMaxAttempts": 5,
  "autoRetryBaseDelaySeconds": 30,
  "closeToTray": false,
  "powerMode": true,
  "mediaToolsDir": null,
  "confirmRemoveCompleted": false,
  "theme": "dark",
  "onboardingCompleted": true
}
```

- [ ] **Step 4: Write the module with the golden-shape tests**

`apps/desktop/src-tauri/src/characterization.rs`:

```rust
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
```

Append to the very end of `apps/desktop/src-tauri/src/lib.rs`:

```rust

#[cfg(test)]
mod characterization;
```

- [ ] **Step 5: Run the three tests**

Run: `cargo test -p fetchpath-desktop --locked characterization`
Expected: 3 passed. If `the_0_1_0_queue_file_is_exactly_what_the_queue_writes` fails, the fixture — not the code — is wrong: the assertion diff names the field; correct the fixture to match the writer and rerun. Do not change production code.

- [ ] **Step 6: Commit**

```bash
git add apps/desktop/src-tauri/src/characterization.rs apps/desktop/src-tauri/src/lib.rs apps/desktop/src-tauri/tests/fixtures docs/tasks/backlog.json
git commit -m "test(desktop): pin the 0.1.0 queue and settings file shapes (FP-048)"
```

---

### Task 2: What each saved state becomes after a restart

**Files:**
- Modify: `apps/desktop/src-tauri/src/characterization.rs` (append)

**Interfaces:**
- Consumes: `fixture_queue`, `id`, `NOW` from Task 1; `QueueRecord::restore(saved, now, browser_store, media_tools)`; `BridgeStore::new(PathBuf)`; `UNREADABLE_CHECKSUM`.

- [ ] **Step 1: Write the restore table test**

Append:

```rust
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
```

- [ ] **Step 2: Run them**

Run: `cargo test -p fetchpath-desktop --locked characterization`
Expected: 5 passed. A failing row prints `record N: <field>`; compare it with `QueueRecord::restore` in `lib.rs` (the branch order is: paused with a live link → private or missing link on failed/cancelled → terminal → live link → otherwise needs source). Correct the table to what the code does, and if that behavior looks wrong, rename the assertion's test to `finding_…` and note it for the record in Task 6. Never edit `lib.rs`.

- [ ] **Step 3: Commit**

```bash
git add apps/desktop/src-tauri/src/characterization.rs
git commit -m "test(desktop): pin what every saved state becomes after a restart (FP-048)"
```

---

### Task 3: Queue file robustness, including finding F1

**Files:**
- Modify: `apps/desktop/src-tauri/src/characterization.rs` (append)

**Interfaces:**
- Consumes: `queue_fixture`, `id` (Task 1); `save_persisted(&Path, &QueueState)`, `load_persisted(&Path) -> io::Result<Option<PersistedQueue>>`, `DesktopJobs::load(PathBuf, usize)`, `DesktopJobs::list()`.

- [ ] **Step 1: Write the tests**

Append:

```rust
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
```

- [ ] **Step 2: Run them**

Run: `cargo test -p fetchpath-desktop --locked characterization`
Expected: 11 passed. `finding_f1` must pass: it proves the loss exists today. If it fails because the backup text is formatted differently, match the assertion to the real content (`serde_json::to_writer_pretty` writes `"schemaVersion": 2`), not the other way round.

- [ ] **Step 3: Commit**

```bash
git add apps/desktop/src-tauri/src/characterization.rs
git commit -m "test(desktop): pin queue file fallback, compatibility and finding F1 (FP-048)"
```

---

### Task 4: Failure classification and automatic retry

**Files:**
- Modify: `apps/desktop/src-tauri/src/characterization.rs` (append)

**Interfaces:**
- Consumes: `action_for_error(&str) -> &'static str`, `recovery_action(&JobSnapshot) -> (Option<String>, bool)`, `DesktopJobs::in_memory(usize)`, `DesktopJobs::schedule_automatic_retries(&self, &mut QueueState, &Settings)`, `QueueRecord::new(String, PathBuf, Option<u64>)`, `DesktopJobs::load`, `now_ms()`; fixture helpers from Tasks 1 and 3.

- [ ] **Step 1: Write the tests**

Append:

```rust
#[test]
fn each_failure_maps_to_the_step_a_person_or_the_queue_takes_next() {
    let table = [
        ("integrity.checksum_mismatch: expected aa, received bb", "check_checksum"),
        (UNREADABLE_CHECKSUM, "check_checksum"),
        ("media.source_expired: the page link expired", "refresh_source"),
        ("media.unknown_variant: 137+140", "refresh_source"),
        ("media.helper_unavailable: yt-dlp not found", "configure_media_tools"),
        ("storage.destination_conflict: exists", "choose_new_path"),
        ("A file already exists at the destination.", "choose_new_path"),
        ("input.invalid_url: not http", "edit_link"),
        ("input.invalid_destination: reserved name", "edit_link"),
        ("source.transfer_failed: HTTP status 404", "edit_link"),
        ("source.transfer_failed: HTTP status 503", "retry"),
        ("source.transfer_failed: [28] Timeout was reached", "retry"),
        // A checksum problem outranks the HTTP status in the same message.
        ("integrity.checksum_mismatch after HTTP status 404", "check_checksum"),
    ];
    for (error, action) in table {
        assert_eq!(action_for_error(error), action, "{error}");
    }
}

/// Finding F2: anything unrecognized, including an internal invariant
/// failure, maps to "retry" and is therefore retried automatically. The job
/// contract (§9) says an unrecognized error must not be assumed retryable.
#[test]
fn finding_f2_unrecognized_and_internal_errors_are_treated_as_retryable() {
    assert_eq!(action_for_error(""), "retry");
    assert_eq!(action_for_error("internal.unknown: something new"), "retry");
    assert_eq!(action_for_error("internal.metadata_failure: journal"), "retry");
}

#[test]
fn only_failed_rows_get_a_recovery_action() {
    let dir = tempfile::tempdir().unwrap();
    let mut view = QueueRecord::new("https://example.test/a.bin".into(), dir.path().join("a.bin"), None).view;
    for state in ["queued", "scheduled", "running", "paused", "cancelling", "completed", "cancelled", "needs_source"] {
        view.state = state.into();
        view.error = Some("source.transfer_failed: HTTP status 404".into());
        assert_eq!(recovery_action(&view), (None, false), "{state}");
    }
    view.state = "failed".into();
    view.error = None;
    assert_eq!(recovery_action(&view), (Some("retry".into()), true));
}

fn failed_record(dir: &Path, name: &str, action: &str, attempt: u32) -> QueueRecord {
    let mut record = QueueRecord::new(
        format!("https://example.test/{name}"),
        dir.join(name),
        None,
    );
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
    let jobs = DesktopJobs::in_memory(3);
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
    assert_eq!(first.view.error.as_deref(), Some("Retrying automatically (attempt 1 of 3)."));
    let due = first.retry_at_ms.expect("retry is due later");
    assert!((before + 15_000..=after + 15_000).contains(&due), "first retry waits 15 s");
    assert_eq!(first.not_before_ms, Some(due));
    assert!(first.job.is_some(), "a fresh job is prepared");

    let second = &state.records[1];
    assert_eq!(second.view.state, "scheduled");
    assert_eq!(second.attempt, 2);
    let due = second.retry_at_ms.unwrap();
    assert!((before + 30_000..=after + 30_000).contains(&due), "second retry waits 30 s");

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

    let jobs = DesktopJobs::load(path, 1).unwrap();
    let rows = jobs.list().unwrap();
    let row = |n: u8| rows.iter().find(|row| row.job_id == id(n)).unwrap();
    assert_eq!(row(2).state, "scheduled");
    assert_eq!(row(2).attempt, 1);
    assert_eq!(row(14).state, "failed", "attempts already exhausted before the restart");
    assert_eq!(row(14).attempt, 3);
}
```

- [ ] **Step 2: Run them**

Run: `cargo test -p fetchpath-desktop --locked characterization`
Expected: 16 passed. If `only_failed_rows_get_a_recovery_action` fails on construction, check that `QueueRecord::new` is reachable (it is a private associated function in the crate root, visible to this child module).

- [ ] **Step 3: Commit**

```bash
git add apps/desktop/src-tauri/src/characterization.rs
git commit -m "test(desktop): pin failure classification and automatic retry, finding F2 (FP-048)"
```

---

### Task 5: Rates, remaining time, redaction and the interface's JSON shapes

**Files:**
- Modify: `apps/desktop/src-tauri/src/characterization.rs` (append)

**Interfaces:**
- Consumes: `RateEstimate::{observe, clear, bytes_per_second, eta_seconds}`, `display_url`, `restartable_url`, `JobSnapshot`, `QueueStats`, `SegmentView`, `JobDetails`, `CancelResponse`.

- [ ] **Step 1: Write the tests**

Append:

```rust
#[test]
fn the_rate_is_smoothed_ignores_short_intervals_and_withdraws_when_stalled() {
    let mut rate = RateEstimate::default();
    rate.observe(0, 0);
    assert_eq!(rate.bytes_per_second(), None, "a first sample is only a baseline");
    rate.observe(1_000, 200);
    assert_eq!(rate.bytes_per_second(), None, "under 400 ms is ignored");
    rate.observe(1_000, 1_000);
    assert_eq!(rate.bytes_per_second(), Some(1_000));
    rate.observe(3_000, 2_000);
    assert_eq!(rate.bytes_per_second(), Some(1_300), "0.3 × 2000 + 0.7 × 1000");
    rate.observe(3_000, 3_000);
    assert_eq!(rate.bytes_per_second(), Some(909), "no progress decays, truncated");
    rate.observe(3_000, 8_000);
    assert_eq!(rate.bytes_per_second(), None, "5 s without progress withdraws the rate");
    rate.observe(500, 9_000);
    assert_eq!(rate.bytes_per_second(), None, "a lower offset restarts the estimate");
    rate.observe(1_000, 10_000);
    assert_eq!(rate.bytes_per_second(), Some(500));
    rate.clear();
    assert_eq!(rate.bytes_per_second(), None);
}

#[test]
fn a_rate_below_one_byte_per_second_is_not_shown() {
    let mut rate = RateEstimate::default();
    rate.observe(0, 0);
    rate.observe(1, 4_000); // 0.25 B/s
    assert_eq!(rate.bytes_per_second(), None);
}

#[test]
fn remaining_time_needs_a_total_and_a_rate_and_rounds_up() {
    let mut rate = RateEstimate::default();
    assert_eq!(rate.eta_seconds(0, Some(10_000)), None, "no rate yet");
    rate.observe(0, 0);
    rate.observe(1_000, 1_000);
    assert_eq!(rate.eta_seconds(1_000, None), None, "no total");
    assert_eq!(rate.eta_seconds(1_000, Some(1_000)), None, "nothing remaining");
    assert_eq!(rate.eta_seconds(2_000, Some(1_000)), None, "received beyond total");
    assert_eq!(rate.eta_seconds(1_000, Some(2_001)), Some(2), "1001 bytes at 1000 B/s");
}

#[test]
fn shown_links_never_carry_queries_fragments_or_user_info() {
    let table = [
        ("https://example.test/a.zip", "https://example.test/a.zip"),
        ("https://example.test/a.zip?token=s", "https://example.test/a.zip?…"),
        ("https://example.test/a.zip#part", "https://example.test/a.zip?…"),
        ("https://user:pw@example.test/a.zip", "https://…@example.test/a.zip?…"),
        ("https://user@example.test/a.zip?x=1", "https://…@example.test/a.zip?…"),
        ("example.test/a.zip", "example.test/a.zip"),
    ];
    for (url, shown) in table {
        assert_eq!(display_url(url), shown, "{url}");
    }
    assert_eq!(restartable_url("https://example.test/a.zip").as_deref(), Some("https://example.test/a.zip"));
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
    let mut view = QueueRecord::new("https://example.test/a.bin".into(), dir.path().join("a.bin"), None).view;
    assert_eq!(
        keys(&serde_json::to_value(&view).unwrap()),
        [
            "action", "attempt", "bytesReceived", "cleanupPending", "createdAtMs", "destination",
            "error", "finishedAtMs", "jobId", "kind", "notBeforeMs", "observedSha256",
            "qualityLabel", "retryable", "source", "state", "totalBytes",
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
            "activeBytes", "combinedBytesPerSecond", "completed", "completedBytes", "failed",
            "maxActiveDownloads", "paused", "queued", "running", "scheduled",
        ]
    );
    let segment = SegmentView { start: 0, end: 9, received: 5 };
    assert_eq!(keys(&serde_json::to_value(&segment).unwrap()), ["end", "received", "start"]);
    let details = JobDetails { job: view.clone(), segments: vec![segment] };
    assert_eq!(keys(&serde_json::to_value(&details).unwrap()), ["job", "segments"]);
    let cancel = CancelResponse { outcome: "accepted", job: view };
    assert_eq!(keys(&serde_json::to_value(&cancel).unwrap()), ["job", "outcome"]);
}
```

- [ ] **Step 2: Run them**

Run: `cargo test -p fetchpath-desktop --locked characterization`
Expected: 21 passed. The rate values are exact f64 results (1300 and 909.99… truncated to 909); if a value differs, the smoothing constants changed — stop and report rather than edit.

- [ ] **Step 3: Commit**

```bash
git add apps/desktop/src-tauri/src/characterization.rs
git commit -m "test(desktop): pin rates, redaction and interface field names (FP-048)"
```

---

### Task 6: Prove the tests bite, record them, close the task

**Files:**
- Create: `docs/development/ENGINE-SESSION.md`
- Modify: `docs/development/README.md`
- Modify: `docs/tasks/backlog.json`

- [ ] **Step 1: Mutation spot-checks (revert each immediately)**

For each change below: edit `lib.rs`, run `cargo test -p fetchpath-desktop --locked characterization`, confirm the named test fails, then `git checkout -- apps/desktop/src-tauri/src/lib.rs`.

| Temporary change in `lib.rs` | Test that must fail |
|---|---|
| In `action_for_error`, return `"retry"` for `checksum_mismatch` | `each_failure_maps_to_the_step_a_person_or_the_queue_takes_next` |
| In `QueueRecord::restore`, change `let paused = saved.view.state == "paused";` to `let paused = false;` | `every_saved_state_comes_back_after_a_restart_as_it_did_in_0_1_0` |
| In `load_persisted`, remove the `backup.as_path()` candidate | `a_corrupt_queue_file_falls_back_to_its_backup` |
| In `RateEstimate`, change `MIN_INTERVAL_MS` to `100` | `the_rate_is_smoothed_ignores_short_intervals_and_withdraws_when_stalled` |
| Rename `cleanup_pending` to `cleanup` in `JobSnapshot` with `#[serde(rename = "cleanup")]` | `the_interface_reads_these_exact_field_names` and `the_0_1_0_queue_file_is_exactly_what_the_queue_writes` |

Record which test failed for each in the task record.

- [ ] **Step 2: Run the whole desktop suite and lints**

Run: `cargo test -p fetchpath-desktop --locked`
Expected: all existing tests plus 21 characterization tests pass.
Run: `cargo clippy -p fetchpath-desktop --all-targets --locked -- -D warnings` and `cargo fmt --check`
Expected: clean. Fix formatting with `cargo fmt -p fetchpath-desktop` if needed (test file only).

- [ ] **Step 3: Write the task record**

`docs/development/ENGINE-SESSION.md` with: purpose (FP-048, then FP-049); the pinned areas (file shapes, restore table, file robustness, classification and retry, rates, redaction, field names); the date, machine (`Windows 11 Pro 26200 x64`), commands actually run and their counts; the mutation table from Step 1 with the observed failing test for each; and the findings:

- **F1** — a queue written by a newer schema is discarded and deleted by the second save; matters once an engine and clients of different builds coexist. Proposed fix: keep an unreadable-newer queue untouched and refuse to write over it.
- **F2** — unrecognized and `internal.*` errors map to `retry` and are auto-retried, contrary to job contract §9. Proposed fix belongs with protocol error codes (FP-050/FP-051).
- **F3** — classification reads error message text; the protocol should carry the engine's code and action instead (job contract §9: "UI buttons come from `action`, not string parsing").

State that none are fixed here, by design.

Add under "Next phase: engine platform" in `docs/development/README.md`:

```markdown
- [Engine session](ENGINE-SESSION.md) — FP-048 characterization of the desktop queue, then FP-049.
```

- [ ] **Step 4: Close FP-048 and queue the findings**

In `docs/tasks/backlog.json`: FP-048 `status` `done`, `evidence` `["apps/desktop/src-tauri/src/characterization.rs", "apps/desktop/src-tauri/tests/fixtures/queue-0.1.0.json", "apps/desktop/src-tauri/tests/fixtures/settings-0.1.0.json", "docs/development/ENGINE-SESSION.md"]`, `verification` replaced with the actual counts and date. Add FP-070:

```json
{
  "id": "FP-070",
  "title": "Keep a queue written by a newer Fetchpath instead of discarding it",
  "milestone": "M10",
  "dependsOn": ["FP-049"],
  "status": "todo",
  "owner": null,
  "priority": "P1",
  "modelTier": "astra-high",
  "review": "strong",
  "files": ["crates/fetchpath-session"],
  "acceptanceIds": ["A04", "A12"],
  "acceptance": "Loading a queue whose schema is newer than this build leaves the file and its backup untouched, starts read-only with a plain explanation, and never writes over it; finding F1's characterization test is updated deliberately to the new behavior.",
  "verification": "A test writes a newer-schema queue, opens and saves repeatedly, and finds the original bytes intact; strong-model review.",
  "evidence": [],
  "blockedReason": null
}
```

Add to FP-051's `acceptance` the sentence: `Unrecognized and internal errors are not retryable and are never retried automatically (finding F2), and clients act on the engine's error code and action rather than message text (finding F3).`

Run: `node tools/tasks.mjs check` and `node --test tests/repo/structure.test.mjs tests/repo/tasks.test.mjs`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add docs/development/ENGINE-SESSION.md docs/development/README.md docs/tasks/backlog.json
git commit -m "docs: record the desktop queue characterization and its findings (FP-048)"
```
