//! Shared by the CLI's integration tests: a data folder with its own
//! engine, and a local HTTP server.
#![allow(dead_code)]

use serde_json::Value;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Child, Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

pub const EXE: &str = env!("CARGO_BIN_EXE_fetchpath");

/// A data folder with its own engine, stopped when the test ends.
pub struct Home {
    pub dir: tempfile::TempDir,
    engine: Child,
}

impl Home {
    pub fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("profile").join("Downloads")).unwrap();
        std::fs::create_dir_all(dir.path().join("out")).unwrap();
        // The engine places a download sent without a folder; it must never
        // reach the real Downloads folder.
        let data = dir.path().join("data");
        std::fs::create_dir_all(&data).unwrap();
        std::fs::write(
            data.join("settings-v1.json"),
            serde_json::json!({
                "schemaVersion": 1,
                "defaultDestinationDir": dir.path().join("profile").join("Downloads"),
            })
            .to_string(),
        )
        .unwrap();
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

    pub fn out(&self) -> PathBuf {
        self.dir.path().join("out")
    }

    pub fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(EXE);
        command
            .args(args)
            .current_dir(self.out())
            .env("FETCHPATH_APP_DATA_DIR", self.dir.path().join("data"))
            .env("USERPROFILE", self.dir.path().join("profile"))
            .stdin(Stdio::null());
        command
    }

    pub fn run(&self, args: &[&str]) -> Output {
        self.command(args).output().unwrap()
    }

    /// Runs a command and returns its exit code and its standard output as
    /// JSON lines.
    pub fn json(&self, args: &[&str]) -> (i32, Vec<Value>) {
        let output = self.run(args);
        let lines = String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap_or_else(|_| panic!("not JSON: {line}")))
            .collect();
        (output.status.code().unwrap(), lines)
    }

    pub fn jobs(&self) -> Vec<Value> {
        let (code, lines) = self.json(&["ls", "--json"]);
        assert_eq!(code, 0);
        assert_eq!(lines[0]["type"], "Jobs");
        lines[0]["jobs"].as_array().unwrap().clone()
    }

    pub fn wait_for_state(&self, index: usize, state: &str) -> Value {
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

pub fn code(output: &Output) -> i32 {
    output.status.code().unwrap()
}

pub fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// Serves `body` at any path, slowly, with ranges and a strong validator;
/// paths containing `missing` answer 404.
pub fn server(body: Vec<u8>, pause: Duration) -> String {
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

pub fn body(length: usize) -> Vec<u8> {
    (0..length).map(|index| (index % 251) as u8).collect()
}
