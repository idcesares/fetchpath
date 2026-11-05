//! The response body of a link: a small page, or the encrypted stream pulled
//! from a [`Transfer`] one piece at a time (FP-117).

use fetchpath_session::share::{MAX_READ, Transfer};
use hyper::body::{Body as HttpBody, Bytes, Frame, SizeHint};
use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::time::Sleep;

pub(super) enum Body {
    Page(Option<Bytes>),
    Stream(Box<Stream>),
}

impl Body {
    pub(super) fn page(bytes: Bytes) -> Self {
        Self::Page(Some(bytes))
    }

    pub(super) fn empty() -> Self {
        Self::Page(None)
    }

    /// Bytes `start..end` of the transfer's stream.
    pub(super) fn stream(transfer: Transfer, start: u64, end: u64) -> Self {
        Self::Stream(Box::new(Stream {
            transfer,
            pos: start,
            end,
            sent: None,
            held: None,
        }))
    }
}

/// Pulls from the ticket on demand, so a slow client holds back the reads
/// (and the ticket's rate allowance) instead of a buffer.
///
/// A piece counts as delivered when the connection asks for the next one (the
/// last, when it is handed over), that is, when hyper has taken it for
/// writing. That is "handed to the connection", not "received": a client that
/// drops right after the last write still used its link up, which is the safe
/// direction for a single-use ticket.
pub(super) struct Stream {
    transfer: Transfer,
    pos: u64,
    end: u64,
    /// The range last handed out, not yet reported.
    sent: Option<(u64, u64)>,
    /// A piece read ahead of the ticket's rate allowance.
    held: Option<(Bytes, Pin<Box<Sleep>>)>,
}

impl HttpBody for Body {
    type Data = Bytes;
    type Error = io::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, io::Error>>> {
        match &mut *self {
            Self::Page(bytes) => Poll::Ready(bytes.take().map(|bytes| Ok(Frame::data(bytes)))),
            Self::Stream(stream) => stream.poll(context),
        }
    }

    fn size_hint(&self) -> SizeHint {
        match self {
            Self::Page(bytes) => SizeHint::with_exact(bytes.as_ref().map_or(0, |b| b.len() as u64)),
            Self::Stream(stream) => stream.size_hint(),
        }
    }
}

impl Stream {
    fn poll(&mut self, context: &mut Context<'_>) -> Poll<Option<Result<Frame<Bytes>, io::Error>>> {
        loop {
            if let Some((_, sleep)) = &mut self.held {
                std::task::ready!(sleep.as_mut().poll(context));
                if let Some((bytes, _)) = self.held.take() {
                    return Poll::Ready(Some(Ok(self.yield_piece(bytes))));
                }
            }
            if let Some((start, end)) = self.sent.take() {
                self.transfer.delivered(start, end);
            }
            if self.pos >= self.end {
                return Poll::Ready(None);
            }
            let want = (self.end - self.pos).min(MAX_READ as u64) as usize;
            let piece = match self.transfer.read(self.pos, want) {
                Ok(piece) => piece,
                // The link ended mid-download (revoked, expired, file changed):
                // the connection is cut so the client sees a failed download,
                // never a short file that looks complete.
                Err(_) => return Poll::Ready(Some(Err(io::Error::other("the link ended")))),
            };
            let start = self.pos;
            self.pos += piece.bytes.len() as u64;
            self.sent = Some((start, self.pos));
            let bytes = Bytes::from(piece.bytes);
            if piece.wait > Duration::ZERO {
                self.held = Some((bytes, Box::pin(tokio::time::sleep(piece.wait))));
            } else {
                return Poll::Ready(Some(Ok(self.yield_piece(bytes))));
            }
        }
    }

    /// Hands a piece to the connection. Hyper does not ask again once it has
    /// written the last byte its length promised, so the last piece is
    /// reported here rather than on the next poll.
    fn yield_piece(&mut self, bytes: Bytes) -> Frame<Bytes> {
        if self.pos >= self.end
            && let Some((start, end)) = self.sent.take()
        {
            self.transfer.delivered(start, end);
        }
        Frame::data(bytes)
    }

    fn size_hint(&self) -> SizeHint {
        let held = self
            .held
            .as_ref()
            .map_or(0, |(bytes, _)| bytes.len() as u64);
        SizeHint::with_exact(self.end.saturating_sub(self.pos) + held)
    }
}
