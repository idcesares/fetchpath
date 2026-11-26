//! Single-request HTTP(S) streaming transfer used by verified multi-source
//! downloads and piece repair. Keeps all libcurl FFI strictly inside
//! `fetchpath-http`.

use super::{ProtocolCapabilities, ProtocolDecision, RequestContext, configure, decide_protocol};
use std::cell::{Cell, RefCell};
use std::time::{Duration, Instant};

/// Structured failure from [`fetch_single_request`].
#[derive(Debug)]
pub enum SingleTransferError<E> {
    /// The caller's chunk callback stopped the transfer with its own error.
    Callback(E),
    /// `is_cancelled` returned `true`.
    Cancelled,
    /// Connection, DNS, proxy, or receive failure before completion.
    Offline(String),
    /// The wall-clock ceiling expired.
    TimedOut { detail: String, responded: bool },
    /// An HTTP status >= 400 or libcurl protocol/configuration error.
    Protocol(String),
}

/// Streams a single HTTP(S) GET (or optional `(start, length)` byte range)
/// through libcurl with strict TLS verification, HTTP(S)-only redirects,
/// cancellation checks, and a hard wall-clock timeout.
#[allow(clippy::too_many_arguments)]
pub fn fetch_single_request<E, H, W, C>(
    url: &str,
    context: &RequestContext,
    buffer_bytes: usize,
    range: Option<(u64, u64)>,
    timeout: Duration,
    mut on_header: H,
    mut on_chunk: W,
    is_cancelled: C,
) -> Result<(), SingleTransferError<E>>
where
    H: FnMut(&[u8]),
    W: FnMut(&[u8]) -> Result<(), E>,
    C: Fn() -> bool,
{
    let decision: ProtocolDecision = decide_protocol(ProtocolCapabilities::detect());
    let mut easy = curl::easy::Easy::new();
    configure(&mut easy, url, context, &decision)
        .map_err(|error| SingleTransferError::Protocol(error.to_string()))?;
    easy.buffer_size(buffer_bytes)
        .map_err(|error| SingleTransferError::Protocol(error.to_string()))?;
    // A hard ceiling inside libcurl, so a mirror that stalls cannot hold the
    // transfer open while the progress callback waits to be called again.
    easy.timeout(timeout)
        .map_err(|error| SingleTransferError::Protocol(error.to_string()))?;
    if let Some((start, length)) = range {
        easy.range(&format!("{start}-{}", start + length - 1))
            .map_err(|error| SingleTransferError::Protocol(error.to_string()))?;
    }

    let started = Instant::now();
    let saw_response = Cell::new(false);
    let callback_error: RefCell<Option<E>> = RefCell::new(None);
    let timed_out = Cell::new(false);

    let transfer_result = {
        let mut stream = easy.transfer();
        stream
            .header_function(|line| {
                if line.starts_with(b"HTTP/") {
                    saw_response.set(true);
                }
                on_header(line);
                true
            })
            .map_err(|error| SingleTransferError::Protocol(error.to_string()))?;
        stream
            .write_function(|data| {
                saw_response.set(true);
                if let Err(error) = on_chunk(data) {
                    *callback_error.borrow_mut() = Some(error);
                    return Ok(0);
                }
                Ok(data.len())
            })
            .map_err(|error| SingleTransferError::Protocol(error.to_string()))?;
        stream
            .progress_function(|_, _, _, _| {
                if is_cancelled() {
                    return false;
                }
                if started.elapsed() > timeout {
                    timed_out.set(true);
                    return false;
                }
                true
            })
            .map_err(|error| SingleTransferError::Protocol(error.to_string()))?;
        stream.perform()
    };

    if let Some(error) = callback_error.into_inner() {
        return Err(SingleTransferError::Callback(error));
    }
    if is_cancelled() {
        return Err(SingleTransferError::Cancelled);
    }
    let responded = saw_response.get();
    if let Err(error) = transfer_result {
        if error.is_couldnt_connect()
            || error.is_couldnt_resolve_host()
            || error.is_couldnt_resolve_proxy()
            || error.is_recv_error()
        {
            return Err(SingleTransferError::Offline(error.to_string()));
        }
        if timed_out.get() || error.is_operation_timedout() {
            return Err(SingleTransferError::TimedOut {
                detail: format!("nothing completed within {timeout:?}"),
                responded,
            });
        }
        return Err(SingleTransferError::Protocol(match easy.response_code() {
            Ok(status) if status >= 400 => format!("HTTP status {status}"),
            _ => error.to_string(),
        }));
    }
    if started.elapsed() > timeout {
        return Err(SingleTransferError::TimedOut {
            detail: format!("no complete response within {timeout:?}"),
            responded: true,
        });
    }
    Ok(())
}
