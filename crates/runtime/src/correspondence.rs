//! Correspondence v1 — the identity-scoped dial tone (`lait/correspondence/1`).
//!
//! This is a registered protocol and nothing behind it. It exists so that the
//! ALPN reaches installed machines *before* anything dials it, because a client
//! refuses an unregistered ALPN **before reading a byte**: the connection is
//! dropped with no negotiation and nothing the initiator can read as "try
//! another way". An ALPN shipped in the same release as the feature using it
//! therefore only works between two already-updated machines, and every older
//! install makes the feature look broken rather than absent.
//!
//! It is the successor-key lesson in another costume: a machine can only accept
//! what it already carries.
//!
//! # Why it names no Space
//!
//! Every other plane opens with a Space and is routed by it. Correspondence is
//! *identity-scoped* — reaching a person who may hold no actor in any Space you
//! know is the entire point — so its opening carries no Space and its route
//! cannot be keyed by one. That is the single structural change the plane needs
//! from the transport layer, and landing it here, empty, is what makes the rest
//! ordinary work later.
//!
//! # What it does not carry
//!
//! No identity claim. The transport has already authenticated the peer — in
//! lait a peer *is* its device key — so a self-declared initiator field would be
//! a forgeable copy of a fact the connection already knows, and the kind of
//! field someone eventually trusts. The exchange is two messages of pure
//! protocol and no assertions.

use serde::{Deserialize, Serialize};

/// The only correspondence protocol version this build speaks.
pub const CORRESPONDENCE_PROTOCOL: u16 = 1;

/// The correspondence v1 ALPN.
pub const CORRESPONDENCE_ALPN: &[u8] = b"lait/correspondence/1";

/// The Own lane: framed, identity-scoped, and admitted only from a device in
/// this profile's own set — the hub decides that before a byte is read.
///
/// Registered beside the dial tone for the dial tone's reason: an ALPN that
/// ships in the same release as the feature using it only ever works between
/// two already-updated machines.
pub const OWN_ALPN: &[u8] = b"lait/own/1";

/// Ceiling for one Own frame. A Space ticket is at most
/// `coordinates::MAX_DECODED` (64 KiB); this leaves room for the frame that
/// carries it and nothing like room for a payload.
pub const MAX_OWN_FRAME: usize = 96 * 1024;

/// Maximum encoded message size. Small on purpose: nothing here carries
/// payloads, and a generous bound on a plane that answers everything with a
/// refusal is a free amplifier.
pub const MAX_MESSAGE: usize = 1024;

/// What an initiator is opening for.
///
/// One variant today. It is an enum rather than an absence because postcard
/// tags enum variants, so *adding* one is a compatible change where widening a
/// struct is not — the same reasoning that makes a new `AclAction` variant safe
/// where a new field is not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Intent {
    /// Asking only whether this node serves correspondence at all.
    Probe,
}

/// The opening frame. Identity-scoped: it names no Space, by construction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hello {
    pub protocol: u16,
    pub intent: Intent,
}

/// Why a node is not proceeding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Outcome {
    /// This build registers the protocol and serves no correspondence plane.
    ///
    /// The honest answer for every node until the plane exists, and the whole
    /// reason this module ships early: it is *legible*. A peer learns "this
    /// node does not do correspondence yet" instead of watching a connection
    /// vanish, and that is a fact it can log, count and act on.
    Unavailable,
    /// The opening named a protocol version this build does not speak.
    UnsupportedProtocol { speaks: u16 },
}

/// The answer to a [`Hello`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reply {
    pub protocol: u16,
    pub outcome: Outcome,
}

/// Why a correspondence message failed validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Invalid {
    /// Oversized, malformed, or not the canonical encoding of what it decodes
    /// to.
    NonCanonical,
}

impl std::fmt::Display for Invalid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for Invalid {}

fn decode_canonical<T>(bytes: &[u8]) -> Result<T, Invalid>
where
    T: serde::de::DeserializeOwned + Serialize,
{
    if bytes.len() > MAX_MESSAGE {
        return Err(Invalid::NonCanonical);
    }
    let value: T = postcard::from_bytes(bytes).map_err(|_| Invalid::NonCanonical)?;
    let re = postcard::to_stdvec(&value).map_err(|_| Invalid::NonCanonical)?;
    if re != bytes {
        return Err(Invalid::NonCanonical);
    }
    Ok(value)
}

impl Hello {
    /// The opening this build sends.
    pub fn probe() -> Self {
        Self {
            protocol: CORRESPONDENCE_PROTOCOL,
            intent: Intent::Probe,
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        postcard::to_stdvec(self).unwrap_or_default()
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, Invalid> {
        decode_canonical(bytes)
    }
}

impl Reply {
    /// The answer every node gives until a correspondence plane exists.
    pub fn unavailable() -> Self {
        Self {
            protocol: CORRESPONDENCE_PROTOCOL,
            outcome: Outcome::Unavailable,
        }
    }

    /// The answer to an opening this build cannot read.
    pub fn unsupported() -> Self {
        Self {
            protocol: CORRESPONDENCE_PROTOCOL,
            outcome: Outcome::UnsupportedProtocol {
                speaks: CORRESPONDENCE_PROTOCOL,
            },
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        postcard::to_stdvec(self).unwrap_or_default()
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, Invalid> {
        decode_canonical(bytes)
    }
}

/// The answer this build gives to an opening, without consulting any Space.
///
/// Pure, so the routing decision is testable without a transport: the hub reads
/// one frame and calls this. When a plane exists it takes this function's place
/// and every caller is unchanged.
pub fn answer(opening: &[u8]) -> Reply {
    match Hello::decode(opening) {
        Ok(hello) if hello.protocol == CORRESPONDENCE_PROTOCOL => Reply::unavailable(),
        // A version this build cannot read is answered, not dropped. The
        // difference between "was answered no" and "could not be asked" is the
        // fact worth preserving, and silence destroys it.
        Ok(_) | Err(Invalid::NonCanonical) => Reply::unsupported(),
    }
}

/// What an initiator learned by dialing a peer.
///
/// The two arms are deliberately different facts. A node that refused the ALPN
/// is a node that could not be asked — it predates the protocol, and the right
/// response is to reach it another way. A node that answered [`Outcome`] was
/// asked and said no. Folding them together is the false-disconnection defect
/// one layer up, and it is exactly what an unregistered ALPN forces on a caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reachability {
    /// The peer answered.
    Answered(Outcome),
    /// The peer could not be asked: it refused the protocol, or the connection
    /// or the exchange failed. Never "answered no".
    Unasked(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_well_formed_probe_is_answered_unavailable() {
        let reply = answer(&Hello::probe().encode());
        assert_eq!(reply.outcome, Outcome::Unavailable);
        assert_eq!(reply.protocol, CORRESPONDENCE_PROTOCOL);
    }

    #[test]
    fn the_opening_carries_no_space() {
        // The structural claim this module exists to make. A Space is 29 bytes;
        // the whole encoded opening is smaller than that, so there is nowhere
        // for one to hide.
        let encoded = Hello::probe().encode();
        assert!(
            encoded.len() < 29,
            "an identity-scoped opening must not be carrying a Space, got {} bytes",
            encoded.len()
        );
    }

    #[test]
    fn an_unreadable_opening_is_answered_rather_than_dropped() {
        for hostile in [
            &b""[..],
            &b"not postcard at all"[..],
            &[0xFFu8; MAX_MESSAGE + 1][..],
        ] {
            let reply = answer(hostile);
            assert_eq!(
                reply.outcome,
                Outcome::UnsupportedProtocol {
                    speaks: CORRESPONDENCE_PROTOCOL
                },
                "every opening gets an answer; silence is the thing being avoided"
            );
        }
    }

    #[test]
    fn a_future_protocol_version_is_told_what_this_build_speaks() {
        let ahead = Hello {
            protocol: CORRESPONDENCE_PROTOCOL + 1,
            intent: Intent::Probe,
        };
        assert_eq!(
            answer(&ahead.encode()).outcome,
            Outcome::UnsupportedProtocol {
                speaks: CORRESPONDENCE_PROTOCOL
            }
        );
    }

    #[test]
    fn messages_round_trip_and_refuse_a_non_canonical_encoding() {
        let hello = Hello::probe();
        assert_eq!(Hello::decode(&hello.encode()), Ok(hello));
        let reply = Reply::unavailable();
        assert_eq!(Reply::decode(&reply.encode()), Ok(reply));

        let mut trailing = Hello::probe().encode();
        trailing.push(0);
        assert_eq!(Hello::decode(&trailing), Err(Invalid::NonCanonical));
    }
}
