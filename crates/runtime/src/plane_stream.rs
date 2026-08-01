//! One framing, and one way to find out what a stream is.
//!
//! Freight grew its own length-prefixed framing because it was the first plane
//! to need one. Live needs the same thing, and a second copy would be a second
//! place for the property that matters to be got subtly wrong — so the framing
//! moves here, generalised over what it decodes and bounded by what the caller
//! says it will accept.
//!
//! The property, stated once: **a frame is refused by its declared length,
//! before a buffer that size exists.** Reading first and checking after is not
//! a bound, it is a bound-shaped comment on an allocation a peer already chose.
//!
//! The other half is `read_stream_kind`. `plane::stream_kind` distinguishes a
//! kind this build does not implement *yet* from one it has never heard of, and
//! that distinction had no reader. It matters: a reserved kind is a peer
//! speaking a protocol we agreed to and have not finished, so the stream is
//! reset and the connection continues. An unknown kind is a peer speaking
//! something else, which is worth counting separately even though the immediate
//! response is the same.

use crate::plane::{bounds, stream_kind};

/// The length prefix every framed message carries.
///
/// Four bytes, little-endian. A request needs no delimiter — one request per
/// flow, and finishing says where it ends — but a response does: an answer that
/// is a header followed by raw bytes has to tell the reader where one stops
/// without the reader trusting a length inside the part it has not read yet.
pub const FRAME_PREFIX: usize = 4;

/// Why a framed read did not produce a message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Invalid {
    /// The flow ended before the message did.
    Truncated,
    /// A declared length past what this side will allocate.
    TooLarge,
    /// The bytes arrived and did not decode.
    Malformed,
    /// A stream kind this build knows about and does not implement.
    ///
    /// Separate from `UnknownKind` because it is not a peer misbehaving — it is
    /// a peer using a reservation we published.
    ReservedKind(u8),
    /// A stream kind from outside the vocabulary.
    UnknownKind(u8),
}

impl std::fmt::Display for Invalid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

/// Encode one message with its length prefix.
pub fn frame(body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(FRAME_PREFIX + body.len());
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(body);
    out
}

/// Read one length-prefixed message, bounded before anything is allocated.
///
/// `max` is the caller's ceiling and is intersected with the plane-wide control
/// bound rather than replacing it — a caller cannot raise the protocol's limit
/// by asking, only lower it for its own use.
pub async fn read_framed(flow: &mut dyn comms::RecvFlow, max: usize) -> Result<Vec<u8>, Invalid> {
    let header = flow
        .read_exact(FRAME_PREFIX)
        .await
        .map_err(|_| Invalid::Truncated)?;
    let len = u32::from_le_bytes(header.try_into().expect("four bytes")) as usize;
    if len > max.min(bounds::MAX_CONTROL_FRAME_BYTES) {
        return Err(Invalid::TooLarge);
    }
    flow.read_exact(len).await.map_err(|_| Invalid::Truncated)
}

/// Read the one byte that says what a stream is.
///
/// Three answers, and the difference between the last two is the whole reason
/// this exists rather than a `match` at each call site:
///
/// - implemented: the caller serves it;
/// - reserved: a kind this build published and has not built. The stream is
///   reset and the connection stays up, because the peer is not wrong;
/// - unknown: outside the vocabulary. Same immediate response, different
///   counter — one of these is a version skew and the other is noise, and an
///   operator looking at a Station wants to know which.
pub async fn read_stream_kind(flow: &mut dyn comms::RecvFlow) -> Result<u8, Invalid> {
    let byte = flow.read_exact(1).await.map_err(|_| Invalid::Truncated)?;
    let kind = byte.first().copied().ok_or(Invalid::Truncated)?;
    if stream_kind::is_implemented(kind) {
        Ok(kind)
    } else if stream_kind::is_reserved(kind) {
        Err(Invalid::ReservedKind(kind))
    } else {
        Err(Invalid::UnknownKind(kind))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_frame_round_trips_through_its_own_prefix() {
        let framed = frame(b"hello");
        assert_eq!(&framed[..FRAME_PREFIX], &5u32.to_le_bytes());
        assert_eq!(&framed[FRAME_PREFIX..], b"hello");
    }

    #[test]
    fn a_caller_can_lower_the_ceiling_and_not_raise_it() {
        // The plane-wide bound is the protocol's, and a caller asking for more
        // than the protocol permits is asking for something no peer may send.
        // Intersecting rather than replacing is what keeps the ceiling a
        // property of the plane rather than of whoever called last.
        let generous = usize::MAX;
        assert_eq!(
            generous.min(bounds::MAX_CONTROL_FRAME_BYTES),
            bounds::MAX_CONTROL_FRAME_BYTES
        );
        let tight = 16usize;
        assert_eq!(tight.min(bounds::MAX_CONTROL_FRAME_BYTES), tight);
    }

    #[test]
    fn a_reserved_kind_and_an_unknown_one_are_different_answers() {
        // The distinction `plane::stream_kind` draws and nothing read until
        // now. A reserved kind is a peer using a reservation we published; an
        // unknown one is a peer speaking something else. Both reset the stream;
        // only one of them means a version skew.
        assert!(stream_kind::is_implemented(stream_kind::CONTROL));
        assert!(stream_kind::is_implemented(stream_kind::RELIABLE_SIGNAL));
        assert!(stream_kind::is_reserved(stream_kind::RESERVED_MEDIA_FRAME));
        assert!(stream_kind::is_reserved(
            stream_kind::RESERVED_MEDIA_FEEDBACK
        ));
        assert!(!stream_kind::is_implemented(
            stream_kind::RESERVED_MEDIA_FRAME
        ));
        assert!(!stream_kind::is_reserved(0x7f));
        assert!(!stream_kind::is_implemented(0x7f));
    }
}
