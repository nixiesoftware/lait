//! Plan 14's two delivery planes, frozen.
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
/// One byte, and reserved values are never silently reused. `0x03` and `0x04`
/// belong to the media reservation (plan 14 §10) which this docket does not
/// build; keeping them allocated is what lets media arrive later without
/// disturbing anything here.
pub mod stream_kind {
    /// Long-lived bidirectional framed control.
    pub const CONTROL: u8 = 0x01;
    /// One bounded message per short stream.
    pub const RELIABLE_SIGNAL: u8 = 0x02;
    /// Reserved: one header plus raw frame bytes per unidirectional stream.
    pub const RESERVED_MEDIA_FRAME: u8 = 0x03;
    /// Reserved: bounded media control and feedback.
    pub const RESERVED_MEDIA_FEEDBACK: u8 = 0x04;

    /// Whether this build implements a kind. A reserved kind is known and
    /// unimplemented, which is a different answer from unknown — one resets
    /// the stream, the other is a peer speaking a protocol we agreed to.
    pub fn is_implemented(kind: u8) -> bool {
        matches!(kind, CONTROL | RELIABLE_SIGNAL)
    }

    pub fn is_reserved(kind: u8) -> bool {
        matches!(kind, RESERVED_MEDIA_FRAME | RESERVED_MEDIA_FEEDBACK)
    }
}

/// Which plane an opening is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Plane {
    Freight,
    Live,
}

impl Plane {
    pub fn alpn(self) -> &'static [u8] {
        match self {
            Plane::Freight => FREIGHT_ALPN,
            Plane::Live => LIVE_ALPN,
        }
    }

    pub fn protocol_version(self) -> u16 {
        match self {
            Plane::Freight => FREIGHT_PROTOCOL_VERSION,
            Plane::Live => LIVE_PROTOCOL_VERSION,
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
}

/// What a peer advertises about a plane it speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProtocolCapability {
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
/// state the first opening already minted. `session_id` and `session_epoch`
/// together are what make a replay recognisable, and no lane whose demand has
/// an effect may dispatch on 0.5-RTT data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionOpen {
    pub plane: Plane,
    pub protocol_version: u16,
    pub features: u64,
    pub space: [u8; SPACE_ID_LEN],
    pub initiator_station: [u8; 32],
    pub responder_station: [u8; 32],
    /// Random per session. With `session_epoch`, what identifies a replay.
    pub session_id: [u8; 16],
    /// Random per reconnect, so packets from an old session cannot outrank a
    /// new one.
    pub session_epoch: [u8; 16],
    pub authority_frontier: Vec<u8>,
    pub requested_lanes: Vec<u8>,
}

/// The accepting side's answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionAccept {
    pub session_id: [u8; 16],
    pub session_epoch: [u8; 16],
    pub capability: ProtocolCapability,
    pub granted_lanes: Vec<u8>,
}

/// Why an opening was refused.
///
/// Deliberately coarse. A refusal that distinguished "not admitted" from "not
/// authorized for this lane" from "over budget" would tell an unadmitted peer
/// more about the Space than it should learn from being turned away.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionRefusal {
    /// The opening did not parse, did not bind to this peer, or named another
    /// Space.
    Malformed,
    /// Not admitted, not authorized, or over budget.
    Refused,
    /// The protocol generation is one this build does not speak. Distinct
    /// because it is the one refusal a peer can act on.
    UnsupportedVersion { supported: u16 },
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
pub enum PlaneWireError {
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

impl std::fmt::Display for PlaneWireError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for PlaneWireError {}

impl SessionOpen {
    pub fn encode(&self) -> Vec<u8> {
        postcard::to_stdvec(self).expect("postcard session open")
    }

    /// Decode an opening from a peer.
    ///
    /// Order is the contract: length before decode, canonical form before
    /// interpretation, version before anything semantic. Nothing here allocates
    /// past the ceiling, and nothing here trusts a field before the frame that
    /// carried it has been proven whole.
    pub fn decode_canonical(bytes: &[u8]) -> Result<Self, PlaneWireError> {
        if bytes.len() > bounds::MAX_OPENING_BYTES {
            return Err(PlaneWireError::TooLarge);
        }
        let open: Self = postcard::from_bytes(bytes).map_err(|_| PlaneWireError::NonCanonical)?;
        if open.encode() != bytes {
            return Err(PlaneWireError::NonCanonical);
        }
        if open.protocol_version != open.plane.protocol_version() {
            return Err(PlaneWireError::UnsupportedVersion(open.protocol_version));
        }
        // Half an opening, not a control frame. The outer length gate already
        // refuses anything past MAX_OPENING_BYTES, so a 64 KiB inner check could
        // never fire — a bound that cannot be reached is not a bound, and
        // reading one as though it were is how a ceiling gets quietly raised.
        if open.requested_lanes.len() > bounds::MAX_LANES
            || open.authority_frontier.len() > bounds::MAX_OPENING_BYTES / 2
        {
            return Err(PlaneWireError::Bounds);
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
        self.session_id == other.session_id && self.session_epoch == other.session_epoch
    }
}

impl SessionAccept {
    pub fn encode(&self) -> Vec<u8> {
        postcard::to_stdvec(self).expect("postcard session accept")
    }

    pub fn decode_canonical(bytes: &[u8]) -> Result<Self, PlaneWireError> {
        if bytes.len() > bounds::MAX_OPENING_BYTES {
            return Err(PlaneWireError::TooLarge);
        }
        let accept: Self = postcard::from_bytes(bytes).map_err(|_| PlaneWireError::NonCanonical)?;
        if accept.encode() != bytes {
            return Err(PlaneWireError::NonCanonical);
        }
        if accept.granted_lanes.len() > bounds::MAX_LANES {
            return Err(PlaneWireError::Bounds);
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

    pub fn decode_canonical(bytes: &[u8]) -> Result<Self, PlaneWireError> {
        if bytes.len() > bounds::MAX_CONTROL_FRAME_BYTES {
            return Err(PlaneWireError::TooLarge);
        }
        let frame: Self = postcard::from_bytes(bytes).map_err(|_| PlaneWireError::NonCanonical)?;
        if frame.encode() != bytes {
            return Err(PlaneWireError::NonCanonical);
        }
        frame.validate()?;
        Ok(frame)
    }

    pub fn validate(&self) -> Result<(), PlaneWireError> {
        match self {
            FreightFrame::Have { wanted, .. } => {
                if wanted.len() > MAX_WANTED_CHUNKS {
                    return Err(PlaneWireError::Bounds);
                }
            }
            FreightFrame::Available { chunks, .. } => {
                if chunks.len() > MAX_WANTED_CHUNKS {
                    return Err(PlaneWireError::Bounds);
                }
            }
            FreightFrame::GetChunk { max_len, .. } => {
                if *max_len as usize > bounds::MAX_CHUNK_FRAME_BYTES {
                    return Err(PlaneWireError::Bounds);
                }
            }
            FreightFrame::ChunkHeader {
                proof, total_len, ..
            } => {
                if proof.len() > MAX_PROOF_BYTES
                    || *total_len as usize > bounds::MAX_CHUNK_FRAME_BYTES
                {
                    return Err(PlaneWireError::Bounds);
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
