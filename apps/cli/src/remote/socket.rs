//! One signed-in browser socket (FP-104).
//!
//! Frames are JSON text. The browser sends `{id, call, args}` and gets
//! `{id, ok}` or `{id, error: {code, message}}`; the engine's changes reach
//! it as `{notice: "queue"}`. The calls are the desktop's commands, fixed
//! here, and each runs as the session's `device` principal, so the engine
//! (not this file) refuses what a device may not do. A frame that is not a
//! known call closes the socket.
//!
//! The WebSocket protocol is driven by hand over the upgraded connection:
//! the stream type of `tokio-tungstenite` needs a futures crate this build
//! does not carry, while the plain `tungstenite` state machine only needs
//! bytes moved in and out.

use super::{Shared, internal};
use fetchpath_protocol::client::EventStream;
use fetchpath_protocol::command::{Command, JobFilter};
use fetchpath_protocol::error::{ErrorCode, ErrorScope, ProtocolError};
use fetchpath_protocol::message::{CommandResult, ControlOutcome};
use fetchpath_protocol::model::JobState;
use fetchpath_protocol::principal::{DeviceId, Principal};
use fetchpath_protocol::view;
use fetchpath_protocol::{ClientId, JobId, JobSnapshot, Timestamp};
use hyper::upgrade::Upgraded;
use hyper_util::rt::TokioIo;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::pin::Pin;
use std::sync::Arc;
use std::task::Poll;
use std::time::{Duration, Instant};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio_tungstenite::tungstenite::protocol::{Role, WebSocketConfig};
use tokio_tungstenite::tungstenite::{self, Message, WebSocket};
use tokio_util::sync::CancellationToken;

const MAX_MESSAGE: usize = 1 << 20;
/// Queue changes are told to the browser at most this often.
const COALESCE: Duration = Duration::from_millis(150);
/// A browser that takes this long to accept a frame is gone.
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_ID_LENGTH: usize = 64;

const QUEUE_NOTICE: &str = r#"{"notice":"queue"}"#;
const ENGINE_NOTICE: &str = r#"{"notice":"engine","state":{"connected":true}}"#;

/// Every call the browser may make.
const CALLS: [&str; 13] = [
    "listDownloads",
    "queueStats",
    "downloadDetails",
    "pauseDownload",
    "resumeDownload",
    "cancelDownload",
    "startNow",
    "retryDownload",
    "removeDownload",
    "revealDownload",
    "startBatch",
    "folderChoices",
    "engineConnection",
];

/// Bytes in and out of the WebSocket state machine.
#[derive(Default)]
struct Wire {
    input: VecDeque<u8>,
    output: Vec<u8>,
}

impl Read for Wire {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if self.input.is_empty() {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        let count = buffer.len().min(self.input.len());
        for (slot, byte) in buffer.iter_mut().zip(self.input.drain(..count)) {
            *slot = byte;
        }
        Ok(count)
    }
}

impl Write for Wire {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.output.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

enum Next {
    Text(String),
    /// Nothing complete is waiting.
    Idle,
    /// Closed, or not something this socket accepts.
    Done,
}

struct Peer {
    io: TokioIo<Upgraded>,
    ws: WebSocket<Wire>,
}

impl Peer {
    fn new(io: TokioIo<Upgraded>) -> Self {
        let config = WebSocketConfig::default()
            .max_message_size(Some(MAX_MESSAGE))
            .max_frame_size(Some(MAX_MESSAGE));
        Self {
            io,
            ws: WebSocket::from_raw_socket(Wire::default(), Role::Server, Some(config)),
        }
    }

    async fn read_some(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        std::future::poll_fn(|context| {
            let mut read = ReadBuf::new(&mut *buffer);
            match Pin::new(&mut self.io).poll_read(context, &mut read) {
                Poll::Ready(Ok(())) => Poll::Ready(Ok(read.filled().len())),
                Poll::Ready(Err(error)) => Poll::Ready(Err(error)),
                Poll::Pending => Poll::Pending,
            }
        })
        .await
    }

    async fn write_all(&mut self, mut bytes: &[u8]) -> io::Result<()> {
        while !bytes.is_empty() {
            let written =
                std::future::poll_fn(|context| Pin::new(&mut self.io).poll_write(context, bytes))
                    .await?;
            if written == 0 {
                return Err(io::ErrorKind::WriteZero.into());
            }
            bytes = &bytes[written..];
        }
        std::future::poll_fn(|context| Pin::new(&mut self.io).poll_flush(context)).await
    }

    /// Moves what the state machine has to say onto the connection.
    async fn drain(&mut self) -> io::Result<()> {
        let _ = self.ws.flush();
        let output = std::mem::take(&mut self.ws.get_mut().output);
        if output.is_empty() {
            return Ok(());
        }
        tokio::time::timeout(WRITE_TIMEOUT, self.write_all(&output))
            .await
            .map_err(|_| io::Error::from(io::ErrorKind::TimedOut))?
    }

    async fn send(&mut self, text: impl Into<String>) -> io::Result<()> {
        self.ws
            .send(Message::text(text.into()))
            .map_err(io::Error::other)?;
        self.drain().await
    }

    fn feed(&mut self, bytes: &[u8]) {
        self.ws.get_mut().input.extend(bytes);
    }

    fn next(&mut self) -> Next {
        loop {
            match self.ws.read() {
                Ok(Message::Text(text)) => return Next::Text(text.as_str().to_owned()),
                Ok(Message::Ping(_) | Message::Pong(_)) => {}
                Ok(_) => return Next::Done,
                Err(tungstenite::Error::Io(error)) if error.kind() == io::ErrorKind::WouldBlock => {
                    return Next::Idle;
                }
                Err(_) => return Next::Done,
            }
        }
    }

    async fn goodbye(&mut self) {
        let _ = self.ws.close(None);
        let _ = tokio::time::timeout(Duration::from_secs(1), self.drain()).await;
    }
}

/// What a socket needs to run commands as its device.
struct Ctx<'a> {
    shared: &'a Arc<Shared>,
    device: &'a DeviceId,
    token: &'a CancellationToken,
    client: ClientId,
}

impl Ctx<'_> {
    async fn run(&self, command: Command) -> Result<CommandResult, ProtocolError> {
        let engine = Arc::clone(&self.shared.engine);
        let principal = Principal::Device(self.device.clone());
        let envelope = self.shared.envelope(&self.client, command);
        tokio::task::spawn_blocking(move || engine.execute_as(&principal, &envelope))
            .await
            .map_err(internal)?
    }

    async fn job(&self, id: &str) -> Result<JobSnapshot, ProtocolError> {
        let job_id = job_id(id)?;
        match self.run(Command::GetJob { job_id }).await? {
            CommandResult::Job { job } | CommandResult::Control { job, .. } => Ok(job),
            other => Err(unexpected(&other)),
        }
    }

    async fn job_view(&self, command: Command) -> Result<Value, ProtocolError> {
        match self.run(command).await? {
            CommandResult::Job { job } | CommandResult::Control { job, .. } => {
                Ok(json(&view::job(&job, Timestamp::now())))
            }
            other => Err(unexpected(&other)),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    id: Id,
    call: String,
    #[serde(default)]
    args: Value,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(untagged)]
enum Id {
    Number(u64),
    Text(String),
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct JobArgs {
    job_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RetryArgs {
    job_id: String,
    #[serde(default)]
    url: Option<Value>,
    #[serde(default)]
    destination: Option<Value>,
    #[serde(default)]
    checksum: Option<Value>,
}

#[derive(Deserialize)]
struct BatchArgs {
    drafts: Vec<view::JobDraft>,
}

/// The desktop's answer to a cancel, in the same shape.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CancelResponse {
    outcome: &'static str,
    job: view::JobView,
}

fn json(value: &impl Serialize) -> Value {
    serde_json::to_value(value).expect("view types serialize")
}

fn args<T: serde::de::DeserializeOwned>(value: Value) -> Result<T, ProtocolError> {
    serde_json::from_value(value).map_err(ProtocolError::malformed)
}

fn job_id(value: &str) -> Result<JobId, ProtocolError> {
    JobId::try_from(value).map_err(|_| {
        ProtocolError::new(
            ErrorCode::UNKNOWN_JOB,
            ErrorScope::Command,
            "This download is no longer available.",
        )
    })
}

fn unexpected(result: &CommandResult) -> ProtocolError {
    internal(format!(
        "The engine answered with an unexpected result ({}).",
        serde_json::to_value(result)
            .ok()
            .and_then(|value| value.get("type").and_then(Value::as_str).map(str::to_owned))
            .unwrap_or_default()
    ))
}

fn refused(message: impl Into<String>) -> ProtocolError {
    ProtocolError::new(
        ErrorCode::try_from("input.invalid_request".to_owned()).expect("a valid built-in code"),
        ErrorScope::Command,
        message,
    )
}

/// The reply to one known call.
async fn call(ctx: &Ctx<'_>, name: &str, arguments: Value) -> Result<Value, ProtocolError> {
    match name {
        "listDownloads" => match ctx
            .run(Command::ListJobs {
                filter: JobFilter::All,
            })
            .await?
        {
            CommandResult::Jobs { jobs } => Ok(json(&view::jobs(&jobs))),
            other => Err(unexpected(&other)),
        },
        "queueStats" => match ctx.run(Command::QueueStats).await? {
            CommandResult::QueueStats { stats } => Ok(json(&view::QueueStats::from(&stats))),
            other => Err(unexpected(&other)),
        },
        "downloadDetails" => {
            let job_id = job_id(&args::<JobArgs>(arguments)?.job_id)?;
            match ctx.run(Command::JobDetails { job_id }).await? {
                CommandResult::Details { details } => Ok(json(&view::JobDetails::from(&details))),
                other => Err(unexpected(&other)),
            }
        }
        "pauseDownload" => {
            let job_id = job_id(&args::<JobArgs>(arguments)?.job_id)?;
            ctx.job_view(Command::Pause { job_id }).await
        }
        "resumeDownload" => {
            let job_id = job_id(&args::<JobArgs>(arguments)?.job_id)?;
            ctx.job_view(Command::Resume { job_id }).await
        }
        "startNow" => {
            let job_id = job_id(&args::<JobArgs>(arguments)?.job_id)?;
            ctx.job_view(Command::Start { job_id }).await
        }
        "cancelDownload" => {
            let job_id = job_id(&args::<JobArgs>(arguments)?.job_id)?;
            match ctx
                .run(Command::Cancel {
                    job_id,
                    retain_partial: false,
                })
                .await?
            {
                CommandResult::Control { outcome, job } => Ok(json(&CancelResponse {
                    outcome: match outcome {
                        ControlOutcome::Accepted => "accepted",
                        ControlOutcome::TooLateToCancel | ControlOutcome::TooLate => "too_late",
                        ControlOutcome::AlreadyTerminal => "already_terminal",
                        _ => "no_op",
                    },
                    job: view::job(&job, Timestamp::now()),
                })),
                other => Err(unexpected(&other)),
            }
        }
        "retryDownload" => {
            let retry: RetryArgs = args(arguments)?;
            // A changed link, destination or checksum is the person's to
            // decide at the computer, not from a browser.
            if [&retry.url, &retry.destination, &retry.checksum]
                .iter()
                .any(|field| field.is_some())
            {
                return Err(ProtocolError::new(
                    ErrorCode::try_from("policy.not_permitted".to_owned())
                        .expect("a valid built-in code"),
                    ErrorScope::Command,
                    "Do this in Fetchpath on this PC",
                ));
            }
            let job_id = job_id(&retry.job_id)?;
            ctx.job_view(Command::Retry {
                job_id,
                expected_sha256: None,
            })
            .await
        }
        "removeDownload" => {
            let job_id = job_id(&args::<JobArgs>(arguments)?.job_id)?;
            match ctx.run(Command::RemoveJob { job_id }).await? {
                CommandResult::Removed { .. } => Ok(Value::Null),
                other => Err(unexpected(&other)),
            }
        }
        "revealDownload" => {
            let job = ctx.job(&args::<JobArgs>(arguments)?.job_id).await?;
            tokio::task::spawn_blocking(move || reveal(&job))
                .await
                .map_err(internal)?
                .map_err(refused)?;
            Ok(Value::Null)
        }
        "startBatch" => {
            let batch: BatchArgs = args(arguments)?;
            let requests = batch
                .drafts
                .iter()
                .map(view::JobDraft::request)
                .collect::<Result<Vec<_>, _>>()
                .map_err(refused)?;
            match ctx.run(Command::CreateJobs { requests }).await? {
                CommandResult::Jobs { jobs } => Ok(json(&view::jobs(&jobs))),
                other => Err(unexpected(&other)),
            }
        }
        "folderChoices" => match ctx.run(Command::ListFolderChoices).await? {
            CommandResult::FolderChoices { choices } => Ok(json(&choices)),
            other => Err(unexpected(&other)),
        },
        "engineConnection" => Ok(json!({ "connected": true })),
        other => Err(ProtocolError::unknown_command(other)),
    }
}

/// Opens Explorer with a finished download's file selected, as the desktop
/// does. Only a path the engine recorded for the job is ever shown.
fn reveal(job: &JobSnapshot) -> Result<(), String> {
    if job.state != JobState::Completed {
        return Err("This download has not finished yet.".into());
    }
    let destination = job
        .destination
        .as_ref()
        .ok_or("This download has no saved file yet.")?;
    let path = std::path::PathBuf::from(destination);
    if !path.exists() {
        return Err(format!("{destination} is no longer on disk."));
    }
    // Explorer parses its own command line: `/select,` and the quoted path
    // must arrive as written, so the arguments are raw. A quote in the path
    // would end the quoting early, so one is refused.
    if destination.contains('"') {
        return Err("That destination cannot be shown in File Explorer.".into());
    }
    std::os::windows::process::CommandExt::raw_arg(
        &mut std::process::Command::new("explorer.exe"),
        format!("/select,\"{}\"", path.display()),
    )
    .stdin(std::process::Stdio::null())
    .stdout(std::process::Stdio::null())
    .stderr(std::process::Stdio::null())
    .spawn()
    .map(|_| ())
    .map_err(|error| format!("Could not open the folder: {error}"))
}

fn frame(id: &Id, outcome: Result<Value, ProtocolError>) -> String {
    match outcome {
        Ok(ok) => json!({ "id": id, "ok": ok }),
        Err(error) => json!({
            "id": id,
            "error": { "code": error.code.as_str(), "message": error.message },
        }),
    }
    .to_string()
}

/// What the pump tells the socket.
enum Pumped {
    Queue,
    Failed,
}

/// Follows the queue stream on its own thread and tells the socket of
/// changes, as the desktop's engine link does. Ends when the socket does.
fn pump(mut events: Box<dyn EventStream>, sender: UnboundedSender<Pumped>) {
    std::thread::spawn(move || {
        let mut pending = false;
        let mut last = Instant::now();
        loop {
            if sender.is_closed() {
                return;
            }
            let wait = if pending {
                COALESCE.saturating_sub(last.elapsed())
            } else {
                Duration::from_millis(500)
            };
            match events.next_item(wait) {
                Ok(Some(_)) => pending = true,
                Ok(None) => {}
                Err(_) => {
                    let _ = sender.send(Pumped::Failed);
                    return;
                }
            }
            if pending && last.elapsed() >= COALESCE {
                if sender.send(Pumped::Queue).is_err() {
                    return;
                }
                pending = false;
                last = Instant::now();
            }
        }
    });
}

async fn subscribe(ctx: &Ctx<'_>) -> Result<Box<dyn EventStream>, ProtocolError> {
    let shared = Arc::clone(ctx.shared);
    let principal = Principal::Device(ctx.device.clone());
    let client = ctx.client.clone();
    tokio::task::spawn_blocking(move || {
        let status = match shared
            .engine
            .execute_as(&principal, &shared.envelope(&client, Command::EngineStatus))?
        {
            CommandResult::EngineStatus { status } => status,
            other => return Err(unexpected(&other)),
        };
        let envelope = shared.envelope(
            &client,
            Command::SubscribeQueue {
                after_cursor: status.queue_cursor,
            },
        );
        Ok(shared.engine.subscribe_as(&principal, &envelope)?.events)
    })
    .await
    .map_err(internal)?
}

enum Event {
    Revoked,
    Bytes(io::Result<usize>),
    Pump(Option<Pumped>),
}

/// Runs a socket until the browser leaves, a frame is refused, the session
/// ends or the listener stops.
pub(super) async fn run(
    shared: &Arc<Shared>,
    device: &DeviceId,
    token: &CancellationToken,
    io: TokioIo<Upgraded>,
) {
    let ctx = Ctx {
        shared,
        device,
        token,
        client: ClientId::random(),
    };
    let mut peer = Peer::new(io);
    let Ok(events) = subscribe(&ctx).await else {
        return;
    };
    let (sender, mut pumped) = mpsc::unbounded_channel();
    pump(events, sender);
    if peer.send(ENGINE_NOTICE).await.is_ok() && peer.send(QUEUE_NOTICE).await.is_ok() {
        converse(&ctx, &mut peer, &mut pumped).await;
    }
    peer.goodbye().await;
}

async fn converse(ctx: &Ctx<'_>, peer: &mut Peer, pumped: &mut UnboundedReceiver<Pumped>) {
    let mut buffer = [0_u8; 8192];
    loop {
        loop {
            match peer.next() {
                Next::Text(text) => {
                    if !dispatch(ctx, peer, &text).await {
                        return;
                    }
                }
                Next::Idle => break,
                Next::Done => return,
            }
        }
        // Pongs and the like the state machine queued while reading.
        if peer.drain().await.is_err() {
            return;
        }
        let event = tokio::select! {
            () = ctx.token.cancelled() => Event::Revoked,
            read = peer.read_some(&mut buffer) => Event::Bytes(read),
            pumped = pumped.recv() => Event::Pump(pumped),
        };
        match event {
            Event::Revoked
            | Event::Bytes(Ok(0) | Err(_))
            | Event::Pump(None | Some(Pumped::Failed)) => {
                return;
            }
            Event::Bytes(Ok(count)) => peer.feed(&buffer[..count]),
            Event::Pump(Some(Pumped::Queue)) => {
                if peer.send(QUEUE_NOTICE).await.is_err() {
                    return;
                }
            }
        }
    }
}

/// Answers one frame. False closes the socket.
async fn dispatch(ctx: &Ctx<'_>, peer: &mut Peer, text: &str) -> bool {
    let Ok(request) = serde_json::from_str::<Request>(text) else {
        return false;
    };
    if matches!(&request.id, Id::Text(id) if id.len() > MAX_ID_LENGTH)
        || !CALLS.contains(&request.call.as_str())
    {
        return false;
    }
    let outcome = tokio::select! {
        () = ctx.token.cancelled() => return false,
        outcome = call(ctx, &request.call, request.args) => outcome,
    };
    peer.send(frame(&request.id, outcome)).await.is_ok()
}
