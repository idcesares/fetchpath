//! The relay itself: enrolled engines on one side, visitors on the other.
//!
//! A visitor's request head is read only to find `GET /s/<id>`; the bytes
//! are then forwarded unchanged to a work connection of the engine that
//! announced the id. Nothing about a visitor, an id or a body is kept or
//! logged: the observer sees [`Event`]s that carry no such data.

use crate::wire::{self, EnrollmentKey, MAX_ID, MAX_IDS, Role, TAG_ANNOUNCE, TAG_PING};
use std::collections::{HashMap, HashSet, VecDeque};
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::sync::{Notify, Semaphore};

pub trait Io: AsyncRead + AsyncWrite + Unpin + Send + 'static {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send + 'static> Io for T {}
pub type Conn = Box<dyn Io>;

/// Sizes and times that bound what one visitor or engine can hold.
#[derive(Clone, Debug)]
pub struct Limits {
    pub visitors: usize,
    pub idle_per_engine: usize,
    pub head_bytes: usize,
    pub head_timeout: Duration,
    pub wait_for_engine: Duration,
    pub control_silence: Duration,
    pub lifetime: Duration,
    /// A transfer that moves no byte for this long ends.
    pub idle: Duration,
    /// An idle work connection older than this is not handed to a visitor.
    pub work_life: Duration,
    pub handshakes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            visitors: 64,
            idle_per_engine: 16,
            head_bytes: 8 * 1024,
            head_timeout: Duration::from_secs(10),
            wait_for_engine: Duration::from_secs(5),
            control_silence: Duration::from_secs(60),
            lifetime: Duration::from_secs(4 * 60 * 60),
            idle: Duration::from_secs(5 * 60),
            work_life: Duration::from_secs(90),
            handshakes: 32,
        }
    }
}

/// What the observer is told. No variant holds a path, id, header, key or
/// body byte; `name` is the engine's chosen label.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Event {
    EngineEnrolled { name: String },
    EngineLeft { name: String },
    EngineRefused,
    Served,
    NotFound,
    NoEngine,
    Busy,
    Malformed,
}

impl std::fmt::Display for Event {
    /// The log line: a fixed name, plus the engine label where there is one.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Event::EngineEnrolled { name } => write!(formatter, "engine enrolled name={name}"),
            Event::EngineLeft { name } => write!(formatter, "engine left name={name}"),
            Event::EngineRefused => formatter.write_str("engine refused"),
            Event::Served => formatter.write_str("served"),
            Event::NotFound => formatter.write_str("not found"),
            Event::NoEngine => formatter.write_str("no engine"),
            Event::Busy => formatter.write_str("busy"),
            Event::Malformed => formatter.write_str("malformed"),
        }
    }
}

struct Slot {
    generation: u64,
    ids: HashSet<String>,
    idle: VecDeque<(Conn, Instant)>,
    /// Wakes the control loop that this slot replaced.
    evict: Arc<Notify>,
}

pub struct Relay {
    key: EnrollmentKey,
    limits: Limits,
    engines: Mutex<HashMap<String, Slot>>,
    generation: AtomicU64,
    visitors: Arc<Semaphore>,
    heads: Arc<Semaphore>,
    handshakes: Arc<Semaphore>,
    observer: Box<dyn Fn(Event) + Send + Sync>,
}

const NOT_FOUND: &[u8] =
    b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
const BAD_REQUEST: &[u8] =
    b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
const BAD_GATEWAY: &[u8] =
    b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
const BUSY: &[u8] =
    b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";

impl Relay {
    pub fn new(
        key: EnrollmentKey,
        limits: Limits,
        observer: impl Fn(Event) + Send + Sync + 'static,
    ) -> Arc<Self> {
        Arc::new(Self {
            key,
            visitors: Arc::new(Semaphore::new(limits.visitors)),
            heads: Arc::new(Semaphore::new(limits.visitors * 2)),
            handshakes: Arc::new(Semaphore::new(limits.handshakes)),
            limits,
            engines: Mutex::new(HashMap::new()),
            generation: AtomicU64::new(0),
            observer: Box::new(observer),
        })
    }

    fn engines(&self) -> MutexGuard<'_, HashMap<String, Slot>> {
        self.engines
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn observe(&self, event: Event) {
        (self.observer)(event);
    }

    /// An engine's connection: proves the key, then becomes the engine's
    /// control connection or one of its idle work connections.
    pub async fn serve_engine<S: Io>(self: Arc<Self>, mut stream: S) {
        // Bounds engine-side connections that have not proved the key yet.
        let Ok(permit) = Arc::clone(&self.handshakes).try_acquire_owned() else {
            return;
        };
        let accepted = wire::accept(&mut stream, &self.key).await;
        drop(permit);
        let Ok((role, name)) = accepted else {
            self.observe(Event::EngineRefused);
            return;
        };
        match role {
            Role::Control => self.control(stream, name).await,
            Role::Work => {
                let mut engines = self.engines();
                if let Some(slot) = engines.get_mut(&name) {
                    // The oldest is the likeliest to be dead: make room by
                    // dropping it rather than refusing a fresh one.
                    while slot.idle.len() >= self.limits.idle_per_engine {
                        slot.idle.pop_front();
                    }
                    slot.idle.push_back((Box::new(stream), Instant::now()));
                }
            }
        }
    }

    async fn control<S: Io>(&self, mut stream: S, name: String) {
        let generation = self.generation.fetch_add(1, Ordering::Relaxed);
        let evict = Arc::new(Notify::new());
        // A second control connection under one name replaces the first and
        // closes it: names are unique per relay.
        if let Some(old) = self.engines().insert(
            name.clone(),
            Slot {
                generation,
                ids: HashSet::new(),
                idle: VecDeque::new(),
                evict: Arc::clone(&evict),
            },
        ) {
            old.evict.notify_one();
        }
        self.observe(Event::EngineEnrolled { name: name.clone() });
        loop {
            let read = tokio::time::timeout(self.limits.control_silence, async {
                let mut tag = [0u8; 1];
                let evicted = evict.notified();
                tokio::pin!(evicted);
                let read_tag = stream.read_exact(&mut tag);
                tokio::select! {
                    () = &mut evicted => return Err(std::io::Error::other("replaced")),
                    read = read_tag => { read?; }
                }
                match tag[0] {
                    TAG_PING => Ok(None),
                    TAG_ANNOUNCE => read_ids(&mut stream).await.map(Some),
                    _ => Err(std::io::Error::other("unknown frame")),
                }
            })
            .await;
            match read {
                Ok(Ok(Some(ids))) => {
                    if let Some(slot) = self.engines().get_mut(&name)
                        && slot.generation == generation
                    {
                        slot.ids = ids;
                    }
                }
                Ok(Ok(None)) => {}
                _ => break,
            }
        }
        let mut engines = self.engines();
        if engines
            .get(&name)
            .is_some_and(|slot| slot.generation == generation)
        {
            engines.remove(&name);
        }
        drop(engines);
        self.observe(Event::EngineLeft { name });
    }

    /// A visitor's connection: one request, forwarded unchanged.
    pub async fn serve_visitor<S: Io>(self: Arc<Self>, mut stream: S) {
        // Slow heads are bounded apart from running transfers, so they cannot
        // take every transfer slot.
        let Ok(head_permit) = Arc::clone(&self.heads).try_acquire_owned() else {
            self.observe(Event::Busy);
            let _ = stream.write_all(BUSY).await;
            return;
        };
        let (head, id) = match read_head(&mut stream, &self.limits).await {
            Head::Routed(head, id) => (head, id),
            Head::Elsewhere => {
                self.observe(Event::NotFound);
                let _ = stream.write_all(NOT_FOUND).await;
                return;
            }
            Head::Malformed => {
                self.observe(Event::Malformed);
                let _ = stream.write_all(BAD_REQUEST).await;
                return;
            }
        };
        drop(head_permit);
        let Ok(_permit) = Arc::clone(&self.visitors).try_acquire_owned() else {
            self.observe(Event::Busy);
            let _ = stream.write_all(BUSY).await;
            return;
        };
        let Some(mut engine) = self.engine_for(&id, &head).await else {
            self.observe(Event::NoEngine);
            let _ = stream.write_all(BAD_GATEWAY).await;
            return;
        };
        self.observe(Event::Served);
        let seen = Arc::new(AtomicU64::new(0));
        let started = Instant::now();
        let mut stream = Watched {
            inner: stream,
            seen: Arc::clone(&seen),
            started,
        };
        let pipe = tokio::io::copy_bidirectional(&mut stream, &mut engine);
        let watchdog = async {
            loop {
                tokio::time::sleep(self.limits.idle / 4).await;
                let quiet = started
                    .elapsed()
                    .saturating_sub(Duration::from_millis(seen.load(Ordering::Relaxed)));
                if quiet >= self.limits.idle {
                    return;
                }
            }
        };
        let _ = tokio::time::timeout(self.limits.lifetime, async {
            tokio::select! {
                _ = pipe => {}
                () = watchdog => {}
            }
        })
        .await;
        let _ = stream.shutdown().await;
    }

    /// An idle work connection of the engine that announced `id`, with the
    /// request head already written to it.
    async fn engine_for(&self, id: &str, head: &[u8]) -> Option<Conn> {
        let deadline = Instant::now() + self.limits.wait_for_engine;
        loop {
            let taken = {
                let mut engines = self.engines();
                engines
                    .values_mut()
                    .find(|slot| slot.ids.contains(id))
                    .map(|slot| slot.idle.pop_front())
            };
            match taken {
                // No engine has this id: waiting would not change that.
                None => return None,
                Some(Some((_, since))) if since.elapsed() > self.limits.work_life => {
                    // Older than a NAT would keep it open: dropped unused.
                    continue;
                }
                Some(Some((mut conn, _))) => {
                    // An idle connection never receives: anything readable
                    // now (end of stream, error, stray byte) means it is dead.
                    let mut probe = [0u8; 1];
                    if tokio::time::timeout(Duration::ZERO, conn.read(&mut probe))
                        .await
                        .is_ok()
                    {
                        continue;
                    }
                    if conn.write_all(head).await.is_ok() && conn.flush().await.is_ok() {
                        return Some(conn);
                    }
                    // A work connection that died while idle: try the next.
                    continue;
                }
                Some(None) => {}
            }
            if Instant::now() >= deadline {
                return None;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }
}

async fn read_ids<S: AsyncRead + Unpin>(stream: &mut S) -> std::io::Result<HashSet<String>> {
    let mut count = [0u8; 2];
    stream.read_exact(&mut count).await?;
    let count = usize::from(u16::from_be_bytes(count));
    if count > MAX_IDS {
        return Err(std::io::Error::other("too many ids"));
    }
    let mut ids = HashSet::with_capacity(count);
    for _ in 0..count {
        let mut length = [0u8; 1];
        stream.read_exact(&mut length).await?;
        let length = usize::from(length[0]);
        if length == 0 || length > MAX_ID {
            return Err(std::io::Error::other("bad id"));
        }
        let mut id = vec![0u8; length];
        stream.read_exact(&mut id).await?;
        let id = String::from_utf8(id).map_err(|_| std::io::Error::other("bad id"))?;
        if !wire::valid_id(&id) {
            return Err(std::io::Error::other("bad id"));
        }
        ids.insert(id);
    }
    Ok(ids)
}

enum Head {
    /// The whole head as received, and the id it asks for.
    Routed(Vec<u8>, String),
    /// A well-formed request for something the relay does not route.
    Elsewhere,
    Malformed,
}

async fn read_head<S: AsyncRead + Unpin>(stream: &mut S, limits: &Limits) -> Head {
    let mut buffer: Vec<u8> = Vec::with_capacity(1024);
    let work = async {
        loop {
            let mut chunk = [0u8; 1024];
            let read = match stream.read(&mut chunk).await {
                Ok(0) | Err(_) => return Head::Malformed,
                Ok(read) => read,
            };
            buffer.extend_from_slice(&chunk[..read]);
            let mut headers = [httparse::EMPTY_HEADER; 64];
            let mut request = httparse::Request::new(&mut headers);
            match request.parse(&buffer) {
                Ok(httparse::Status::Complete(_)) => {
                    let id = (request.method == Some("GET"))
                        .then_some(request.path)
                        .flatten()
                        .map(|path| path.split('?').next().unwrap_or(path))
                        .and_then(|path| path.strip_prefix("/s/"))
                        .filter(|id| wire::valid_id(id))
                        .map(str::to_owned);
                    return match id {
                        Some(id) => Head::Routed(std::mem::take(&mut buffer), id),
                        None => Head::Elsewhere,
                    };
                }
                Ok(httparse::Status::Partial) if buffer.len() < limits.head_bytes => {}
                _ => return Head::Malformed,
            }
        }
    };
    tokio::time::timeout(limits.head_timeout, work)
        .await
        .unwrap_or(Head::Malformed)
}

/// Records when the visitor side last moved a byte, for the idle limit.
struct Watched<S> {
    inner: S,
    seen: Arc<AtomicU64>,
    started: Instant,
}

impl<S> Watched<S> {
    fn touch(&self) {
        let millis = u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX);
        self.seen.store(millis, Ordering::Relaxed);
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for Watched<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let polled = Pin::new(&mut self.inner).poll_read(context, buffer);
        if polled.is_ready() {
            self.touch();
        }
        polled
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for Watched<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let polled = Pin::new(&mut self.inner).poll_write(context, bytes);
        if polled.is_ready() {
            self.touch();
        }
        polled
    }

    fn poll_flush(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(context)
    }

    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(context)
    }
}
