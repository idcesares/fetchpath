//! The bounded wire contract between the engine and its isolated torrent helper.

use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Write};
use std::num::NonZeroU64;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

pub const MAX_REQUEST_BYTES: usize = 16 * 1024;
pub const MAX_SOURCE_BYTES: usize = 8 * 1024;
pub const MAX_PEERS: usize = 32;
pub const MAX_DOWNLOAD_BPS: u32 = 32 * 1024 * 1024;
pub const MAX_UPLOAD_BPS: u32 = 128 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Request {
    pub source: String,
    pub destination: String,
    pub job_id: String,
    pub discover_peers: bool,
    pub upload: bool,
    #[serde(default)]
    pub max_bytes: Option<u64>,
}

impl Request {
    pub fn validate(&self) -> Result<(), &'static str> {
        let source = self.source.as_str();
        if source.is_empty() || source.len() > MAX_SOURCE_BYTES {
            return Err("source.invalid");
        }
        if !(source.starts_with("magnet:?") || source.starts_with("https://")) {
            return Err("source.unsupported");
        }
        if self.destination.is_empty() || self.destination.len() > 4096 {
            return Err("destination.invalid");
        }
        if self.job_id.len() != 36
            || !self
                .job_id
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() || byte == b'-')
        {
            return Err("job.invalid");
        }
        if !self.discover_peers {
            return Err("policy.discovery_required");
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    Progress { received: u64, total: u64 },
    Completed { received: u64, total: u64 },
    Failed { code: String },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JobState {
    Queued,
    Running,
    Cancelling,
    Cancelled,
    Completed,
    Failed,
}

#[derive(Clone, Debug)]
pub struct Snapshot {
    pub state: JobState,
    pub received: u64,
    pub total: Option<u64>,
    pub error: Option<String>,
}

struct Inner {
    snapshot: Snapshot,
    child: Option<Arc<Mutex<Child>>>,
    worker: Option<JoinHandle<()>>,
}

#[derive(Clone)]
pub struct TorrentJob {
    request: Request,
    inner: Arc<Mutex<Inner>>,
    byte_limit: Arc<AtomicU64>,
}

impl TorrentJob {
    pub fn create(request: Request) -> Self {
        Self {
            request,
            byte_limit: Arc::new(AtomicU64::new(0)),
            inner: Arc::new(Mutex::new(Inner {
                snapshot: Snapshot {
                    state: JobState::Queued,
                    received: 0,
                    total: None,
                    error: None,
                },
                child: None,
                worker: None,
            })),
        }
    }

    pub fn snapshot(&self) -> Snapshot {
        self.inner
            .lock()
            .expect("torrent job poisoned")
            .snapshot
            .clone()
    }

    pub fn limit_bytes(&self, limit: u64) {
        self.byte_limit.store(limit.max(1), Ordering::SeqCst);
    }

    pub fn start(&self) -> Result<(), &'static str> {
        self.request.validate()?;
        let mut inner = self.inner.lock().expect("torrent job poisoned");
        if inner.snapshot.state != JobState::Queued {
            return Err("torrent.already_started");
        }
        let helper = std::env::current_exe()
            .map_err(|_| "torrent.helper_unavailable")?
            .with_file_name("fetchpath-torrent-helper.exe");
        if !helper.is_file() {
            return Err("torrent.helper_unavailable");
        }
        let mut command = Command::new(helper);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x08000000); // CREATE_NO_WINDOW
        }
        let mut child = command.spawn().map_err(|_| "torrent.helper_unavailable")?;
        let mut request = self.request.clone();
        request.max_bytes =
            NonZeroU64::new(self.byte_limit.load(Ordering::SeqCst)).map(NonZeroU64::get);
        let payload = serde_json::to_vec(&request).map_err(|_| "torrent.request_invalid")?;
        if payload.len() > MAX_REQUEST_BYTES {
            let _ = child.kill();
            return Err("torrent.request_invalid");
        }
        if let Some(mut stdin) = child.stdin.take() {
            if stdin.write_all(&payload).is_err() {
                let _ = child.kill();
                return Err("torrent.helper_unavailable");
            }
        }
        let stdout = child.stdout.take().ok_or("torrent.helper_unavailable")?;
        let process = Arc::new(Mutex::new(child));
        inner.snapshot.state = JobState::Running;
        inner.child = Some(process.clone());
        let shared = self.inner.clone();
        let request_destination = self.request.destination.clone();
        inner.worker = Some(std::thread::spawn(move || {
            let mut completed = false;
            let mut failed = None;
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if line.len() > 4096 {
                    failed = Some("torrent.protocol_invalid".to_owned());
                    break;
                }
                let Ok(event) = serde_json::from_str::<Event>(&line) else {
                    failed = Some("torrent.protocol_invalid".to_owned());
                    break;
                };
                let mut guard = shared.lock().expect("torrent job poisoned");
                match event {
                    Event::Progress { received, total } => {
                        guard.snapshot.received = received;
                        guard.snapshot.total = Some(total);
                    }
                    Event::Completed { received, total } => {
                        guard.snapshot.received = received;
                        guard.snapshot.total = Some(total);
                        completed = true;
                    }
                    Event::Failed { code } => {
                        failed = Some(
                            if code == "size_limit"
                                || code.starts_with("torrent.")
                                || code.starts_with("destination.")
                                || code.starts_with("stage.")
                                || code.starts_with("policy.")
                            {
                                code
                            } else {
                                "torrent.transfer_failed".into()
                            },
                        );
                    }
                }
            }
            let success = process
                .lock()
                .expect("torrent child poisoned")
                .wait()
                .is_ok_and(|status| status.success());
            let mut guard = shared.lock().expect("torrent job poisoned");
            guard.child = None;
            if guard.snapshot.state == JobState::Cancelling {
                guard.snapshot.state = if completed && success {
                    JobState::Completed
                } else if PathBuf::from(&request_destination).exists() {
                    guard.snapshot.error = Some("torrent.publication_uncertain".into());
                    JobState::Failed
                } else {
                    JobState::Cancelled
                };
            } else if completed && success {
                guard.snapshot.state = JobState::Completed;
            } else {
                guard.snapshot.state = JobState::Failed;
                guard.snapshot.error =
                    Some(failed.unwrap_or_else(|| "torrent.helper_failed".into()));
            }
        }));
        Ok(())
    }

    pub fn cancel(&self) -> &'static str {
        let process = {
            let mut guard = self.inner.lock().expect("torrent job poisoned");
            match guard.snapshot.state {
                JobState::Completed => return "too_late",
                JobState::Cancelled | JobState::Failed => return "already_terminal",
                JobState::Queued => {
                    guard.snapshot.state = JobState::Cancelled;
                    return "accepted";
                }
                JobState::Running | JobState::Cancelling => {
                    guard.snapshot.state = JobState::Cancelling;
                    guard.child.clone()
                }
            }
        };
        if let Some(process) = process {
            let _ = process.lock().expect("torrent child poisoned").kill();
        }
        "accepted"
    }

    pub fn join(&self) {
        let worker = self
            .inner
            .lock()
            .expect("torrent job poisoned")
            .worker
            .take();
        if let Some(worker) = worker {
            let _ = worker.join();
        }
    }
}

pub fn request(
    source: String,
    destination: PathBuf,
    job_id: String,
    discover_peers: bool,
    upload: bool,
) -> Request {
    Request {
        source,
        destination: destination.display().to_string(),
        job_id,
        discover_peers,
        upload,
        max_bytes: None,
    }
}
