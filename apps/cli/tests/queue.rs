//! The queue commands (FP-058), run as separate processes against a real
//! engine over the pipe, each test in its own data folder: exit codes,
//! `--json` shapes, and job references by queue index or id prefix.
#![cfg(windows)]

mod common;

use common::*;
use serde_json::Value;
use std::io::Read;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

#[test]
fn download_keeps_its_exit_codes_and_joins_the_shared_queue() {
    let home = Home::new();
    let base = server(body(40_000), Duration::from_millis(1));
    let link = format!("{base}/file.bin");

    // Saved, into Downloads without a destination; the JSON record keeps
    // its shape.
    let (exit, lines) = home.json(&["download", &link, "--json"]);
    assert_eq!(exit, 0);
    let saved = &lines[0];
    assert_eq!(saved["result"], "downloaded_observed");
    assert_eq!(saved["bytes"], 40_000);
    assert_eq!(saved["checksum_matched"], false);
    let destination = home
        .dir
        .path()
        .join("profile")
        .join("Downloads")
        .join("file.bin");
    assert_eq!(saved["destination"], destination.display().to_string());
    assert_eq!(std::fs::read(&destination).unwrap(), body(40_000));
    let observed = saved["observed_sha256"].as_str().unwrap().to_owned();

    // A relative destination is taken from the folder the command ran in;
    // --quiet prints only the path.
    let quiet = home.run(&["download", &link, "copy.bin", "--quiet"]);
    assert_eq!(code(&quiet), 0, "{}", text(&quiet.stderr));
    assert_eq!(
        text(&quiet.stdout).trim(),
        home.out().join("copy.bin").display().to_string()
    );
    assert!(quiet.stderr.is_empty());

    // 3: never overwritten.
    let conflict = home.run(&["download", &link, "copy.bin"]);
    assert_eq!(code(&conflict), 3, "{}", text(&conflict.stderr));

    // 4: the server refused.
    let (exit, lines) = home.json(&["download", &format!("{base}/missing.bin"), "--json"]);
    assert_eq!(exit, 4);
    assert_eq!(lines[0]["result"], "failed");
    assert_eq!(lines[0]["error_code"], "source.transfer_failed");

    // 5: a checksum that does not match saves nothing.
    let wrong = "0".repeat(64);
    let mismatch = home.run(&["download", &link, "checked.bin", "--sha256", &wrong]);
    assert_eq!(code(&mismatch), 5, "{}", text(&mismatch.stderr));
    assert!(!home.out().join("checked.bin").exists());
    // ... and one that does saves it.
    let matched = home.run(&["download", &link, "checked.bin", "--sha256", &observed]);
    assert_eq!(code(&matched), 0, "{}", text(&matched.stderr));
    assert!(text(&matched.stderr).contains("matches the checksum you entered"));

    // 2: bad input, refused before anything is queued.
    assert_eq!(code(&home.run(&["download", &link, "--sha256", "12"])), 2);
    assert_eq!(code(&home.run(&["download"])), 2);
    assert_eq!(code(&home.run(&["download", "ftp:/nope", "x.bin"])), 2);

    // 6: the folder cannot be written. Automatic retries are turned off so
    // the first failure is the last.
    assert_eq!(code(&home.run(&["settings", "auto-retry", "off"])), 0);
    std::fs::write(home.out().join("afile"), b"x").unwrap();
    let unwritable = home.run(&["download", &link, "afile/inside.bin"]);
    assert_eq!(code(&unwritable), 6, "{}", text(&unwritable.stderr));

    // Every download is in the shared queue and its history.
    let jobs = home.jobs();
    assert_eq!(jobs.len(), 7, "{jobs:?}");
    assert!(jobs.iter().all(|job| job["job_id"].is_string()));
    let (exit, lines) = home.json(&["history", "checked", "--json"]);
    assert_eq!(exit, 0);
    assert_eq!(lines[0]["type"], "Jobs");
    let states: Vec<&str> = lines[0]["jobs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|job| job["state"].as_str().unwrap())
        .collect();
    assert_eq!(states, ["completed", "failed"]);
}

#[test]
fn a_download_cancelled_from_another_client_exits_130() {
    let home = Home::new();
    let base = server(body(2_000_000), Duration::from_millis(20));
    let mut download = home
        .command(&["download", &format!("{base}/slow.bin"), "--json"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    home.wait_for_state(0, "running");
    // Someone else following the same job sees it end cancelled too.
    let mut watch = home
        .command(&["watch", "1"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    thread::sleep(Duration::from_millis(500));
    let cancel = home.run(&["cancel", "1"]);
    assert_eq!(code(&cancel), 0, "{}", text(&cancel.stderr));

    let deadline = Instant::now() + Duration::from_secs(20);
    let status = loop {
        if let Some(status) = download.try_wait().unwrap() {
            break status;
        }
        assert!(Instant::now() < deadline, "download kept waiting");
        thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(status.code(), Some(130));
    let deadline = Instant::now() + Duration::from_secs(20);
    let watched = loop {
        if let Some(status) = watch.try_wait().unwrap() {
            break status;
        }
        assert!(Instant::now() < deadline, "watch kept waiting");
        thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(watched.code(), Some(130));
    let mut stdout = String::new();
    download
        .stdout
        .take()
        .unwrap()
        .read_to_string(&mut stdout)
        .unwrap();
    let result: Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(result["result"], "cancelled");
    assert!(!home.out().join("slow.bin").exists());
}

#[test]
fn queue_commands_control_jobs_by_index_and_id_prefix() {
    let home = Home::new();
    let base = server(body(3_000_000), Duration::from_millis(15));

    // A scheduled job, then a running one: the newest is number 1.
    let (exit, lines) = home.json(&["add", &format!("{base}/later.bin"), "--at", "+1h", "--json"]);
    assert_eq!(exit, 0);
    assert_eq!(lines[0]["type"], "Job");
    assert_eq!(lines[0]["job"]["state"], "queued");
    assert!(lines[0]["job"]["not_before"].is_string());
    let later = lines[0]["job"]["job_id"].as_str().unwrap().to_owned();

    let (exit, lines) = home.json(&["add", &format!("{base}/now.bin"), "--to", "./", "--json"]);
    assert_eq!(exit, 0);
    let now = lines[0]["job"]["job_id"].as_str().unwrap().to_owned();
    assert_eq!(
        lines[0]["job"]["destination"],
        home.out().join("now.bin").display().to_string()
    );
    assert_eq!(home.jobs()[0]["job_id"], now.as_str());
    home.wait_for_state(0, "running");

    // Pause by index, resume by id prefix.
    let (exit, lines) = home.json(&["pause", "1", "--json"]);
    assert_eq!(exit, 0);
    assert_eq!(lines[0]["type"], "Control");
    assert_eq!(lines[0]["outcome"], "accepted");
    home.wait_for_state(0, "paused");
    let (exit, lines) = home.json(&["show", &now[..6], "--json"]);
    assert_eq!(exit, 0);
    assert_eq!(lines[0]["job"]["state"], "paused");
    let (exit, lines) = home.json(&["resume", &now[..6], "--json"]);
    assert_eq!(exit, 0, "{lines:?}");
    assert_eq!(lines[0]["type"], "Job");

    // watch follows it to the end as protocol messages.
    let (exit, lines) = home.json(&["watch", &now[..8], "--json"]);
    assert_eq!(exit, 0);
    assert!(matches!(
        lines[0]["type"].as_str(),
        Some("Subscribed" | "SnapshotBoundary")
    ));
    assert!(
        lines[1..]
            .iter()
            .all(|line| matches!(line["message"].as_str(), Some("event" | "progress")))
    );
    assert!(
        lines.iter().any(|line| line["kind"] == "state_changed"
            && line["public_payload"]["state"] == "completed"),
        "{lines:?}"
    );
    assert_eq!(
        std::fs::read(home.out().join("now.bin")).unwrap(),
        body(3_000_000)
    );

    // Cancel the scheduled one by prefix, then remove both.
    let (exit, lines) = home.json(&["cancel", &later[..5], "--json"]);
    assert_eq!(exit, 0);
    assert_eq!(lines[0]["outcome"], "accepted");
    home.wait_for_state(1, "cancelled");
    let (exit, lines) = home.json(&["rm", "1", "2", "--json"]);
    assert_eq!(exit, 0);
    assert_eq!(lines.len(), 2);
    assert!(lines.iter().all(|line| line["type"] == "Removed"));
    assert!(home.jobs().is_empty());

    // A failed download can be retried; references that match nothing are
    // bad input.
    let failed = home.run(&["add", &format!("{base}/missing.bin"), "--wait"]);
    assert_eq!(code(&failed), 4, "{}", text(&failed.stderr));
    let (exit, lines) = home.json(&["retry", "1", "--json"]);
    assert_eq!(exit, 0, "{lines:?}");
    assert_eq!(lines[0]["type"], "Job");
    let (exit, lines) = home.json(&["show", "7", "--json"]);
    assert_eq!(exit, 2);
    assert_eq!(lines[0]["error"]["code"], "input.invalid_request");
    assert_eq!(code(&home.run(&["pause", "zzzzz"])), 2);
    assert_eq!(code(&home.run(&["pause"])), 2);
}

#[test]
fn add_wait_batch_settings_inspect_and_engine_status() {
    let home = Home::new();
    let base = server(body(50_000), Duration::from_millis(1));

    // add --wait ends like download: saved, or the checksum's exit code.
    let saved = home.run(&["add", &format!("{base}/a.bin"), "--to", "a.bin", "--wait"]);
    assert_eq!(code(&saved), 0, "{}", text(&saved.stderr));
    assert!(text(&saved.stdout).contains(&home.out().join("a.bin").display().to_string()));
    let wrong = "f".repeat(64);
    let mismatch = home.run(&[
        "add",
        &format!("{base}/b.bin"),
        "--to",
        "b.bin",
        "--sha256",
        &wrong,
        "--wait",
    ]);
    assert_eq!(code(&mismatch), 5, "{}", text(&mismatch.stderr));

    // Without --to, a job goes to Downloads.
    let (exit, lines) = home.json(&["add", &format!("{base}/c.bin"), "--json"]);
    assert_eq!(exit, 0);
    assert_eq!(
        lines[0]["job"]["destination"],
        home.dir
            .path()
            .join("profile")
            .join("Downloads")
            .join("c.bin")
            .display()
            .to_string()
    );

    // A batch file: a link with its own destination, a comment, a plain link.
    let list = home.out().join("links.txt");
    std::fs::write(
        &list,
        format!("{base}/d.bin d.bin\n# skipped\n\n{base}/e.bin\n"),
    )
    .unwrap();
    let batch = home.run(&["batch", "links.txt", "--to", "./", "--wait"]);
    assert_eq!(code(&batch), 0, "{}", text(&batch.stderr));
    assert!(home.out().join("d.bin").exists());
    assert!(home.out().join("e.bin").exists());
    assert_eq!(code(&home.run(&["batch", "nothing-here.txt"])), 2);

    // Settings: read one, change one (the engine's applied value comes
    // back), refuse an unknown name or a wrong type.
    let theme = home.run(&["settings", "theme"]);
    assert_eq!(text(&theme.stdout).trim(), "system");
    let (exit, lines) = home.json(&["settings", "max-active-downloads", "2", "--json"]);
    assert_eq!(exit, 0);
    assert_eq!(lines[0]["type"], "Settings");
    assert_eq!(lines[0]["view"]["settings"]["max_active_downloads"], 2);
    assert_eq!(code(&home.run(&["settings", "no-such-thing"])), 2);
    assert_eq!(code(&home.run(&["settings", "auto-retry", "maybe"])), 2);

    // inspect reports the engine's refusal as JSON and a nonzero exit.
    let (exit, lines) = home.json(&["inspect", &format!("{base}/page"), "--json"]);
    assert_ne!(exit, 0);
    assert!(lines[0]["error"]["code"].is_string());

    let (exit, lines) = home.json(&["engine", "status", "--json"]);
    assert_eq!(exit, 0);
    assert_eq!(lines[0]["type"], "EngineStatus");
    assert_eq!(lines[0]["status"]["schema_version"], 1);

    // Unknown options are bad input for every command.
    assert_eq!(code(&home.run(&["ls", "--bogus"])), 2);
    assert_eq!(code(&home.run(&["add", "--wait"])), 2);
}

#[test]
fn a_command_that_starts_the_engine_returns_at_once_to_a_script_reading_its_output() {
    // No engine yet: the command starts one, which then stays for its idle
    // grace. The engine must not hold the script's pipe open that long.
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    let run = |args: &[&str]| {
        Command::new(EXE)
            .args(args)
            .env("FETCHPATH_APP_DATA_DIR", &data)
            .stdin(Stdio::null())
            .output()
            .unwrap()
    };
    let started = Instant::now();
    let listed = run(&["ls"]);
    let took = started.elapsed();
    let stopped = run(&["engine", "stop"]);
    assert_eq!(code(&listed), 0, "{}", text(&listed.stderr));
    assert_eq!(text(&listed.stdout).trim(), "No downloads.");
    assert!(
        took < Duration::from_secs(15),
        "the script waited {took:?} for the engine to let go of its output"
    );
    assert_eq!(code(&stopped), 0);
}

#[test]
fn without_a_terminal_the_interactive_mode_prints_help_and_exits_2() {
    let data = tempfile::tempdir().unwrap();
    for args in [&[][..], &["--plain"][..], &["--json"][..]] {
        let output = Command::new(EXE)
            .args(args)
            .env("FETCHPATH_APP_DATA_DIR", data.path())
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert_eq!(code(&output), 2, "{args:?}");
        assert!(output.stdout.is_empty());
        assert!(text(&output.stderr).contains("Usage:"), "{args:?}");
    }
    // It never reached for the engine, so it made no data folder.
    assert_eq!(std::fs::read_dir(data.path()).unwrap().count(), 0);
}

#[test]
fn tools_say_what_is_missing_and_never_install_from_a_script_without_yes() {
    let home = Home::new();
    let status = home.run(&["tools"]);
    assert_eq!(code(&status), 0);
    let said = text(&status.stdout);
    assert!(said.contains("not set up"), "{said}");
    assert!(said.contains("yt-dlp") && said.contains("ffmpeg"), "{said}");
    assert!(said.contains("fetchpath tools install"), "{said}");

    let install = home.run(&["tools", "install"]);
    assert_eq!(code(&install), 2);
    let said = text(&install.stdout);
    assert!(said.contains("Unlicense") && said.contains("GPL"), "{said}");
    assert!(text(&install.stderr).contains("--yes"));
    assert!(
        !home.dir.path().join("data").join("media-tools").exists(),
        "nothing was downloaded"
    );
    assert_eq!(code(&home.run(&["tools", "use", "nowhere-at-all"])), 2);
}

#[test]
fn rules_place_downloads_explain_themselves_and_can_require_a_checksum() {
    let home = Home::new();
    let base = server(body(30_000), Duration::from_millis(1));
    let sorted = home.out().join("sorted");
    std::fs::create_dir_all(&sorted).unwrap();
    let sorted_arg = sorted.display().to_string();

    let added = home.run(&[
        "rules",
        "add",
        "--name",
        "Archives",
        "--type",
        "zip",
        "--min-size",
        "20KB",
        "--folder",
        &sorted_arg,
    ]);
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr));
    assert!(text(&added.stdout).contains("Rule 1 (Archives): a .zip file, 20.0 KiB or more"));
    let guarded = home.run(&["rules", "add", "--type", "iso", "--require-checksum"]);
    assert_eq!(guarded.status.code(), Some(0), "{}", text(&guarded.stderr));
    let refused = home.run(&["rules", "add", "--folder", &sorted_arg]);
    assert_eq!(refused.status.code(), Some(2), "a rule needs a condition");

    let tested = home.run(&["rules", "test", &format!("{base}/pack.zip")]);
    let said = text(&tested.stdout);
    assert!(
        said.contains("Rule 1 (Archives) decides: save in"),
        "{said}"
    );
    assert!(
        said.contains("the file is a .zip; 29.3 KiB is at least 20.0 KiB"),
        "{said}"
    );

    // Without --to, the engine puts it where the rule says.
    let saved = home.run(&["add", &format!("{base}/pack.zip"), "--wait", "--quiet"]);
    assert_eq!(saved.status.code(), Some(0), "{}", text(&saved.stderr));
    assert_eq!(
        std::fs::read(sorted.join("pack.zip")).unwrap(),
        body(30_000)
    );

    // The second rule refuses an .iso without a checksum.
    let unchecked = home.run(&["add", &format!("{base}/disc.iso")]);
    assert_ne!(unchecked.status.code(), Some(0));
    assert!(
        text(&unchecked.stderr).contains("requires a SHA-256"),
        "{}",
        text(&unchecked.stderr)
    );

    let removed = home.run(&["rules", "rm", "1"]);
    assert!(!text(&removed.stdout).contains("Archives"));
}

/// FP-061: an agent's requests outside its folders wait for the person, who
/// answers them from the command line (the terminal's `/approve` and
/// `/deny` and its approval card send the same commands).
#[test]
fn an_agent_request_is_approved_and_denied_from_the_command_line() {
    use fetchpath_protocol::command::{
        Command as Wire, ConflictPolicy, DestinationIntent, JobInput, JobRequest,
    };
    use fetchpath_protocol::launch::{self, EngineHome};
    use fetchpath_protocol::message::CommandResult;
    use fetchpath_protocol::model::JobState;
    use fetchpath_protocol::pipe::Limits;
    use fetchpath_protocol::principal::{AgentName, AgentPolicy, Principal};
    use fetchpath_protocol::{ClientId, EngineClient, SensitiveUrl, Timestamp};

    let home = Home::new();
    let data = EngineHome::at(home.dir.path().join("data"));
    let granted = home.dir.path().join("granted");
    std::fs::create_dir_all(&granted).unwrap();
    let person = launch::attach(&data, Limits::default()).unwrap();
    person
        .send(
            &ClientId::random(),
            Wire::SetAgentPolicy {
                agent: AgentName::try_from("helper").unwrap(),
                policy: Some(AgentPolicy {
                    folders: vec![granted.display().to_string()],
                    ..AgentPolicy::default()
                }),
            },
        )
        .unwrap();
    let agent = launch::attach(&data, Limits::default())
        .unwrap()
        .with_principal(Principal::try_from("agent:helper").unwrap());
    // Scheduled an hour ahead, so an approved request waits instead of
    // reaching for a server.
    let later = Some(Timestamp::from_unix_ms(
        Timestamp::now().unix_ms() + 3_600_000,
    ));
    let ask = |name: &str| {
        let request = JobRequest::File {
            input: JobInput::Url {
                url: SensitiveUrl::try_from(format!("http://127.0.0.1:9/{name}")).unwrap(),
            },
            destination: DestinationIntent {
                path: home.out().join(name).display().to_string(),
                conflict: ConflictPolicy::Ask,
            },
            not_before: later,
            expected_sha256: None,
        };
        match agent
            .send(&ClientId::random(), Wire::CreateJob { request })
            .unwrap()
        {
            CommandResult::Job { job } => job,
            other => panic!("not a job: {other:?}"),
        }
    };
    let approved = ask("approve-me.bin");
    let denied = ask("deny-me.bin");
    assert_eq!(approved.state, JobState::AwaitingApproval);
    let id = |job: &fetchpath_protocol::JobSnapshot| job.job_id.as_str()[..8].to_owned();

    let output = home.run(&["approve", &id(&approved)]);
    assert_eq!(code(&output), 0, "{}", text(&output.stderr));
    assert!(
        text(&output.stdout).starts_with("Approved "),
        "{}",
        text(&output.stdout)
    );
    let output = home.run(&["deny", &id(&denied)]);
    assert_eq!(code(&output), 0, "{}", text(&output.stderr));
    assert!(
        text(&output.stdout).contains("it will not download"),
        "{}",
        text(&output.stdout)
    );

    let states: Vec<(String, String)> = home
        .jobs()
        .iter()
        .map(|job| {
            (
                job["job_id"].as_str().unwrap()[..8].to_owned(),
                job["state"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    assert!(
        states.contains(&(id(&approved), "queued".to_owned())),
        "{states:?}"
    );
    assert!(
        states.contains(&(id(&denied), "cancelled".to_owned())),
        "{states:?}"
    );

    // Nothing is waiting any more, so answering again is refused.
    let again = home.run(&["approve", &id(&approved)]);
    assert_ne!(code(&again), 0);
}
