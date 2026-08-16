#![allow(
    clippy::as_conversions,
    clippy::expect_used,
    reason = "plane framing converts only values already bounded by its wire ceilings"
)]
//! The runtime's delivery-plane protocol records.
//!
//! Freight moves exact requested bytes. Live hosts bounded realtime sessions —
//! cursors, presence, and small reliable signals. They are separate ALPNs
//! because they have different admission, timeouts, memory profiles, shutdown
//! semantics, and compatibility lifetimes: a file fetch should not keep a
//! realtime session alive, and a cursor bug should not block artifact recovery.
//!
//! **On the name.** The docket calls the second plane "Session". This crate
//! already has `session`, meaning a *World* session — a client docked against a
//! Station, committing transactions. Two unrelated things under one name is
//! worse for a reader than one module diverging from the prose, so the plane is
//! `live` here and the encoded ALPN keeps `lait/session/1` exactly as
//! specified. Nothing on the wire moves.
//!
//! This module is shapes and bounds only. There is no handler, no connection,
//! and no routing: S0 freezes what S1 and later build against, and freezing it
//! first is what stops three planes growing three bespoke framings.

use serde::{Deserialize, Serialize};

pub mod contact;
pub mod freight;
pub mod live;

pub use crate::contact_driver::{CommsOptions, GossipOptions, MAX_CONTACTS_IN_FLIGHT};
pub use crate::lifecycle::Activation;

/// Reliable exact-object request and response.
pub const FREIGHT_ALPN: &[u8] = b"lait/freight/1";
/// Long-lived realtime session.
pub const LIVE_ALPN: &[u8] = b"lait/session/1";

/// The protocol generation each ALPN speaks.
///
/// **The ALPN is the version gate.** iroh negotiates it during the QUIC
/// handshake, so peers on different generations share no common ALPN and cannot
/// connect at all — no in-band check, no half-speaking pair. That makes a bump
/// expensive, and expensive gates get avoided, so the discipline is: bump only
/// for a change an old peer would *misinterpret* — a removed or repurposed
/// field, changed semantics of an existing one — and carry every additive
/// capability as a feature bit instead.
pub const FREIGHT_PROTOCOL_VERSION: u16 = 1;
pub const LIVE_PROTOCOL_VERSION: u16 = 1;

/// Typed stream kinds within one live connection.
///
/// One byte, and allocated values are never silently reused. `0x03` and `0x04`
/// are the native media pair: Group streams and bounded control/feedback.
pub mod stream_kind {
    /// Long-lived bidirectional framed control.
    pub const CONTROL: u8 = 0x01;
    /// One bounded message per short stream.
    pub const RELIABLE_SIGNAL: u8 = 0x02;
    /// One Group header plus length-delimited encoded Frames on a uni stream.
    pub const MEDIA_GROUP: u8 = 0x03;
    /// Bounded media control and feedback on a short bidirectional flow.
    pub const MEDIA_CONTROL: u8 = 0x04;

    /// Whether this build implements a kind.
    pub fn is_implemented(kind: u8) -> bool {
        matches!(
            kind,
            CONTROL | RELIABLE_SIGNAL | MEDIA_GROUP | MEDIA_CONTROL
        )
    }

    /// Media lanes are granted as a pair and only when the media feature was
    /// negotiated. Keeping this predicate separate makes that admission rule
    /// explicit without teaching `comms` what a lane means.
    pub fn is_media(kind: u8) -> bool {
        matches!(kind, MEDIA_GROUP | MEDIA_CONTROL)
    }
}

/// The longest human-readable text a signal may carry.
///
/// A display name or a media type, not a message. Both are shown to a person
/// and neither is content — a signal that wanted to carry prose would be a
/// signal carrying a Body's job.
pub const MAX_SIGNAL_TEXT_BYTES: usize = 256;

/// What a peer is invited to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum InviteKind {
    Collaborate,
}

/// One reliable signal.
///
/// Bounded, one per stream, and never durable. The shapes are closed: a World
/// that needs its own says so through `WorldSignal`, whose payload the
/// substrate carries and does not interpret.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Signal {
    /// Are you there. The one signal that expects an answer.
    Ping { nonce: [u8; 16] },
    /// I am. Never itself acknowledged — that is how a ping becomes a loop.
    Acknowledge { nonce: [u8; 16] },
    /// Look at this.
    Attention { scope: crate::transient::Target },
    /// Come and work on this with me.
    SessionInvite {
        kind: InviteKind,
        scope: crate::transient::Target,
    },
    /// I have a file you may want.
    ///
    /// An offer, not a transfer. It names content the sender holds; whether the
    /// receiver wants it is a decision a person makes afterwards, which is why
    /// this expects no answer.
    FileOffer {
        content: [u8; 32],
        plaintext_len: u64,
        /// What to call it, as the sender sees it.
        ///
        /// Sanitised **on use**, never on decode. A decode-time rewrite makes
        /// `encode(decode(x)) == x` false, and canonical re-encode equality is
        /// what every shape on this plane rests on.
        display_name: String,
        media_type: String,
    },
    /// A World's own signal. Opaque here.
    WorldSignal {
        world: String,
        schema: String,
        payload: Vec<u8>,
    },
}

impl Signal {
    /// Which declaration governs this signal.
    pub fn selector(&self) -> u16 {
        use crate::signal::selector;
        match self {
            Self::Ping { .. } => selector::PING,
            Self::Acknowledge { .. } => selector::ACKNOWLEDGE,
            Self::Attention { .. } => selector::ATTENTION,
            Self::SessionInvite { .. } => selector::SESSION_INVITE,
            Self::FileOffer { .. } => selector::FILE_OFFER,
            Self::WorldSignal { .. } => selector::WORLD,
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        postcard::to_stdvec(self).expect("postcard signal")
    }

    /// Decode one signal, in the order that makes each check protect the next.
    ///
    /// Deliberately no `encode_bounded`. A frame that is too large to send is a
    /// caller's error and gets a `Result`; substituting a refusal — which is
    /// right for `FreightFrame`, where the alternative is telling a peer
    /// nothing — would silently send something other than what was asked for.
    pub fn decode_canonical(bytes: &[u8]) -> Result<Self, WireError> {
        if bytes.len() > bounds::MAX_SIGNAL_BYTES {
            return Err(WireError::TooLarge);
        }
        let signal: Self = postcard::from_bytes(bytes).map_err(|_| WireError::NonCanonical)?;
        if signal.encode() != bytes {
            return Err(WireError::NonCanonical);
        }
        signal.validate()?;
        Ok(signal)
    }

    pub fn validate(&self) -> Result<(), WireError> {
        let text = |value: &str| {
            if value.len() > MAX_SIGNAL_TEXT_BYTES {
                return Err(WireError::Bounds);
            }
            // A control character in a name lands in a header, a filename, or a
            // terminal. None of those are places a peer chooses what happens.
            if value.chars().any(|c| c.is_control()) {
                return Err(WireError::NonCanonical);
            }
            Ok(())
        };
        match self {
            Self::Ping { .. } | Self::Acknowledge { .. } => Ok(()),
            Self::Attention { scope } | Self::SessionInvite { scope, .. } => {
                scope.validate_wire().map_err(|_| WireError::Bounds)
            }
            Self::FileOffer {
                display_name,
                media_type,
                ..
            } => {
                text(display_name)?;
                text(media_type)
            }
            Self::WorldSignal {
                world,
                schema,
                payload,
            } => {
                // Parsed through the real grammars rather than a length check:
                // a World id and a schema id have shapes, and something that is
                // merely short is not therefore one.
                replica::body::WorldId::parse(world).ok_or(WireError::NonCanonical)?;
                replica::body::SchemaId::parse(schema).ok_or(WireError::NonCanonical)?;
                if payload.len() > bounds::MAX_SIGNAL_BYTES {
                    return Err(WireError::Bounds);
                }
                Ok(())
            }
        }
    }
}

/// Bounds that constrain each other, checked at compile time.
///
/// Moved out of the fixtures for the reason clippy names: a comparison of two
/// constants is decided when this file compiles, so making it a test defers a
/// build error into a test run. Here it stops the build at the file the numbers
/// live in.
mod bound_consistency {
    use super::bounds;

    const _: () = assert!(
        bounds::MAX_DATAGRAM_BYTES <= 1_200,
        "a datagram must fit the smallest path anyone measures, not the largest"
    );
    const _: () = assert!(
        bounds::MAX_SIGNAL_BYTES < bounds::MAX_CONTROL_FRAME_BYTES,
        "one signal must fit inside a control frame, or it can never be sent"
    );
}

/// Which plane an opening is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Plane {
    Freight,
    Live,
    Contact,
}

impl Plane {
    /// Whether one connection on this plane carries typed stream kinds.
    ///
    /// Freight does not: the ALPN types the connection and every flow is a
    /// request. Live does, which is why its opening names the lanes it wants and
    /// Freight's names none.
    pub fn serves_lanes(self) -> bool {
        match self {
            Self::Freight => false,
            Self::Live => true,
            Self::Contact => false,
        }
    }

    pub fn alpn(self) -> &'static [u8] {
        match self {
            Plane::Freight => FREIGHT_ALPN,
            Plane::Live => LIVE_ALPN,
            Plane::Contact => b"lait/contact/2",
        }
    }

    pub fn protocol_version(self) -> u16 {
        match self {
            Plane::Freight => FREIGHT_PROTOCOL_VERSION,
            Plane::Live => LIVE_PROTOCOL_VERSION,
            Plane::Contact => 2,
        }
    }
}

/// Additive capabilities, negotiated inside an ALPN rather than by bumping it.
///
/// A peer acts on a bit only if the other side set it, and an absent field
/// decodes to zero — which is exactly what an older build sends. This is what
/// keeps the ALPN bump reserved for changes that would be *misread*.
pub mod feature {
    /// The peer will serve chunks it holds without a prior offer.
    pub const UNSOLICITED_PROVIDE: u64 = 1 << 0;
    /// The peer understands residency hints.
    pub const RESIDENCY_HINTS: u64 = 1 << 1;
    /// The peer implements the lait-live generation-1 media vocabulary and
    /// serves the `MEDIA_GROUP`/`MEDIA_CONTROL` lane pair.
    pub const NATIVE_LIVE_MEDIA: u64 = 1 << 2;

    /// What *this* build actually implements.
    ///
    /// `RESIDENCY_HINTS` is here because there is now a `ResidencyOracle` that
    /// answers them; `UNSOLICITED_PROVIDE` is not, because nothing serves a
    /// chunk without being asked. Intersecting against this rather than against
    /// "every bit we have a name for" is the difference between advertising a
    /// capability and advertising a constant — a peer that acted on a bit we
    /// echoed back but do not honour would be right to be annoyed.
    ///
    /// A bit joins this constant in the same commit as the code that honours
    /// it, and never before.
    pub const LOCAL_SUPPORTED: u64 = RESIDENCY_HINTS | NATIVE_LIVE_MEDIA;
}

/// What a peer advertises about a plane it speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capability {
    pub plane: Plane,
    pub protocol_version: u16,
    #[serde(default)]
    pub features: u64,
}

/// The canonical bounded opening both planes start with.
///
/// **0.5-RTT.** The accepting side may begin writing before the client finishes
/// the handshake, and the client's initial bytes can be replayed by an attacker
/// who intercepts handshake packets. So accepting an opening must be idempotent:
/// a replay may not allocate a second session, consume a budget twice, or mint
/// state the first opening already minted. `connection_id` and `connection_epoch`
/// together are what make a replay recognisable, and no lane whose demand has
/// an effect may dispatch on 0.5-RTT data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Open {
    pub plane: Plane,
    pub protocol_version: u16,
    pub features: u64,
    pub space: [u8; SPACE_ID_LEN],
    pub initiator_station: [u8; 32],
    pub responder_station: [u8; 32],
    /// Random per session. With `connection_epoch`, what identifies a replay.
    pub connection_id: [u8; 16],
    /// Random per reconnect, so packets from an old session cannot outrank a
    /// new one.
    pub connection_epoch: [u8; 16],
    pub authority_frontier: Vec<u8>,
    pub requested_lanes: Vec<u8>,
}

/// The accepting side's answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Accept {
    pub connection_id: [u8; 16],
    pub connection_epoch: [u8; 16],
    pub capability: Capability,
    pub granted_lanes: Vec<u8>,
}

/// Why an opening was refused.
///
/// Deliberately coarse. A refusal that distinguished "not admitted" from "not
/// authorized for this lane" from "over budget" would tell an unadmitted peer
/// more about the Space than it should learn from being turned away.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Refusal {
    /// The opening did not parse, did not bind to this peer, or named another
    /// Space.
    Malformed,
    /// Not admitted, not authorized, or over budget.
    Refused,
    /// The protocol generation is one this build does not speak. Distinct
    /// because it is the one refusal a peer can act on.
    UnsupportedVersion { supported: u16 },
}

impl Refusal {
    pub fn encode(&self) -> Vec<u8> {
        postcard::to_stdvec(self).expect("postcard session refusal")
    }

    /// Decode a refusal, with the same discipline every other shape on this
    /// plane has.
    ///
    /// It matters more here than it looks. A refusal is the one message a peer
    /// reads when something has already gone wrong, so it is the message most
    /// likely to be read from a stream that is truncated, reset, or carrying
    /// something else entirely — and a decoder that guessed would turn "this
    /// peer is on another generation" into "this peer is unavailable", which is
    /// the difference between a fix and a mystery.
    pub fn decode_canonical(bytes: &[u8]) -> Result<Self, WireError> {
        if bytes.len() > bounds::MAX_OPENING_BYTES {
            return Err(WireError::TooLarge);
        }
        let refusal: Self = postcard::from_bytes(bytes).map_err(|_| WireError::NonCanonical)?;
        if refusal.encode() != bytes {
            return Err(WireError::NonCanonical);
        }
        Ok(refusal)
    }
}

/// The fixed rendered-SpaceId length.
pub const SPACE_ID_LEN: usize = 29;

/// Bounds. Every one is a pre-allocation ceiling: checked against a declared
/// length before a buffer is reserved, never after bytes have arrived.
pub mod bounds {
    /// Maximum encoded opening. Small and fixed — an opening carries
    /// identities and ids, not material.
    pub const MAX_OPENING_BYTES: usize = 4 * 1024;
    /// Maximum encoded control frame on the live plane's control stream.
    pub const MAX_CONTROL_FRAME_BYTES: usize = 64 * 1024;
    /// Maximum one reliable signal. The docket's hard ceiling.
    pub const MAX_SIGNAL_BYTES: usize = 16 * 1024;
    /// Maximum bytes read from a raw flow before the reader must act.
    ///
    /// `comms::MAX_FRAME` is 64 MiB, the framing guard for whole protocol
    /// messages on the existing `Stream`. A raw flow must **not** inherit it:
    /// a flow is read incrementally, so its ceiling bounds one read rather than
    /// one message, and 64 MiB of pre-allocation per flow is how a handful of
    /// concurrent transfers exhausts a receiver.
    pub const MAX_FLOW_READ_BYTES: usize = 256 * 1024;
    /// Maximum one transferred chunk plus its framing. Matches the frozen
    /// content geometry: a 256 KiB plaintext chunk, its envelope, and a proof.
    pub const MAX_CHUNK_FRAME_BYTES: usize = 320 * 1024;
    /// Maximum datagram payload lait will *attempt*.
    ///
    /// Advisory only, and not conservatively so: two runs of
    /// `comms::transport_capabilities` on the same machine and a direct path
    /// reported 1382 and then 1162, the second *below* this number. The real
    /// limit is the connection's current `max_datagram_size`, which moves with
    /// NAT traversal and relay fallback. Send through [`datagram_fits`].
    pub const MAX_DATAGRAM_BYTES: usize = 1_200;
    /// Maximum concurrent lanes one connection may request.
    pub const MAX_LANES: usize = 8;
    /// Maximum concurrent stream workers per connection. The permit is taken
    /// *before* work is spawned, not inside it, so a flood cannot outrun the
    /// cap by queueing tasks.
    pub const MAX_STREAM_WORKERS: usize = 32;
}

/// Whether a datagram of `payload_len` may be sent right now.
///
/// Both ceilings apply, and neither is reliably the smaller: lait's own bound
/// caps what any path is asked for, and `path_capacity` — the connection's
/// current `max_datagram_size` — is what the path will actually carry. A `None`
/// capacity means the peer negotiated no datagram support, which is a refusal
/// rather than an unlimited one.
///
/// The answer is never "truncate". Transient payloads have no retransmit, so a
/// half-delivered one arrives as corruption rather than as a gap.
pub fn datagram_fits(payload_len: usize, path_capacity: Option<usize>) -> bool {
    payload_len <= bounds::MAX_DATAGRAM_BYTES
        && path_capacity.is_some_and(|capacity| payload_len <= capacity)
}

/// Why a frame was refused before it was trusted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WireError {
    /// Longer than its pre-allocation ceiling.
    TooLarge,
    /// Did not decode, or re-encoding did not reproduce the input.
    NonCanonical,
    /// A protocol generation this build does not speak.
    UnsupportedVersion(u16),
    /// A stream kind this build does not implement.
    UnknownStreamKind(u8),
    /// A field outside its declared bound.
    Bounds,
}

impl std::fmt::Display for WireError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for WireError {}

impl Open {
    pub fn encode(&self) -> Vec<u8> {
        postcard::to_stdvec(self).expect("postcard session open")
    }

    /// Domain-separated commitment used by post-acceptance plane protocols.
    pub fn hash(&self) -> [u8; 32] {
        let mut hash = blake3::Hasher::new();
        hash.update(b"lait/plane/open/1");
        hash.update(&self.encode());
        *hash.finalize().as_bytes()
    }

    /// Decode an opening from a peer.
    ///
    /// Order is the contract: length before decode, canonical form before
    /// interpretation, version before anything semantic. Nothing here allocates
    /// past the ceiling, and nothing here trusts a field before the frame that
    /// carried it has been proven whole.
    pub fn decode_canonical(bytes: &[u8]) -> Result<Self, WireError> {
        if bytes.len() > bounds::MAX_OPENING_BYTES {
            return Err(WireError::TooLarge);
        }
        let open: Self = postcard::from_bytes(bytes).map_err(|_| WireError::NonCanonical)?;
        if open.encode() != bytes {
            return Err(WireError::NonCanonical);
        }
        if open.protocol_version != open.plane.protocol_version() {
            return Err(WireError::UnsupportedVersion(open.protocol_version));
        }
        // Half an opening, not a control frame. The outer length gate already
        // refuses anything past MAX_OPENING_BYTES, so a 64 KiB inner check could
        // never fire — a bound that cannot be reached is not a bound, and
        // reading one as though it were is how a ceiling gets quietly raised.
        if open.requested_lanes.len() > bounds::MAX_LANES
            || open.authority_frontier.len() > bounds::MAX_OPENING_BYTES / 2
        {
            return Err(WireError::Bounds);
        }
        // Deliberately *not* refused here. Rejecting an opening that names a
        // lane this build has not implemented would break the one thing feature
        // negotiation exists for: a newer peer asking for something extra must
        // get everything it asked for that we do have, not a closed door. The
        // grant decides — see `admission::judge`, which keeps only the lanes it
        // can serve — and a peer that then opens an ungranted flow is refused at
        // the flow, where the cost is a reset rather than a connection.
        Ok(open)
    }

    /// Whether `other` is a replay of this opening rather than a new session.
    pub fn is_replay_of(&self, other: &Self) -> bool {
        self.connection_id == other.connection_id && self.connection_epoch == other.connection_epoch
    }
}

impl Accept {
    pub fn encode(&self) -> Vec<u8> {
        postcard::to_stdvec(self).expect("postcard session accept")
    }

    pub fn decode_canonical(bytes: &[u8]) -> Result<Self, WireError> {
        if bytes.len() > bounds::MAX_OPENING_BYTES {
            return Err(WireError::TooLarge);
        }
        let accept: Self = postcard::from_bytes(bytes).map_err(|_| WireError::NonCanonical)?;
        if accept.encode() != bytes {
            return Err(WireError::NonCanonical);
        }
        if accept.granted_lanes.len() > bounds::MAX_LANES {
            return Err(WireError::Bounds);
        }
        Ok(accept)
    }
}

/// What Freight asks for and answers with.
///
/// Requests are exact. There is no "list what you have" and no remote path: a
/// peer asks for one chunk of one content it already knows the id of, and a
/// provider may refuse without saying whether authorization, policy, load,
/// absence, or incomplete proof material caused it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FreightFrame {
    /// Do you hold any of these chunks of this content?
    Have {
        content_id: [u8; 32],
        wanted: Vec<u32>,
    },
    /// Which of them are servable right now. A chunk counts only when its
    /// ciphertext *and* a validated proof are both resident.
    Available {
        content_id: [u8; 32],
        chunks: Vec<u32>,
    },
    /// Send me this chunk, from this offset.
    ///
    /// `resume_leaf` is the leaf hash a partial transfer already validated. A
    /// provider whose leaf differs is rejected before a byte is appended, so a
    /// resumed transfer cannot be steered onto different content.
    GetChunk {
        content_id: [u8; 32],
        chunk_index: u32,
        offset: u64,
        max_len: u32,
        resume_leaf: Option<[u8; 32]>,
    },
    /// Bounded leaf metadata and proof, ahead of the raw bytes.
    ChunkHeader {
        content_id: [u8; 32],
        chunk_index: u32,
        proof: Vec<u8>,
        total_len: u32,
    },
    /// No. Deliberately without a reason.
    Refused,
}

impl FreightFrame {
    pub fn encode(&self) -> Vec<u8> {
        postcard::to_stdvec(self).expect("postcard freight frame")
    }

    /// Encode, substituting a coarse refusal for anything that would exceed
    /// what the peer is allowed to read.
    ///
    /// A bound checked only on receive turns a local mistake into a remote
    /// protocol error: the peer refuses a frame we should never have written,
    /// and the failure is attributed to the wrong side. A provider that finds
    /// itself about to answer with something oversized says no instead — which
    /// is a legal answer, and the only one that keeps the refusal coarse.
    pub fn encode_bounded(&self) -> Vec<u8> {
        let bytes = self.encode();
        if self.validate().is_err() || bytes.len() > bounds::MAX_CONTROL_FRAME_BYTES {
            return FreightFrame::Refused.encode();
        }
        bytes
    }

    pub fn decode_canonical(bytes: &[u8]) -> Result<Self, WireError> {
        if bytes.len() > bounds::MAX_CONTROL_FRAME_BYTES {
            return Err(WireError::TooLarge);
        }
        let frame: Self = postcard::from_bytes(bytes).map_err(|_| WireError::NonCanonical)?;
        if frame.encode() != bytes {
            return Err(WireError::NonCanonical);
        }
        frame.validate()?;
        Ok(frame)
    }

    pub fn validate(&self) -> Result<(), WireError> {
        match self {
            FreightFrame::Have { wanted, .. } => {
                if wanted.len() > MAX_WANTED_CHUNKS {
                    return Err(WireError::Bounds);
                }
            }
            FreightFrame::Available { chunks, .. } => {
                if chunks.len() > MAX_WANTED_CHUNKS {
                    return Err(WireError::Bounds);
                }
            }
            FreightFrame::GetChunk { max_len, .. } => {
                if *max_len as usize > bounds::MAX_CHUNK_FRAME_BYTES {
                    return Err(WireError::Bounds);
                }
            }
            FreightFrame::ChunkHeader {
                proof, total_len, ..
            } => {
                if proof.len() > MAX_PROOF_BYTES
                    || *total_len as usize > bounds::MAX_CHUNK_FRAME_BYTES
                {
                    return Err(WireError::Bounds);
                }
            }
            FreightFrame::Refused => {}
        }
        Ok(())
    }
}

/// Maximum chunk indices one `Have`/`Available` may name.
pub const MAX_WANTED_CHUNKS: usize = 4096;
/// Maximum encoded proof sidecar.
///
/// The same number the resident cache accepts, and deliberately not a second
/// one: a sidecar that arrives inside the wire bound and then cannot be stored
/// would be a transfer that verifies and fails.
pub const MAX_PROOF_BYTES: usize = replica::content::MAX_PROOF_BYTES;
