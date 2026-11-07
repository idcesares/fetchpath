//! The engine's side: stay enrolled with a relay and hand each visitor the
//! relay forwards to a handler.
//!
//! One control connection announces the ids of the engine's live tickets and
//! pings; a small pool of idle work connections waits for visitors. When the
//! relay uses one, the pool is topped up. Each work connection serves one
//! request: the handler closes it after the response.

use crate::server::Conn;
use crate::wire::{self, EnrollmentKey, MAX_IDS, Role, TAG_ANNOUNCE, TAG_PING};
use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::sync::{Notify, watch};
use tokio::task::JoinSet;

pub type Connector =
    Arc<dyn Fn() -> Pin<Box<dyn Future<Output = io::Result<Conn>> + Send>> + Send + Sync>;
pub type Handler = Arc<dyn Fn(Conn) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>;

#[derive(Clone)]
pub struct Settings {
    pub key: EnrollmentKey,
    pub name: String,
    /// Idle work connections kept ready.
    pub pool: usize,
    /// Work connections open at once, idle or serving.
    pub max_open: usize,
    pub ping: Duration,
    /// An idle work connection is replaced after this long, before a NAT
    /// or the relay would drop it silently.
    pub recycle: Duration,
}

impl Settings {
    pub fn new(key: EnrollmentKey, name: impl Into<String>) -> Self {
        Self {
            key,
            name: name.into(),
            pool: 4,
            max_open: 32,
            ping: Duration::from_secs(20),
            recycle: Duration::from_secs(60),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Status {
    Connecting,
    Enrolled,
    /// The relay closed the handshake: wrong key or not an enrollable engine.
    Refused,
    Unreachable,
}

/// Replays one byte already read, then the stream.
struct Prefixed<S> {
    first: Option<u8>,
    inner: S,
}

impl<S: AsyncRead + Unpin> AsyncRead for Prefixed<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if let Some(byte) = self.first.take() {
            buffer.put_slice(&[byte]);
            return Poll::Ready(Ok(()));
        }
        Pin::new(&mut self.inner).poll_read(context, buffer)
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for Prefixed<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(context, bytes)
    }

    fn poll_flush(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(context)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(context)
    }
}

/// Stays enrolled until the future is dropped, reconnecting with a growing
/// pause. `ids` is the full set of live ticket ids; every change is announced.
pub async fn run(
    settings: Settings,
    connect: Connector,
    mut ids: watch::Receiver<Vec<String>>,
    handler: Handler,
    status: watch::Sender<Status>,
) {
    let mut pause = Duration::from_secs(1);
    loop {
        let _ = status.send(Status::Connecting);
        let started = Instant::now();
        let ended = session(&settings, &connect, &mut ids, &handler, &status).await;
        let _ = status.send(match ended {
            Err(error) if error.kind() == io::ErrorKind::PermissionDenied => Status::Refused,
            _ => Status::Unreachable,
        });
        if started.elapsed() > Duration::from_secs(10) {
            pause = Duration::from_secs(1);
        }
        tokio::time::sleep(pause).await;
        pause = (pause * 2).min(Duration::from_secs(30));
    }
}

async fn connect_as(connect: &Connector, settings: &Settings, role: Role) -> io::Result<Conn> {
    let mut stream = connect().await?;
    wire::enroll(&mut stream, &settings.key, role, &settings.name).await?;
    Ok(stream)
}

async fn announce<W: AsyncWrite + Unpin>(writer: &mut W, ids: &[String]) -> io::Result<()> {
    let ids: Vec<&String> = ids
        .iter()
        .filter(|id| wire::valid_id(id))
        .take(MAX_IDS)
        .collect();
    let mut frame = vec![TAG_ANNOUNCE];
    frame.extend_from_slice(&(ids.len() as u16).to_be_bytes());
    for id in ids {
        frame.push(id.len() as u8);
        frame.extend_from_slice(id.as_bytes());
    }
    writer.write_all(&frame).await?;
    writer.flush().await
}

async fn session(
    settings: &Settings,
    connect: &Connector,
    ids: &mut watch::Receiver<Vec<String>>,
    handler: &Handler,
    status: &watch::Sender<Status>,
) -> io::Result<()> {
    let control = connect_as(connect, settings, Role::Control).await?;
    let _ = status.send(Status::Enrolled);
    let (mut reader, mut writer) = tokio::io::split(control);
    let current = ids.borrow_and_update().clone();
    announce(&mut writer, &current).await?;

    let idle = Arc::new(AtomicUsize::new(0));
    let open = Arc::new(AtomicUsize::new(0));
    let wake = Arc::new(Notify::new());
    let mut tasks: JoinSet<()> = JoinSet::new();
    let mut ping = tokio::time::interval(settings.ping);
    loop {
        while idle.load(Ordering::SeqCst) < settings.pool
            && open.load(Ordering::SeqCst) < settings.max_open
        {
            idle.fetch_add(1, Ordering::SeqCst);
            open.fetch_add(1, Ordering::SeqCst);
            tasks.spawn(work(
                settings.clone(),
                Arc::clone(connect),
                Arc::clone(handler),
                Arc::clone(&idle),
                Arc::clone(&open),
                Arc::clone(&wake),
            ));
        }
        let mut byte = [0u8; 1];
        tokio::select! {
            () = wake.notified() => {}
            _ = tasks.join_next(), if !tasks.is_empty() => {}
            _ = ping.tick() => {
                writer.write_all(&[TAG_PING]).await?;
                writer.flush().await?;
            }
            changed = ids.changed() => {
                changed.map_err(|_| io::Error::other("ids closed"))?;
                let current = ids.borrow_and_update().clone();
                announce(&mut writer, &current).await?;
            }
            read = reader.read(&mut byte) => {
                // The relay never speaks on the control connection.
                if matches!(read, Ok(0) | Err(_)) {
                    return Err(io::Error::new(io::ErrorKind::ConnectionAborted, "relay closed"));
                }
            }
        }
    }
}

/// Decrements the open count when dropped.
struct Open(Arc<AtomicUsize>);

impl Drop for Open {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

async fn work(
    settings: Settings,
    connect: Connector,
    handler: Handler,
    idle: Arc<AtomicUsize>,
    open: Arc<AtomicUsize>,
    wake: Arc<Notify>,
) {
    let open = Open(open);
    let waited = async {
        let mut stream = connect_as(&connect, &settings, Role::Work).await?;
        let mut first = [0u8; 1];
        match tokio::time::timeout(settings.recycle, stream.read_exact(&mut first)).await {
            Ok(read) => {
                read?;
            }
            Err(_) => return Ok(None),
        }
        Ok::<_, io::Error>(Some((stream, first[0])))
    }
    .await;
    idle.fetch_sub(1, Ordering::SeqCst);
    match waited {
        Ok(Some((inner, first))) => {
            wake.notify_one();
            let conn: Conn = Box::new(Prefixed {
                first: Some(first),
                inner,
            });
            // Detached: a transfer survives the control connection being
            // replaced, and ends when its own connection does.
            tokio::spawn(async move {
                handler(conn).await;
                drop(open);
            });
        }
        // Recycled while idle: replace it at once.
        Ok(None) => {
            drop(open);
            wake.notify_one();
        }
        Err(_) => {
            drop(open);
            // Throttles the respawn while the relay refuses or is away.
            tokio::time::sleep(Duration::from_secs(1)).await;
            wake.notify_one();
        }
    }
}
