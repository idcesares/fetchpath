//! Supervised media inspection and download adapter.
//!
//! The helper is deliberately treated as an untrusted subprocess: arguments are
//! passed without a shell, output is bounded, cancellation terminates the process
//! tree, and an output is published only after an independent ffprobe check.

pub mod setup;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use url::Url;

/// Per stream. Inspection output is kept small by `inspect_args`; this is the
/// ceiling, not the expected size.
const OUTPUT_LIMIT: usize = 8 * 1024 * 1024;
const INSPECT_TIMEOUT: Duration = Duration::from_secs(60);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(6 * 60 * 60);

#[derive(Clone, Debug)]
pub struct MediaTools {
    pub yt_dlp: PathBuf,
    pub ffmpeg_dir: PathBuf,
    pub ffprobe: PathBuf,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MediaInspection {
    pub title: String,
    pub duration_seconds: Option<f64>,
    pub variants: Vec<MediaVariant>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MediaVariant {
    pub id: String,
    pub label: String,
    pub kind: MediaKind,
    pub extension: String,
    pub height: Option<u32>,
    pub fps: Option<u32>,
    #[serde(skip)]
    selector: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MediaKind {
    Video,
    Audio,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaJobState {
    Queued,
    Running,
    Cancelling,
    Completed,
    Cancelled,
    Failed,
}

#[derive(Clone, Debug)]
pub struct MediaJobSnapshot {
    pub state: MediaJobState,
    pub bytes_received: u64,
    pub destination: Option<PathBuf>,
    pub observed_sha256: Option<String>,
    pub error: Option<String>,
    pub action: Option<String>,
}

#[derive(Debug)]
pub enum MediaError {
    InvalidSource,
    HelperUnavailable,
    HelperCrash,
    SourceExpired,
    UnknownVariant,
    DestinationConflict,
    Cancelled,
    TimedOut,
    InvalidOutput,
    /// More arrived than the job's byte cap allows.
    SizeLimit,
    Io(io::Error),
}

impl std::fmt::Display for MediaError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let message = match self {
            Self::InvalidSource => "invalid_source: Enter an HTTP or HTTPS media address.",
            Self::HelperUnavailable => {
                "helper_unavailable: Media tools are unavailable. Configure the Fetchpath media tool directory."
            }
            Self::SizeLimit => {
                "size_limit: The download passed the size allowed without asking, so it stopped."
            }
            Self::HelperCrash => {
                "helper_crash: The media helper stopped unexpectedly. Retry this download."
            }
            Self::SourceExpired => {
                "source_expired: This media session expired. Refresh the source and inspect it again."
            }
            Self::UnknownVariant => {
                "unknown_variant: The selected quality is no longer available. Inspect the source again."
            }
            Self::DestinationConflict => {
                "destination_conflict: A file already exists at this destination."
            }
            Self::Cancelled => "cancelled: The media download was cancelled before publication.",
            Self::TimedOut => "helper_timeout: The media helper exceeded its time limit.",
            Self::InvalidOutput => {
                "verification_failed: The media helper did not produce a valid selected output."
            }
            Self::Io(_) => {
                "media_io: Fetchpath could not safely prepare or publish the media output."
            }
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for MediaError {}

impl From<io::Error> for MediaError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl MediaTools {
    pub fn new(
        yt_dlp: impl Into<PathBuf>,
        ffmpeg_dir: impl Into<PathBuf>,
        ffprobe: impl Into<PathBuf>,
    ) -> Result<Self, MediaError> {
        let tools = Self {
            yt_dlp: yt_dlp.into(),
            ffmpeg_dir: ffmpeg_dir.into(),
            ffprobe: ffprobe.into(),
        };
        if !tools.yt_dlp.is_file() || !tools.ffmpeg_dir.is_dir() || !tools.ffprobe.is_file() {
            return Err(MediaError::HelperUnavailable);
        }
        Ok(tools)
    }

    pub fn discover() -> Result<Self, MediaError> {
        if let (Some(yt_dlp), Some(ffmpeg_dir)) = (
            std::env::var_os("FETCHPATH_YT_DLP"),
            std::env::var_os("FETCHPATH_FFMPEG_DIR"),
        ) {
            let ffmpeg_dir = PathBuf::from(ffmpeg_dir);
            return Self::new(
                PathBuf::from(yt_dlp),
                &ffmpeg_dir,
                ffmpeg_dir.join(executable("ffprobe")),
            );
        }
        let root = std::env::var_os("FETCHPATH_MEDIA_TOOLS_DIR")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::current_exe()
                    .ok()?
                    .parent()
                    .map(|parent| parent.join("media-tools"))
            })
            .ok_or(MediaError::HelperUnavailable)?;
        Self::discover_in(&root)
    }

    /// Resolves the helpers inside one directory the user chose.
    ///
    /// Accepts the two layouts people actually end up with: the executables
    /// sitting directly in the folder, and an ffmpeg release extracted with its
    /// own `bin/` subdirectory left in place. Requiring one exact arrangement
    /// turns a correct download into a support question.
    pub fn discover_in(root: &Path) -> Result<Self, MediaError> {
        let yt_dlp = root.join(executable("yt-dlp"));
        let direct_bin = root.join("bin");
        let ffmpeg_dir = if direct_bin.join(executable("ffmpeg")).is_file() {
            direct_bin
        } else {
            root.to_path_buf()
        };
        Self::new(yt_dlp, &ffmpeg_dir, ffmpeg_dir.join(executable("ffprobe")))
    }

    /// The file name of a helper executable on this platform.
    pub fn executable_name(name: &str) -> String {
        executable(name)
    }

    pub fn inspect(&self, source: &str) -> Result<MediaInspection, MediaError> {
        validate_source(source)?;
        let cancel = AtomicBool::new(false);
        let pid = AtomicU32::new(0);
        let args = inspect_args(source);
        let output = run_helper(&self.yt_dlp, &args, &cancel, &pid, INSPECT_TIMEOUT)?;
        if !output.status.success() {
            return Err(classify_helper_failure(&output.stderr));
        }
        parse_inspection(&output.stdout)
    }

    pub fn versions(&self) -> Result<(String, String), MediaError> {
        let cancel = AtomicBool::new(false);
        let pid = AtomicU32::new(0);
        let yt = run_helper(
            &self.yt_dlp,
            &["--version".into()],
            &cancel,
            &pid,
            Duration::from_secs(10),
        )?;
        let ffmpeg = run_helper(
            &self.ffmpeg_dir.join(executable("ffmpeg")),
            &["-version".into()],
            &cancel,
            &pid,
            Duration::from_secs(10),
        )?;
        if !yt.status.success() || !ffmpeg.status.success() {
            return Err(MediaError::HelperCrash);
        }
        Ok((first_line(&yt.stdout), first_line(&ffmpeg.stdout)))
    }
}

#[derive(Clone)]
pub struct MediaJob {
    source: String,
    variant_id: String,
    destination: PathBuf,
    tools: MediaTools,
    inner: Arc<JobInner>,
}

struct JobInner {
    snapshot: Mutex<MediaJobSnapshot>,
    cancel: AtomicBool,
    /// Most bytes this job may write; 0 for no cap.
    byte_cap: AtomicU64,
    /// Set when the cap stopped the helper, so the stop reads as that.
    over_cap: AtomicBool,
    active_pid: AtomicU32,
    worker: Mutex<Option<JoinHandle<()>>>,
}

impl MediaJob {
    pub fn create(
        source: String,
        variant_id: String,
        destination: PathBuf,
        tools: MediaTools,
    ) -> Self {
        Self {
            source,
            variant_id,
            destination,
            tools,
            inner: Arc::new(JobInner {
                snapshot: Mutex::new(MediaJobSnapshot {
                    state: MediaJobState::Queued,
                    bytes_received: 0,
                    destination: None,
                    observed_sha256: None,
                    error: None,
                    action: None,
                }),
                cancel: AtomicBool::new(false),
                byte_cap: AtomicU64::new(0),
                over_cap: AtomicBool::new(false),
                active_pid: AtomicU32::new(0),
                worker: Mutex::new(None),
            }),
        }
    }

    pub fn start(&self) -> Result<(), &'static str> {
        self.start_with_completion(|| {})
    }

    /// Wakes the host after the terminal snapshot, outside job locks.
    /// The callback must be quick and must not panic; join semantics are unchanged.
    pub fn start_with_completion(
        &self,
        on_complete: impl FnOnce() + Send + 'static,
    ) -> Result<(), &'static str> {
        let mut worker = self.inner.worker.lock().expect("media worker poisoned");
        if worker.is_some() || self.snapshot().state != MediaJobState::Queued {
            return Err("invalid_state");
        }
        self.inner.cancel.store(false, Ordering::Release);
        self.update(|snapshot| snapshot.state = MediaJobState::Running);
        let job = self.clone();
        *worker = Some(thread::spawn(move || {
            match job.execute() {
                Ok((bytes, hash)) => job.update(|snapshot| {
                    snapshot.state = MediaJobState::Completed;
                    snapshot.bytes_received = bytes;
                    snapshot.destination = Some(job.destination.clone());
                    snapshot.observed_sha256 = Some(hash);
                }),
                Err(MediaError::Cancelled) => job.update(|snapshot| {
                    snapshot.state = MediaJobState::Cancelled;
                    snapshot.error = Some(MediaError::Cancelled.to_string());
                }),
                Err(error) => job.update(|snapshot| {
                    snapshot.state = MediaJobState::Failed;
                    snapshot.action = Some(
                        match error {
                            MediaError::SourceExpired | MediaError::UnknownVariant => {
                                "refresh_source"
                            }
                            MediaError::DestinationConflict => "choose_new_path",
                            MediaError::HelperUnavailable => "configure_media_tools",
                            _ => "retry",
                        }
                        .into(),
                    );
                    snapshot.error = Some(error.to_string());
                }),
            }
            job.inner.active_pid.store(0, Ordering::Release);
            on_complete();
        }));
        Ok(())
    }

    /// Caps what this job may write, for an agent's download (FP-067): the
    /// helper is stopped once its work passes `bytes`, and an output over it
    /// is never published. Set before `start`.
    pub fn limit_bytes(&self, bytes: u64) {
        self.inner.byte_cap.store(bytes, Ordering::Release);
    }

    fn cap(&self) -> Option<u64> {
        Some(self.inner.byte_cap.load(Ordering::Acquire)).filter(|cap| *cap > 0)
    }

    pub fn cancel(&self) {
        self.inner.cancel.store(true, Ordering::Release);
        self.update(|snapshot| match snapshot.state {
            MediaJobState::Queued => {
                snapshot.state = MediaJobState::Cancelled;
                snapshot.error = Some(MediaError::Cancelled.to_string());
            }
            MediaJobState::Running => snapshot.state = MediaJobState::Cancelling,
            _ => {}
        });
        let pid = self.inner.active_pid.load(Ordering::Acquire);
        if pid != 0 {
            terminate_tree(pid);
        }
    }

    pub fn join(&self) {
        if let Some(worker) = self
            .inner
            .worker
            .lock()
            .expect("media worker poisoned")
            .take()
        {
            let _ = worker.join();
        }
    }

    pub fn snapshot(&self) -> MediaJobSnapshot {
        self.inner
            .snapshot
            .lock()
            .expect("media snapshot poisoned")
            .clone()
    }

    fn update(&self, update: impl FnOnce(&mut MediaJobSnapshot)) {
        update(&mut self.inner.snapshot.lock().expect("media snapshot poisoned"));
    }

    fn execute(&self) -> Result<(u64, String), MediaError> {
        validate_source(&self.source)?;
        if self.destination.exists() {
            return Err(MediaError::DestinationConflict);
        }
        let inspection = self.inspect_for_job()?;
        let variant = inspection
            .variants
            .into_iter()
            .find(|variant| variant.id == self.variant_id)
            .ok_or(MediaError::UnknownVariant)?;
        let parent = self.destination.parent().ok_or(MediaError::InvalidOutput)?;
        fs::create_dir_all(parent)?;
        let work = parent.join(format!(".fetchpath-media-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&work)?;
        let result = self.download_to_work(&variant, &work);
        let result = match result {
            Ok(output) => self.verify_and_publish(&variant, &output),
            Err(error) => Err(error),
        };
        let _ = fs::remove_dir_all(&work);
        result
    }

    fn inspect_for_job(&self) -> Result<MediaInspection, MediaError> {
        let args = inspect_args(&self.source);
        let output = run_helper(
            &self.tools.yt_dlp,
            &args,
            &self.inner.cancel,
            &self.inner.active_pid,
            INSPECT_TIMEOUT,
        )?;
        if !output.status.success() {
            return Err(classify_helper_failure(&output.stderr));
        }
        parse_inspection(&output.stdout)
    }

    fn download_to_work(&self, variant: &MediaVariant, work: &Path) -> Result<PathBuf, MediaError> {
        let mut args = vec![
            "--ignore-config".into(),
            "--no-playlist".into(),
            "--no-warnings".into(),
            "--newline".into(),
            "--ffmpeg-location".into(),
            self.tools.ffmpeg_dir.display().to_string(),
            "-f".into(),
            variant.selector.clone(),
            "-P".into(),
            format!("home:{}", work.display()),
            "-P".into(),
            format!("temp:{}", work.display()),
            "-o".into(),
            "media.%(ext)s".into(),
        ];
        match variant.kind {
            MediaKind::Video => args.extend([
                "--merge-output-format".into(),
                "mp4".into(),
                "--remux-video".into(),
                "mp4".into(),
            ]),
            MediaKind::Audio => args.extend([
                "--extract-audio".into(),
                "--audio-format".into(),
                "mp3".into(),
                "--audio-quality".into(),
                "0".into(),
            ]),
        }
        args.push(self.source.clone());
        // What has arrived is the size of the work folder, measured while the
        // helper runs. The engine reads it as progress and stops an agent's
        // download at its size limit, as it does for files (FP-067).
        // Over a cap, the helper is stopped as a cancellation would stop it,
        // and the stop is reported as the cap.
        let done = AtomicBool::new(false);
        let output = thread::scope(|scope| {
            scope.spawn(|| {
                while !done.load(Ordering::Acquire) {
                    let arrived = folder_bytes(work);
                    self.update(|snapshot| snapshot.bytes_received = arrived);
                    if self.cap().is_some_and(|cap| arrived > cap) {
                        self.inner.over_cap.store(true, Ordering::Release);
                        self.inner.cancel.store(true, Ordering::Release);
                    }
                    thread::sleep(PROGRESS_EVERY);
                }
            });
            // Set however the helper call ends, a panic included, so the
            // watcher always stops and the scope can finish.
            struct Done<'a>(&'a AtomicBool);
            impl Drop for Done<'_> {
                fn drop(&mut self) {
                    self.0.store(true, Ordering::Release);
                }
            }
            let _done = Done(&done);
            run_helper(
                &self.tools.yt_dlp,
                &args,
                &self.inner.cancel,
                &self.inner.active_pid,
                DOWNLOAD_TIMEOUT,
            )
        });
        if self.inner.over_cap.load(Ordering::Acquire) {
            return Err(MediaError::SizeLimit);
        }
        let output = output?;
        if !output.status.success() {
            return Err(classify_helper_failure(&output.stderr));
        }
        let expected = match variant.kind {
            MediaKind::Video => "mp4",
            MediaKind::Audio => "mp3",
        };
        let outputs = fs::read_dir(work)?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.is_file())
            .filter(|path| {
                path.extension()
                    .is_some_and(|extension| extension.eq_ignore_ascii_case(expected))
            })
            .collect::<Vec<_>>();
        if outputs.len() != 1 {
            return Err(MediaError::InvalidOutput);
        }
        Ok(outputs[0].clone())
    }

    fn verify_and_publish(
        &self,
        variant: &MediaVariant,
        output: &Path,
    ) -> Result<(u64, String), MediaError> {
        if self.inner.cancel.load(Ordering::Acquire) {
            return Err(MediaError::Cancelled);
        }
        let args = vec![
            "-v".into(),
            "error".into(),
            "-show_entries".into(),
            "format=duration:stream=codec_type".into(),
            "-of".into(),
            "json".into(),
            output.display().to_string(),
        ];
        let probe = run_helper(
            &self.tools.ffprobe,
            &args,
            &self.inner.cancel,
            &self.inner.active_pid,
            Duration::from_secs(30),
        )?;
        if !probe.status.success() || !verified_probe(&probe.stdout, variant.kind) {
            return Err(MediaError::InvalidOutput);
        }
        if self.inner.cancel.load(Ordering::Acquire) {
            return Err(MediaError::Cancelled);
        }
        let bytes = fs::metadata(output)?.len();
        // Measured again here: a helper that wrote everything at once was
        // faster than the sampling (FP-067 review).
        if self.cap().is_some_and(|cap| bytes > cap) {
            self.update(|snapshot| snapshot.bytes_received = bytes);
            return Err(MediaError::SizeLimit);
        }
        let hash = sha256_file(output)?;
        match fs::hard_link(output, &self.destination) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                return Err(MediaError::DestinationConflict);
            }
            Err(error) => return Err(MediaError::Io(error)),
        }
        let _ = fs::remove_file(output);
        Ok((bytes, hash))
    }
}

/// Asks the helper for only the fields `parse_inspection` reads. The full
/// `--dump-single-json` record of an ordinary YouTube talk is over 800 KiB,
/// mostly automatic captions in 160 languages; it overran the output limit,
/// was cut off mid-JSON, and every such video failed as "verification_failed".
fn inspect_args(source: &str) -> Vec<String> {
    vec![
        "--ignore-config".into(),
        "--no-playlist".into(),
        "--no-warnings".into(),
        "--skip-download".into(),
        "--print".into(),
        "%(.{title,duration,formats})j".into(),
        source.into(),
    ]
}

fn validate_source(source: &str) -> Result<(), MediaError> {
    let parsed = Url::parse(source).map_err(|_| MediaError::InvalidSource)?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return Err(MediaError::InvalidSource);
    }
    Ok(())
}

#[derive(Deserialize)]
struct HelperInspection {
    #[serde(default)]
    title: String,
    duration: Option<f64>,
    #[serde(default)]
    formats: Vec<HelperFormat>,
}

#[derive(Deserialize)]
struct HelperFormat {
    format_id: String,
    #[serde(default)]
    vcodec: String,
    #[serde(default)]
    acodec: String,
    height: Option<u32>,
    fps: Option<f64>,
    tbr: Option<f64>,
}

fn parse_inspection(bytes: &[u8]) -> Result<MediaInspection, MediaError> {
    let parsed: HelperInspection =
        serde_json::from_slice(bytes).map_err(|_| MediaError::InvalidOutput)?;
    let mut video = parsed
        .formats
        .iter()
        .filter(|format| codec_present(&format.vcodec))
        .collect::<Vec<_>>();
    video.sort_by(|left, right| {
        right
            .height
            .cmp(&left.height)
            .then_with(|| {
                right
                    .fps
                    .partial_cmp(&left.fps)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| {
                right
                    .tbr
                    .partial_cmp(&left.tbr)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    });
    let mut variants = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for format in video {
        let key = (format.height, format.fps.map(|fps| fps.round() as u32));
        if !seen.insert(key) {
            continue;
        }
        let height = format.height;
        let fps = format.fps.map(|fps| fps.round() as u32);
        let quality = height
            .map(|height| format!("{height}p"))
            .unwrap_or_else(|| "Video".into());
        let label = fps
            .filter(|fps| *fps > 30)
            .map_or(quality.clone(), |fps| format!("{quality} · {fps} fps"));
        let selector = if codec_present(&format.acodec) {
            format.format_id.clone()
        } else {
            format!("{}+bestaudio/best", format.format_id)
        };
        variants.push(MediaVariant {
            id: format!("video-{}", format.format_id),
            label,
            kind: MediaKind::Video,
            extension: "mp4".into(),
            height,
            fps,
            selector,
        });
    }
    if parsed
        .formats
        .iter()
        .any(|format| codec_present(&format.acodec))
    {
        variants.push(MediaVariant {
            id: "audio-best".into(),
            label: "Audio only · best available".into(),
            kind: MediaKind::Audio,
            extension: "mp3".into(),
            height: None,
            fps: None,
            selector: "bestaudio/best".into(),
        });
    }
    if variants.is_empty() {
        return Err(MediaError::InvalidOutput);
    }
    let title = parsed.title.trim();
    Ok(MediaInspection {
        title: if title.is_empty() {
            "Media download".into()
        } else {
            title.chars().take(180).collect()
        },
        duration_seconds: parsed
            .duration
            .filter(|duration| duration.is_finite() && *duration >= 0.0),
        variants,
    })
}

fn codec_present(codec: &str) -> bool {
    !codec.is_empty() && codec != "none"
}

fn verified_probe(bytes: &[u8], kind: MediaKind) -> bool {
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(bytes) else {
        return false;
    };
    let duration = value
        .pointer("/format/duration")
        .and_then(|value| value.as_str())
        .and_then(|duration| duration.parse::<f64>().ok())
        .is_some_and(|duration| duration > 0.0);
    let streams = value.get("streams").and_then(|value| value.as_array());
    let has_audio = streams.is_some_and(|streams| {
        streams.iter().any(|stream| {
            stream.get("codec_type").and_then(|value| value.as_str()) == Some("audio")
        })
    });
    let has_video = streams.is_some_and(|streams| {
        streams.iter().any(|stream| {
            stream.get("codec_type").and_then(|value| value.as_str()) == Some("video")
        })
    });
    duration
        && match kind {
            MediaKind::Video => has_video && has_audio,
            MediaKind::Audio => has_audio,
        }
}

struct CapturedOutput {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

/// How often the bytes a helper has written are measured.
const PROGRESS_EVERY: Duration = Duration::from_millis(250);

/// The bytes in the files under `folder`, which only the helper writes.
fn folder_bytes(folder: &Path) -> u64 {
    let Ok(entries) = fs::read_dir(folder) else {
        return 0;
    };
    entries
        .filter_map(Result::ok)
        .map(|entry| match entry.file_type() {
            Ok(kind) if kind.is_dir() => folder_bytes(&entry.path()),
            // Read through the file itself: a directory listing's size for a
            // file still being written lags on Windows.
            Ok(kind) if kind.is_file() => fs::metadata(entry.path()).map_or(0, |data| data.len()),
            _ => 0,
        })
        .sum()
}

fn run_helper(
    executable: &Path,
    args: &[String],
    cancel: &AtomicBool,
    active_pid: &AtomicU32,
    timeout: Duration,
) -> Result<CapturedOutput, MediaError> {
    if !executable.is_file() {
        return Err(MediaError::HelperUnavailable);
    }
    let mut command = Command::new(executable);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW: the desktop app has no
        // console, so without the second flag every helper flashes one.
        command.creation_flags(0x0000_0200 | 0x0800_0000);
    }
    let mut child = command.spawn().map_err(|_| MediaError::HelperUnavailable)?;
    active_pid.store(child.id(), Ordering::Release);
    let stdout = drain_bounded(child.stdout.take().expect("piped stdout"));
    let stderr = drain_bounded(child.stderr.take().expect("piped stderr"));
    let started = Instant::now();
    let status = loop {
        if cancel.load(Ordering::Acquire) {
            terminate_tree(child.id());
            let _ = child.wait();
            active_pid.store(0, Ordering::Release);
            let _ = stdout.join();
            let _ = stderr.join();
            return Err(MediaError::Cancelled);
        }
        if started.elapsed() > timeout {
            terminate_tree(child.id());
            let _ = child.wait();
            active_pid.store(0, Ordering::Release);
            let _ = stdout.join();
            let _ = stderr.join();
            return Err(MediaError::TimedOut);
        }
        if let Some(status) = child.try_wait()? {
            break status;
        }
        thread::sleep(Duration::from_millis(40));
    };
    active_pid.store(0, Ordering::Release);
    Ok(CapturedOutput {
        status,
        stdout: stdout.join().unwrap_or_default(),
        stderr: stderr.join().unwrap_or_default(),
    })
}

fn drain_bounded(mut reader: impl Read + Send + 'static) -> JoinHandle<Vec<u8>> {
    thread::spawn(move || {
        let mut kept = Vec::new();
        let mut buffer = [0_u8; 8192];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(read) => {
                    let remaining = OUTPUT_LIMIT.saturating_sub(kept.len());
                    kept.extend_from_slice(&buffer[..read.min(remaining)]);
                }
            }
        }
        kept
    })
}

fn classify_helper_failure(stderr: &[u8]) -> MediaError {
    let message = String::from_utf8_lossy(stderr).to_ascii_lowercase();
    if [
        "http error 401",
        "http error 403",
        "forbidden",
        "expired",
        "signature extraction failed",
    ]
    .iter()
    .any(|needle| message.contains(needle))
    {
        MediaError::SourceExpired
    } else {
        MediaError::HelperCrash
    }
}

fn sha256_file(path: &Path) -> Result<String, MediaError> {
    let mut file = File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 128 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn first_line(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .lines()
        .next()
        .unwrap_or_default()
        .trim()
        .chars()
        .take(160)
        .collect()
}

#[cfg(windows)]
fn executable(name: &str) -> String {
    format!("{name}.exe")
}
#[cfg(not(windows))]
fn executable(name: &str) -> String {
    name.into()
}

#[cfg(windows)]
fn terminate_tree(pid: u32) {
    use std::os::windows::process::CommandExt;
    let _ = Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .creation_flags(0x0800_0000)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

#[cfg(not(windows))]
fn terminate_tree(pid: u32) {
    let _ = Command::new("kill")
        .args(["-TERM", &pid.to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

#[cfg(test)]
mod tests {
    use super::*;

    const INSPECTION: &str = r#"{
      "title":"Fixture title","duration":12.0,
      "formats":[
        {"format_id":"a","ext":"m4a","vcodec":"none","acodec":"aac","tbr":128},
        {"format_id":"v360-low","ext":"mp4","vcodec":"h264","acodec":"none","height":360,"fps":30,"tbr":500},
        {"format_id":"v360","ext":"mp4","vcodec":"h264","acodec":"none","height":360,"fps":30,"tbr":900},
        {"format_id":"v720","ext":"mp4","vcodec":"h264","acodec":"none","height":720,"fps":60,"tbr":1800}
      ]
    }"#;

    #[test]
    fn quality_choices_are_engine_confirmed_deduplicated_and_sorted() {
        let inspection = parse_inspection(INSPECTION.as_bytes()).unwrap();
        assert_eq!(inspection.title, "Fixture title");
        assert_eq!(inspection.variants.len(), 3);
        assert_eq!(inspection.variants[0].id, "video-v720");
        assert_eq!(inspection.variants[0].label, "720p · 60 fps");
        assert_eq!(inspection.variants[1].id, "video-v360");
        assert_eq!(inspection.variants[2].id, "audio-best");
    }

    #[test]
    fn probe_requires_duration_and_expected_streams() {
        let muxed = br#"{"streams":[{"codec_type":"video"},{"codec_type":"audio"}],"format":{"duration":"12.0"}}"#;
        let audio = br#"{"streams":[{"codec_type":"audio"}],"format":{"duration":"12.0"}}"#;
        assert!(verified_probe(muxed, MediaKind::Video));
        assert!(verified_probe(audio, MediaKind::Audio));
        assert!(!verified_probe(audio, MediaKind::Video));
    }

    #[test]
    fn helper_failures_are_redacted_and_expiry_is_actionable() {
        let expired =
            classify_helper_failure(b"HTTP Error 403: forbidden https://secret.example/?token=abc");
        assert!(matches!(expired, MediaError::SourceExpired));
        assert!(!expired.to_string().contains("secret.example"));
        assert!(matches!(
            classify_helper_failure(b"access violation"),
            MediaError::HelperCrash
        ));
    }

    #[test]
    fn only_http_sources_are_accepted() {
        assert!(validate_source("https://example.test/watch?v=1").is_ok());
        assert!(validate_source("file:///private/video.mp4").is_err());
        assert!(validate_source("javascript:alert(1)").is_err());
    }

    #[test]
    fn queued_media_cancels_without_starting_a_helper() {
        let job = MediaJob::create(
            "https://example.test/media".into(),
            "video-example".into(),
            PathBuf::from("unused.mp4"),
            MediaTools {
                yt_dlp: PathBuf::from("missing-helper"),
                ffmpeg_dir: PathBuf::from("missing-ffmpeg"),
                ffprobe: PathBuf::from("missing-ffprobe"),
            },
        );
        job.cancel();
        assert_eq!(job.snapshot().state, MediaJobState::Cancelled);
        assert_eq!(job.start(), Err("invalid_state"));
    }

    #[cfg(windows)]
    fn fixture_tools(helper: &str) -> MediaTools {
        let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        MediaTools::new(
            fixtures.join(helper),
            &fixtures,
            fixtures.join("fake-ffprobe.cmd"),
        )
        .unwrap()
    }

    #[cfg(windows)]
    #[test]
    fn helper_crash_and_expired_session_are_distinct_and_redacted() {
        let crash = fixture_tools("crash-helper.cmd")
            .inspect("https://example.test/media")
            .unwrap_err();
        let expired = fixture_tools("expired-helper.cmd")
            .inspect("https://example.test/media?token=secret")
            .unwrap_err();
        assert!(matches!(crash, MediaError::HelperCrash));
        assert!(matches!(expired, MediaError::SourceExpired));
        assert!(!expired.to_string().contains("secret"));
    }

    #[cfg(windows)]
    #[test]
    fn cancellation_terminates_helper_and_never_publishes() {
        let temporary = tempfile::tempdir().unwrap();
        let destination = temporary.path().join("cancelled.mp4");
        let job = MediaJob::create(
            "https://example.test/media".into(),
            "video-fixture".into(),
            destination.clone(),
            fixture_tools("slow-helper.cmd"),
        );
        job.start().unwrap();
        thread::sleep(Duration::from_millis(150));
        job.cancel();
        job.join();
        assert_eq!(job.snapshot().state, MediaJobState::Cancelled);
        assert!(!destination.exists());
        assert!(fs::read_dir(temporary.path()).unwrap().next().is_none());
    }

    /// Bytes are reported while the helper writes, so the engine can stop an
    /// agent's download at its size limit (FP-067), and a stop part way
    /// publishes nothing.
    #[cfg(windows)]
    #[test]
    fn bytes_are_reported_while_the_helper_writes() {
        let temporary = tempfile::tempdir().unwrap();
        let destination = temporary.path().join("growing.mp4");
        let job = MediaJob::create(
            "https://example.test/media".into(),
            "video-18".into(),
            destination.clone(),
            fixture_tools("growing-helper.cmd"),
        );
        job.start().unwrap();
        let deadline = Instant::now() + Duration::from_secs(30);
        let seen = loop {
            let snapshot = job.snapshot();
            assert_eq!(snapshot.state, MediaJobState::Running, "{snapshot:?}");
            if snapshot.bytes_received >= 256 * 1024 {
                break snapshot.bytes_received;
            }
            assert!(Instant::now() < deadline, "no bytes reported while running");
            thread::sleep(Duration::from_millis(50));
        };
        assert!(seen < 60 * 64 * 1024, "reported before the helper finished");
        job.cancel();
        job.join();
        assert_eq!(job.snapshot().state, MediaJobState::Cancelled);
        assert!(!destination.exists());
        assert!(fs::read_dir(temporary.path()).unwrap().next().is_none());
    }

    /// A helper that writes its whole output at once is faster than any
    /// sampling (FP-067 review): with a byte cap set, the output is measured
    /// again before publication and a file over the cap is never published.
    /// Without a cap the same helper's file is published, so the fixture
    /// really would get past a sampled limit.
    #[cfg(windows)]
    #[test]
    fn a_burst_past_the_byte_cap_is_never_published() {
        let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        let tools = || {
            MediaTools::new(
                fixtures.join("burst-helper.cmd"),
                &fixtures,
                fixtures.join("fake-ffprobe-video.cmd"),
            )
            .unwrap()
        };
        let temporary = tempfile::tempdir().unwrap();

        let open = temporary.path().join("uncapped.mp4");
        let job = MediaJob::create(
            "https://example.test/v".into(),
            "video-18".into(),
            open.clone(),
            tools(),
        );
        job.start().unwrap();
        job.join();
        assert_eq!(
            job.snapshot().state,
            MediaJobState::Completed,
            "{:?}",
            job.snapshot()
        );
        assert_eq!(fs::metadata(&open).unwrap().len(), 2 * 1024 * 1024);

        let capped = temporary.path().join("capped.mp4");
        let job = MediaJob::create(
            "https://example.test/v".into(),
            "video-18".into(),
            capped.clone(),
            tools(),
        );
        job.limit_bytes(256 * 1024);
        job.start().unwrap();
        job.join();
        let snapshot = job.snapshot();
        assert_eq!(snapshot.state, MediaJobState::Failed, "{snapshot:?}");
        assert!(
            snapshot
                .error
                .as_deref()
                .unwrap_or("")
                .starts_with("size_limit"),
            "{snapshot:?}"
        );
        assert!(!capped.exists(), "a capped download was published");
        // Only the uncapped file is left; the capped job's work folder is gone.
        let left: Vec<_> = fs::read_dir(temporary.path())
            .unwrap()
            .filter_map(Result::ok)
            .collect();
        assert_eq!(left.len(), 1);
    }

    /// The real regression: a YouTube talk whose full metadata is over 800 KiB
    /// because of automatic captions. Before `inspect_args` it was cut off at
    /// the output limit and failed as "verification_failed". Needs network and
    /// real helpers, so it runs only when pointed at them:
    /// `FETCHPATH_TEST_MEDIA_TOOLS=<folder with yt-dlp.exe and bin\ffmpeg.exe>`
    /// `cargo test -p fetchpath-media -- --ignored real_captioned_talk`
    #[test]
    #[ignore = "needs network and real yt-dlp/ffmpeg"]
    fn real_captioned_talk_inspects_and_downloads() {
        let Some(root) = std::env::var_os("FETCHPATH_TEST_MEDIA_TOOLS") else {
            panic!("set FETCHPATH_TEST_MEDIA_TOOLS");
        };
        let tools = MediaTools::discover_in(Path::new(&root)).expect("tools");
        let source = "https://www.youtube.com/watch?v=arj7oStGLkU";
        let inspection = tools.inspect(source).expect("inspection");
        assert!(inspection.title.contains("Procrastinator"));
        assert!(
            inspection
                .variants
                .iter()
                .any(|v| v.kind == MediaKind::Video)
        );
        let audio = inspection
            .variants
            .iter()
            .find(|v| v.kind == MediaKind::Audio)
            .expect("an audio variant");

        let temporary =
            std::env::temp_dir().join(format!("fetchpath-media-real-{}", std::process::id()));
        let _ = fs::remove_dir_all(&temporary);
        fs::create_dir_all(&temporary).unwrap();
        let destination = temporary.join("talk.mp3");
        let job = MediaJob::create(source.into(), audio.id.clone(), destination.clone(), tools);
        job.start().unwrap();
        job.join();
        let done = job.snapshot();
        assert_eq!(done.state, MediaJobState::Completed, "{:?}", done.error);
        assert!(fs::metadata(&destination).unwrap().len() > 1_000_000);
        let _ = fs::remove_dir_all(&temporary);
    }
}
