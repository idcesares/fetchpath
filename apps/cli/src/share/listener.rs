//! Accepting connections on the share port (FP-117).
//!
//! The person's tunnel connects from this computer, so only loopback peers
//! are served, as on the web UI port. Connection count and lifetime are
//! capped: a link is a public address and must not be a way to hold the
//! engine's sockets.

use super::{Shared, http};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::rt::{TokioIo, TokioTimer};
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use std::task::Poll;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

/// Connections served at once.
const MAX_CONNECTIONS: usize = 32;
/// How long a client may take to send its request head.
const HEAD_TIMEOUT: Duration = Duration::from_secs(10);
/// The longest one connection lives, however slowly it downloads.
const MAX_LIFETIME: Duration = Duration::from_secs(4 * 60 * 60);
/// How long connections get to finish when the listener stops.
const DRAIN: Duration = Duration::from_secs(1);

async fn accept_any(listeners: &[TcpListener]) -> std::io::Result<(TcpStream, SocketAddr)> {
    std::future::poll_fn(|context| {
        for listener in listeners {
            if let Poll::Ready(accepted) = listener.poll_accept(context) {
                return Poll::Ready(accepted);
            }
        }
        Poll::Pending
    })
    .await
}

/// Serves until the listener is cancelled, then closes the ports and gives
/// connections a moment to end (they watch the same cancellation).
pub(super) async fn run(shared: Arc<Shared>, listeners: Vec<TcpListener>) {
    let permits = Arc::new(Semaphore::new(MAX_CONNECTIONS));
    let mut tasks = JoinSet::new();
    loop {
        tokio::select! {
            () = shared.cancel.cancelled() => break,
            accepted = accept_any(&listeners) => match accepted {
                Ok((stream, peer)) => {
                    let Ok(permit) = Arc::clone(&permits).try_acquire_owned() else {
                        continue;
                    };
                    if peer.ip().is_loopback() {
                        let _ = stream.set_nodelay(true);
                        let shared = Arc::clone(&shared);
                        tasks.spawn(async move {
                            let _permit = permit;
                            serve(shared, stream, true).await;
                        });
                    }
                }
                // Transient, such as a client that reset while waiting.
                Err(_) => tokio::time::sleep(Duration::from_millis(50)).await,
            }
        }
        while tasks.try_join_next().is_some() {}
    }
    drop(listeners);
    let _ = tokio::time::timeout(DRAIN, async { while tasks.join_next().await.is_some() {} }).await;
    tasks.abort_all();
}

/// Serves HTTP/1 on one connection. `keep_alive` is off for relayed
/// connections: the relay hands each one a single request.
pub(super) async fn serve<I>(shared: Arc<Shared>, stream: I, keep_alive: bool)
where
    I: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let cancel = shared.cancel.clone();
    let service = service_fn(move |request| {
        let shared = Arc::clone(&shared);
        async move { Ok::<_, Infallible>(http::respond(&shared, request)) }
    });
    let served = http1::Builder::new()
        .keep_alive(keep_alive)
        .timer(TokioTimer::new())
        .header_read_timeout(HEAD_TIMEOUT)
        .serve_connection(TokioIo::new(stream), service);
    tokio::select! {
        () = cancel.cancelled() => {}
        () = tokio::time::sleep(MAX_LIFETIME) => {}
        _ = served => {}
    }
}
