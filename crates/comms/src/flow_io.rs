//! Comms flows as ordinary byte streams.
//!
//! The flow traits are message-shaped (`write_all`, `read_chunk`) because
//! that is what a transport owns; anything serving or speaking a byte
//! protocol over a flow — HTTP most of all — wants `AsyncRead`/`AsyncWrite`.
//! This is that adapter, shared so the coordinator's overlay server and the
//! reach router's splice cannot drift apart in how they read a flow.

use std::pin::Pin;
use std::task::{Context, Poll};

use anyhow::Result;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

use crate::{RecvFlow, SendFlow};

/// Ceiling for one read. A pre-allocation bound, not a target.
const READ_CHUNK: usize = 64 * 1024;

/// One flow as the byte stream a protocol rides on.
///
/// The comms flow traits are message-shaped (`write_all`, `read_chunk`), so
/// each poll drives an owned in-flight future that carries the flow half with
/// it and hands it back on completion — ownership passing rather than a
/// self-borrow, and the stored future is what makes every poll resumable.
pub struct FlowIo {
    send: Option<Box<dyn SendFlow>>,
    recv: Option<Box<dyn RecvFlow>>,
    /// Bytes read but not yet handed to the caller.
    buffered: Vec<u8>,
    read_in_flight: Option<ReadInFlight>,
    write_in_flight: Option<WriteInFlight>,
}

type BoxedPoll<T> = Pin<Box<dyn std::future::Future<Output = T> + Send + 'static>>;
type ReadInFlight = BoxedPoll<(Box<dyn RecvFlow>, Result<Option<Vec<u8>>>)>;
/// Carries how many bytes the completed write accepted, because
/// `poll_write` must answer with exactly that on the poll that resolves.
type WriteInFlight = BoxedPoll<(Box<dyn SendFlow>, Result<()>, usize)>;

impl FlowIo {
    pub fn new(send: Box<dyn SendFlow>, recv: Box<dyn RecvFlow>) -> Self {
        Self {
            send: Some(send),
            recv: Some(recv),
            buffered: Vec::new(),
            read_in_flight: None,
            write_in_flight: None,
        }
    }

    fn drain(&mut self, out: &mut ReadBuf<'_>) {
        let take = self.buffered.len().min(out.remaining());
        if let Some(chunk) = self.buffered.get(..take) {
            out.put_slice(chunk);
        }
        self.buffered.drain(..take);
    }
}

fn broken(error: impl std::fmt::Display) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::BrokenPipe, error.to_string())
}

impl AsyncRead for FlowIo {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        out: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        if !self.buffered.is_empty() {
            self.drain(out);
            return Poll::Ready(Ok(()));
        }
        let mut in_flight = match self.read_in_flight.take() {
            Some(in_flight) => in_flight,
            None => {
                let Some(mut recv) = self.recv.take() else {
                    // A finished flow reads as a clean end, repeatably.
                    return Poll::Ready(Ok(()));
                };
                Box::pin(async move {
                    let read = recv.read_chunk(READ_CHUNK).await;
                    (recv, read)
                })
            }
        };
        match in_flight.as_mut().poll(cx) {
            Poll::Pending => {
                self.read_in_flight = Some(in_flight);
                Poll::Pending
            }
            Poll::Ready((recv, Ok(Some(bytes)))) => {
                self.recv = Some(recv);
                self.buffered = bytes;
                self.drain(out);
                Poll::Ready(Ok(()))
            }
            // Clean end: the recv half is dropped, and later polls answer the
            // same way through the `None` arm above.
            Poll::Ready((_, Ok(None))) => Poll::Ready(Ok(())),
            Poll::Ready((_, Err(error))) => Poll::Ready(Err(broken(error))),
        }
    }
}

impl AsyncWrite for FlowIo {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let mut in_flight = match self.write_in_flight.take() {
            Some(in_flight) => in_flight,
            None => {
                let Some(mut send) = self.send.take() else {
                    return Poll::Ready(Err(broken("flow is closed")));
                };
                let owned = bytes.to_vec();
                let accepted = owned.len();
                Box::pin(async move {
                    let wrote = send.write_all(&owned).await;
                    (send, wrote, accepted)
                })
            }
        };
        match in_flight.as_mut().poll(cx) {
            Poll::Pending => {
                self.write_in_flight = Some(in_flight);
                Poll::Pending
            }
            Poll::Ready((send, Ok(()), accepted)) => {
                self.send = Some(send);
                Poll::Ready(Ok(accepted))
            }
            Poll::Ready((_, Err(error), _)) => Poll::Ready(Err(broken(error))),
        }
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        // `write_all` completes only once the transport accepted the bytes, so
        // the only thing to flush is an in-flight write.
        if let Some(mut in_flight) = self.write_in_flight.take() {
            return match in_flight.as_mut().poll(cx) {
                Poll::Pending => {
                    self.write_in_flight = Some(in_flight);
                    Poll::Pending
                }
                Poll::Ready((send, Ok(()), _)) => {
                    self.send = Some(send);
                    Poll::Ready(Ok(()))
                }
                Poll::Ready((_, Err(error), _)) => Poll::Ready(Err(broken(error))),
            };
        }
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match self.as_mut().poll_flush(cx) {
            Poll::Ready(Ok(())) => {}
            other => return other,
        }
        if let Some(mut send) = self.send.take() {
            if let Err(error) = send.finish() {
                return Poll::Ready(Err(broken(error)));
            }
        }
        Poll::Ready(Ok(()))
    }
}
