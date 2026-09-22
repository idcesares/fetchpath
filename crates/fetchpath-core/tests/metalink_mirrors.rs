//! Real loopback mirror fixtures for FP-019.
//!
//! This test is ignored by default and is driven by
//! `tests/compatibility/metalink/run.ps1`, which starts the bad, slow, offline,
//! and healthy mirrors and points the environment at the generated Metalink
//! documents. It records what actually happened as JSON evidence.

use std::fmt::Write as _;
use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use fetchpath_core::{
    MirrorOutcome, MirrorReport, VerificationLevel, VerifiedDownload, VerifiedDownloadError,
    VerifiedDownloadRequest, download_verified, parse_metalink,
};

fn env(name: &str) -> String {
    std::env::var(name)
        .unwrap_or_else(|_| panic!("{name} is required; run tests/compatibility/metalink/run.ps1"))
}

fn metalink_request(variable: &str, destination: PathBuf) -> VerifiedDownloadRequest {
    let document = fs::read(env(variable)).expect("read the metalink document");
    let metalink = parse_metalink(&document).expect("parse the metalink document");
    let file = metalink.files.first().expect("one file entry");
    VerifiedDownloadRequest {
        // A slow mirror must be abandoned quickly enough that it cannot stall
        // the whole transfer, while still leaving room for a dead loopback port
        // to report a refused connection (measured here at about two seconds).
        mirror_attempt_timeout: Duration::from_millis(4000),
        ..VerifiedDownloadRequest::from_metalink_file(file, destination)
    }
}

fn work_dir(label: &str) -> PathBuf {
    let path = PathBuf::from(env("FETCHPATH_METALINK_WORK")).join(label);
    let _ = fs::remove_dir_all(&path);
    fs::create_dir_all(&path).expect("create the scenario directory");
    path
}

fn escape(value: &str) -> String {
    value
        .chars()
        .flat_map(|character| match character {
            '"' => "\\\"".chars().collect::<Vec<_>>(),
            '\\' => "\\\\".chars().collect(),
            control if control.is_control() => {
                format!("\\u{:04x}", control as u32).chars().collect()
            }
            other => vec![other],
        })
        .collect()
}

fn mirror_json(report: &MirrorReport) -> String {
    format!(
        "{{\"mirror_index\": {}, \"redacted_url\": \"{}\", \"priority\": {}, \"attempts\": {}, \
         \"bytes_delivered\": {}, \"corrupt_observations\": {}, \"slow_observations\": {}, \
         \"offline_observations\": {}, \"protocol_failures\": {}, \"deprioritised\": {}, \
         \"outcome\": \"{:?}\"}}",
        report.mirror_index,
        escape(&report.redacted_url),
        report.priority,
        report.attempts,
        report.bytes_delivered,
        report.corrupt_observations,
        report.slow_observations,
        report.offline_observations,
        report.protocol_failures,
        report.deprioritised,
        report.outcome,
    )
}

fn mirrors_json(mirrors: &[MirrorReport]) -> String {
    let entries: Vec<String> = mirrors.iter().map(mirror_json).collect();
    format!("[{}]", entries.join(", "))
}

fn success_json(
    name: &str,
    description: &str,
    elapsed: Duration,
    done: &VerifiedDownload,
) -> String {
    format!(
        "{{\"scenario\": \"{name}\", \"description\": \"{}\", \"published\": true, \
         \"elapsed_ms\": {}, \"bytes\": {}, \"observed_sha256\": \"{}\", \
         \"verification\": \"{:?}\", \"repaired_pieces\": {:?}, \"conservative_restarts\": {}, \
         \"mirrors\": {}}}",
        escape(description),
        elapsed.as_millis(),
        done.bytes,
        escape(&done.observed_sha256),
        done.verification,
        done.repaired_pieces,
        done.conservative_restarts,
        mirrors_json(&done.mirrors),
    )
}

#[test]
#[ignore = "requires real mirror fixtures; run tests/compatibility/metalink/run.ps1"]
fn bad_slow_and_offline_mirrors_are_survived_and_recorded() {
    let payload = fs::read(env("FETCHPATH_METALINK_PAYLOAD")).expect("read the fixture payload");
    let damaged_piece: usize = env("FETCHPATH_METALINK_DAMAGED_PIECE")
        .parse()
        .expect("damaged piece index");
    let mut scenarios: Vec<String> = Vec::new();

    // 1. Offline, slow, and corrupt mirrors are all ranked ahead of the healthy
    //    one. Trusted piece hashes must localize the damage and repair it.
    let dir = work_dir("mixed-mirrors");
    let destination = dir.join("payload.bin");
    let started = Instant::now();
    let done = download_verified(metalink_request(
        "FETCHPATH_METALINK_MIXED",
        destination.clone(),
    ))
    .expect("the healthy mirror must carry the transfer");
    let elapsed = started.elapsed();

    assert_eq!(fs::read(&destination).unwrap(), payload);
    assert_eq!(done.verification, VerificationLevel::PieceHashes);
    assert_eq!(done.repaired_pieces, vec![damaged_piece]);
    assert_eq!(done.conservative_restarts, 0);
    assert!(
        elapsed < Duration::from_secs(60),
        "the slow mirror stalled the transfer for {elapsed:?}"
    );
    let outcomes: Vec<MirrorOutcome> = done.mirrors.iter().map(|m| m.outcome).collect();
    for expected in [
        MirrorOutcome::Offline,
        MirrorOutcome::Slow,
        MirrorOutcome::Corrupt,
    ] {
        assert!(
            outcomes.contains(&expected),
            "expected a {expected:?} mirror among {outcomes:?}"
        );
    }
    scenarios.push(success_json(
        "mixed_mirrors_selective_repair",
        "offline, slow, and corrupt mirrors are ranked first; trusted piece hashes localize the \
         damaged piece and repair only that byte range from a healthy mirror",
        elapsed,
        &done,
    ));

    // 2. The same corrupt mirror without a piece map. A whole-file digest
    //    localizes nothing, so the only honest move is a conservative restart.
    let dir = work_dir("final-hash-only");
    let destination = dir.join("payload.bin");
    let started = Instant::now();
    let done = download_verified(metalink_request(
        "FETCHPATH_METALINK_FINAL_ONLY",
        destination.clone(),
    ))
    .expect("the healthy mirror must carry the transfer");
    let elapsed = started.elapsed();

    assert_eq!(fs::read(&destination).unwrap(), payload);
    assert_eq!(done.verification, VerificationLevel::FinalHashOnly);
    assert!(
        done.repaired_pieces.is_empty(),
        "a final-only hash cannot localize damage, so no repair may be claimed"
    );
    assert_eq!(done.conservative_restarts, 1);
    scenarios.push(success_json(
        "final_hash_only_conservative_restart",
        "with only a whole-file digest the mismatch is not localized: the whole staged file is \
         discarded and restarted from another mirror, and no piece repair is reported",
        elapsed,
        &done,
    ));

    // 3. Every mirror damaged. Nothing may reach the destination.
    let dir = work_dir("all-mirrors-damaged");
    let destination = dir.join("payload.bin");
    let started = Instant::now();
    let error = download_verified(metalink_request(
        "FETCHPATH_METALINK_ALL_DAMAGED",
        destination.clone(),
    ))
    .expect_err("nothing may be published when no mirror verifies");
    let elapsed = started.elapsed();

    assert!(!destination.exists(), "an unverified file was published");
    assert_eq!(fs::read_dir(&dir).unwrap().count(), 0);
    let (detail, mirrors) = match &error {
        VerifiedDownloadError::VerificationFailed {
            detail, mirrors, ..
        } => (detail.clone(), mirrors.clone()),
        other => panic!("expected a verification failure, got {other:?}"),
    };
    let mut scenario = String::new();
    write!(
        scenario,
        "{{\"scenario\": \"all_mirrors_damaged\", \"description\": \"{}\", \"published\": false, \
         \"elapsed_ms\": {}, \"error\": \"{}\", \"detail\": \"{}\", \"mirrors\": {}}}",
        escape(
            "every mirror serves a piece that fails its trusted hash, so the staged file is \
             discarded and the destination is never created"
        ),
        elapsed.as_millis(),
        escape(&error.to_string()),
        escape(&detail),
        mirrors_json(&mirrors),
    )
    .unwrap();
    scenarios.push(scenario);

    let evidence = format!(
        "{{\n  \"task\": \"FP-019\",\n  \"recorded\": \"{}\",\n  \"harness\": \
         \"tests/compatibility/metalink/run.ps1\",\n  \"payload_bytes\": {},\n  \
         \"payload_sha256\": \"{}\",\n  \"piece_length\": {},\n  \"piece_count\": {},\n  \
         \"damaged_piece\": {},\n  \"note\": \"{}\",\n  \"scenarios\": [\n    {}\n  ]\n}}\n",
        escape(&env("FETCHPATH_METALINK_RECORDED")),
        payload.len(),
        escape(&env("FETCHPATH_METALINK_PAYLOAD_SHA256")),
        env("FETCHPATH_METALINK_PIECE_LENGTH"),
        env("FETCHPATH_METALINK_PIECE_COUNT"),
        damaged_piece,
        escape(
            "every digest here is an observed local digest checked against the fixture metadata; \
             none of it is publisher authenticity evidence"
        ),
        scenarios.join(",\n    "),
    );
    let evidence_path = PathBuf::from(env("FETCHPATH_METALINK_EVIDENCE"));
    if let Some(parent) = evidence_path.parent() {
        fs::create_dir_all(parent).expect("create the evidence directory");
    }
    fs::write(&evidence_path, evidence).expect("write the evidence file");
    println!("recorded mirror evidence at {}", evidence_path.display());
}
