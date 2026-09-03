//! The admission vocabulary of a plane opening: which plane, under which
//! capability, opened and answered with which canonical frames — and the wire
//! bounds every one of them is checked against before a byte is trusted.
//! Extracted from the runtime's `plane` module so both halves of a Contact —
//! the daemon and a browser initiator — decode the same door the same way.

use serde::{Deserialize, Serialize};

use crate::{CONTACT_ALPN, CONTACT_PROTOCOL};

pub const FREIGHT_ALPN: &[u8] = b"lait/freight/1";
pub const LIVE_ALPN: &[u8] = b"lait/session/1";
pub const EXEC_ALPN: &[u8] = b"lait/exec/1";
pub const FREIGHT_PROTOCOL_VERSION: u16 = 1;
pub const LIVE_PROTOCOL_VERSION: u16 = 1;
pub const EXEC_PROTOCOL_VERSION: u16 = 1;

/// Which plane an opening is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Plane {
    Freight,
    Live,
    Contact,
    /// Appended to an existing encoding, exactly as `Contact` was, and safe
    /// for the same reason: a `Plane` value only ever rides the connection of
    /// its own ALPN, and an older peer that does not advertise `lait/exec/1`
    /// never completes the negotiation that would hand it this discriminant.
    /// The ALPN is the version gate — a mixed pair fails at the transport
    /// with no common protocol, the same legible refusal any unshared
    /// generation meets today. A peer that smuggles this discriminant into
    /// another plane's opening fails `judge`'s plane-matches-ALPN check and
    /// is refused as malformed.
    Exec,
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
            // Exec types its own control/output/input/Link flows in its own
            // vocabulary; the shared lane grant is Live's.
            Self::Exec => false,
        }
    }

    pub fn alpn(self) -> &'static [u8] {
        match self {
            Plane::Freight => FREIGHT_ALPN,
            Plane::Live => LIVE_ALPN,
            Plane::Contact => CONTACT_ALPN,
            Plane::Exec => EXEC_ALPN,
        }
    }

    pub fn protocol_version(self) -> u16 {
        match self {
            Plane::Freight => FREIGHT_PROTOCOL_VERSION,
            Plane::Live => LIVE_PROTOCOL_VERSION,
            Plane::Contact => CONTACT_PROTOCOL,
            Plane::Exec => EXEC_PROTOCOL_VERSION,
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
    /// The peer runs Contact symmetrically: after serving its own material it
    /// receives and incorporates the dialer's excess, so one dial converges
    /// BOTH sides. Without it a dialer that cannot be dialed back — a browser
    /// tab above all — never gets its writes out; with it, the tab pushes on
    /// the same connection it pulls on. Negotiated (not an ALPN bump) so an old
    /// peer that never sets it simply does today's one-way pull, no regression.
    pub const RECIPROCAL_CONVERGE: u64 = 1 << 3;
    /// The peer relays OTHER peers' presence, not only its own, and frames every
    /// presence datagram as a `RelayedPresence` carrying the origin station. A
    /// browser tab is a Live-plane client that nothing can dial back, so two tabs
    /// never see each other's carets unless a shared node they both dial fans
    /// presence out; this bit is how a tab asks a supporter to do that and how it
    /// learns each caret's true author. Negotiated (not an ALPN bump) so an old
    /// peer that never sets it gets today's own-presence-only, self-attributed
    /// behavior, no regression. Trust is unchanged: the origin is the station of
    /// the authenticated connection the supporter recorded the item on, never
    /// anything from a payload, so a peer still cannot speak for another.
    pub const PRESENCE_RELAY: u64 = 1 << 4;

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
    pub const LOCAL_SUPPORTED: u64 =
        RESIDENCY_HINTS | NATIVE_LIVE_MEDIA | RECIPROCAL_CONVERGE | PRESENCE_RELAY;
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
