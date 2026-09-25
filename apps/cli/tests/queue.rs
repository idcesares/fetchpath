//! The queue commands (FP-058), run as separate processes against a real
//! engine over the pipe, each test in its own data folder: exit codes,
//! `--json` shapes, and job references by queue index or id prefix.
#![cfg(windows)]

use serde_json::Value;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Child, Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const EXE: &str = env!("CARGO_BIN_EXE_fetchpath");

/// A data folder with its own engine, stopped when the test ends.
struct Home {
    dir: tempfile::TempDir,
    engine: Child,
}

impl Home {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("profile").join("Downloads")).unwrap();
        std::fs::create_dir_all(dir.path().join("out")).unwrap();
        let engine = Command::new(EXE)
            .args(["engine", "--idle-grace-ms", "3000"])
            .env("FETCHPATH_APP_DATA_DIR", dir.path().join("data"))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let home = Self { dir, engine };
        let deadline = Instant::now() + Duration::from_secs(15);
        while home.run(&["engine", "status"]).status.code() != Some(0) {
            assert!(Instant::now() < deadline, "the engine never started");
            thread::sleep(Duration::from_millis(50));
        }
        home
    }

    fn out(&self) -> PathBuf {
        self.dir.path().join("out")
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(EXE);
        command
            .args(args)
            .current_dir(self.out())
            .env("FETCHPATH_APP_DATA_DIR", self.dir.path().join("data"))
            .env("USERPROFILE", self.dir.path().join("profile"))
            .stdin(Stdio::null());
        command
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command(args).output().unwrap()
    }

    /// Runs a command and returns its exit code and its standard output as
    /// JSON lines.
    fn json(&self, args: &[&str]) -> (i32, Vec<Value>) {
        let output = self.run(args);
        let lines = String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap_or_else(|_| panic!("not JSON: {line}")))
            .collect();
        (output.status.code().unwrap(), lines)
    }

    fn jobs(&self) -> Vec<Value> {
        let (code, lines) = self.json(&["ls", "--json"]);
        assert_eq!(code, 0);
        assert_eq!(lines[0]["type"], "Jobs");
        lines[0]["jobs"].as_array().unwrap().clone()
    }

    fn wait_for_state(&self, index: usize, state: &str) -> Value {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            // Another process may not have created the job yet.
            let job = self.jobs().get(index).cloned().unwrap_or(Value::Null);
            if job["state"] == state {
                return job;
            }
            assert!(Instant::now() < deadline, "never {state}: {job}");
            thread::sleep(Duration::from_millis(50));
        }
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = self.run(&["engine", "stop"]);
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            if self.engine.try_wait().ok().flatten().is_some() {
                return;
            }
            thread::sleep(Duration::from_millis(50));
        }
        let _ = self.engine.kill();
    }
}

fn code(output: &Output) -> i32 {
    output.status.code().unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// Serves `body` at any path, slowly, with ranges and a strong validator;
/// paths containing `missing` answer 404.
fn server(body: Vec<u8>, pause: Duration) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let body = body.clone();
            thread::spawn(move || {
                let mut request = Vec::new();
                let mut buffer = [0_u8; 1024];
                while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                    match stream.read(&mut buffer) {
                        Ok(0) | Err(_) => return,
                        Ok(read) => request.extend_from_slice(&buffer[..read]),
                    }
                }
                let text = String::from_utf8_lossy(&request).to_ascii_lowercase();
                if text.lines().next().unwrap_or("").contains("missing") {
                    let _ = stream.write_all(
                        b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    );
                    return;
                }
                let range = text
                    .lines()
                    .find_map(|line| line.strip_prefix("range: bytes="))
                    .map(str::to_owned);
                let start = range
                    .as_deref()
                    .and_then(|range| range.split('-').next())
                    .and_then(|start| start.trim().parse::<usize>().ok());
                let end = range
                    .as_deref()
                    .and_then(|range| range.split('-').nth(1))
                    .and_then(|end| end.trim().parse::<usize>().ok())
                    .unwrap_or(body.len() - 1)
                    .min(body.len() - 1);
                let head = match start {
                    Some(start) => format!(
                        "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: bytes {start}-{end}/{}\r\nETag: \"fixed\"\r\nAccept-Ranges: bytes\r\nConnection: close\r\n\r\n",
                        end - start + 1,
                        body.len()
                    ),
                    None => format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nETag: \"fixed\"\r\nAccept-Ranges: bytes\r\nConnection: close\r\n\r\n",
                        body.len()
                    ),
                };
                if stream.write_all(head.as_bytes()).is_err() {
                    return;
                }
                let from = start.unwrap_or(0);
                let to = if start.is_some() { end + 1 } else { body.len() };
                for chunk in body[from..to].chunks(16 * 1024) {
                    if stream.write_all(chunk).is_err() {
                        return;
                    }
                    thread::sleep(pause);
                }
            });
        }
    });
    base
}

fn body(length: usize) -> Vec<u8> {
    (0..length).map(|index| (index % 251) as u8).collect()
}

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
