//! Accepting browser connections on the loopback sockets (FP-104).

use super::{Shared, http};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::rt::{TokioIo, TokioTimer};
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use std::task::Poll;
use std::time::Duration;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::task::JoinSet;

/// Connections served at once, signed in or not.
const MAX_CONNECTIONS: usize = 64;
/// How long a client may take to send its request head.
const HEAD_TIMEOUT: Duration = Duration::from_secs(10);
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
                        tasks.spawn(connection(Arc::clone(&shared), stream, permit));
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

async fn connection(shared: Arc<Shared>, stream: TcpStream, _permit: OwnedSemaphorePermit) {
    let _ = stream.set_nodelay(true);
    let cancel = shared.cancel.clone();
    let service = service_fn(move |request| {
        let shared = Arc::clone(&shared);
        async move { Ok::<_, Infallible>(http::respond(&shared, request)) }
    });
    let served = http1::Builder::new()
        .timer(TokioTimer::new())
        .header_read_timeout(HEAD_TIMEOUT)
        .serve_connection(TokioIo::new(stream), service)
        .with_upgrades();
    tokio::select! {
        () = cancel.cancelled() => {}
        _ = served => {}
    }
}
