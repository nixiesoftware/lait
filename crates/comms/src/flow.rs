//! The multi-flow connection seam — one peer, many concurrent conversations.
//!
//! [`Stream`](crate::Stream) is one framed bidirectional channel per dial, and
//! that is exactly right for Contact: a bounded protocol message at a time,
//! length framing owned by the transport, one conversation per connection. It
//! cannot express what the delivery planes need — a chunk transfer running
//! beside a control exchange beside a cursor datagram, each with its own
//! lifetime, its own abort, and its own idea of how much memory it may cost.
//!
//! So this adds a connection, and flows inside it. Nothing here replaces
//! `Stream`; Contact and presence keep the seam they were built against.
//!
//! **Bounds are the caller's, not the transport's.** `MAX_FRAME` is 64 MiB
//! because a framed transport materialises whole protocol messages and has to
//! guard the length prefix. A flow is read incrementally, so its ceiling bounds
//! *one read* rather than one message — and inheriting 64 MiB per concurrent
//! flow is how a handful of transfers exhausts a receiver. Every read here
//! takes its ceiling from the caller.
//!
//! **No vendor type crosses this seam.** A flow is bytes, an abort is a code,
//! and a datagram capacity is a number that may have changed since you last
//! asked.

use anyhow::Result;
use async_trait::async_trait;

use crate::PeerId;

/// The send half of one flow.
///
/// Finishing and resetting are different endings and the receiver can tell them
/// apart: a finish drains, a reset does not. That distinction is what lets an
/// abandoned transfer look like an abandoned transfer rather than a truncated
/// one — truncation has to be loud.
#[async_trait]
pub trait SendFlow: Send {
    /// Write bytes, parking under the peer's flow control as needed.
    async fn write_all(&mut self, bytes: &[u8]) -> Result<()>;

    /// Queue end-of-flow. Like `Stream::finish`, this does not confirm
    /// delivery — it says there will be nothing more.
    fn finish(&mut self) -> Result<()>;

    /// Abandon the flow. The receiver's next read fails rather than ending
    /// cleanly, which is the whole point of having this as well as `finish`.
    fn reset(&mut self, code: u32);

    /// A relative scheduling hint, higher first. Advisory: a transport may
    /// ignore it, and correctness never depends on it.
    fn set_priority(&mut self, _priority: i32) {}
}

/// The receive half of one flow.
#[async_trait]
pub trait RecvFlow: Send {
    /// Read up to `max` more bytes, `Ok(None)` at a clean end.
    ///
    /// `max` is a **pre-allocation ceiling**, not a target: the transport must
    /// not reserve more than this before bytes arrive, and may return fewer.
    /// A short read is not end-of-flow — only `None` is.
    async fn read_chunk(&mut self, max: usize) -> Result<Option<Vec<u8>>>;

    /// Read exactly `len` bytes. An end before that is an error, because a
    /// caller asking for a fixed-size header has no use for a partial one.
    async fn read_exact(&mut self, len: usize) -> Result<Vec<u8>> {
        let mut out = Vec::with_capacity(len.min(64 * 1024));
        while out.len() < len {
            let want = len.saturating_sub(out.len());
            match self.read_chunk(want).await? {
                Some(bytes) if !bytes.is_empty() => out.extend_from_slice(&bytes),
                _ => anyhow::bail!("flow ended after {} of {len} bytes", out.len()),
            }
        }
        Ok(out)
    }

    /// Read to the end, refusing past `max`.
    ///
    /// The bound is checked as bytes arrive rather than declared up front,
    /// because on a raw flow nobody declared anything.
    async fn read_to_end(&mut self, max: usize) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        while let Some(bytes) = self
            .read_chunk(max.saturating_sub(out.len()).max(1))
            .await?
        {
            if bytes.is_empty() {
                continue;
            }
            out.extend_from_slice(&bytes);
            if out.len() > max {
                anyhow::bail!("flow exceeded the caller's limit of {max} bytes");
            }
        }
        Ok(out)
    }

    /// Tell the sender to stop. Unlike dropping, this reaches the peer.
    fn stop(&mut self, code: u32);
}

/// One connection to one peer, carrying many flows.
#[async_trait]
pub trait Connection: Send + Sync {
    fn peer(&self) -> PeerId;

    /// The protocol this connection was negotiated for. The ALPN is the version
    /// gate, so this is also which generation the peer speaks.
    fn alpn(&self) -> Vec<u8>;

    /// Open a flow.
    ///
    /// **A flow does not exist for the peer until the opener writes to it,
    /// finishes it, or resets it.** Opening is local bookkeeping; the wire
    /// learns about it with the first thing sent. So an opener that parks
    /// waiting for its peer to accept — before sending anything — waits
    /// forever, and both contractors here behave that way on purpose.
    async fn open_bi(&self) -> Result<(Box<dyn SendFlow>, Box<dyn RecvFlow>)>;

    /// `Ok(None)` once the peer will open no more. See [`open_bi`] for when a
    /// flow becomes visible.
    ///
    /// [`open_bi`]: Connection::open_bi
    async fn accept_bi(&self) -> Result<Option<(Box<dyn SendFlow>, Box<dyn RecvFlow>)>>;

    /// Open a send-only flow. Visible to the peer on the first write, finish,
    /// or reset — see [`open_bi`](Connection::open_bi).
    async fn open_uni(&self) -> Result<Box<dyn SendFlow>>;

    async fn accept_uni(&self) -> Result<Option<Box<dyn RecvFlow>>>;

    /// Send one unreliable datagram.
    ///
    /// Fails rather than truncating when the payload does not fit the path.
    /// Transient payloads have no retransmit, so a half-delivered one arrives
    /// as corruption rather than as a gap — the caller's answer is to shrink or
    /// drop, never to send less than it meant.
    fn send_datagram(&self, payload: &[u8]) -> Result<()>;

    /// The next datagram, `Ok(None)` once none will arrive.
    async fn read_datagram(&self) -> Result<Option<Vec<u8>>>;

    /// How large a datagram this connection will currently carry.
    ///
    /// A **runtime query, not a constant.** The underlying limit is
    /// path-dependent and moves with NAT traversal and relay fallback — measured
    /// at 1382 and then 1162 bytes on two runs over one local path, the second
    /// below the 1200 lait attempts. `None` means the peer negotiated no
    /// datagram support, which is a refusal rather than an unlimited path.
    fn datagram_capacity(&self) -> Option<usize>;

    /// Close the connection, telling the peer why in a code it can act on.
    fn close(&self, code: u32, reason: &[u8]);

    /// Park until the connection is gone.
    async fn closed(&self);
}

/// An accepted inbound connection, before any flow has been read.
///
/// Distinct from [`Incoming`](crate::Incoming), which hands over a framed
/// stream: routing a *connection* means reading one bounded opening and then
/// giving the whole thing to one owner, rather than dispatching each stream.
pub struct IncomingConnection {
    pub from: PeerId,
    pub alpn: Vec<u8>,
    pub connection: Box<dyn Connection>,
    /// The bytes a router already read to decide where this connection goes.
    ///
    /// Empty when nobody read anything. It is carried rather than replayed
    /// because a flow is consumed by reading it: a router that reads an opening
    /// to find the Space cannot put it back, and the Space's owner needs the
    /// same bytes to bind the peer, check the lanes, and answer. Handing over
    /// the parse rather than the position is also what stops the two of them
    /// disagreeing about what the peer said.
    pub opening: Vec<u8>,
}
