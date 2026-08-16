//! Who a delivery-plane opening is from, and whether it is let in.
//!
//! One pure function and one bounded registry. Pure because the decision has to
//! be testable without a socket — the interesting cases are a replayed opening,
//! a claim that does not match the peer, and a lane nobody granted, none of
//! which need a network to exercise.
//!
//! **QUIC authenticates the peer; Mechanics decides what it may do.** The
//! transport proves that the bytes came from the holder of a key. It says
//! nothing about whether that key belongs to a member of this Space, and a
//! plane that treated a completed handshake as membership would admit every
//! stranger who could dial.
//!
//! **Refusals are coarse on purpose.** "Not admitted", "not authorized for this
//! lane", and "over budget" are one answer, because a peer that could tell them
//! apart could learn what a Space contains by being turned away from it in
//! different ways. The single exception is an unsupported generation, which is
//! the one refusal a peer can act on.

use std::collections::BTreeMap;
// `tokio::time::Instant`, not `tokio::time::Instant`. Without the `test-util`
// feature it IS `tokio::time::Instant::now()` — same call, same value, no
// indirection — so production pays nothing. With it, `tokio::time::pause()`
// stops the clock for every site at once, which is what lets a test drive a
// sweep interval or a probation window without waiting for one.
use std::time::Duration;
use tokio::time::Instant;

use mechanics::{ids::SpaceId, station::Key};
use replica::frontier::AuthorityFrontier;

use crate::plane::{stream_kind, Accept, Capability, Open, Plane, Refusal};
use crate::world::AuthorityView;

/// What the local side knows before it reads a word of the opening.
pub struct OpeningContext<'a> {
    /// The Space this route serves. An opening naming another one is not ours
    /// however well formed it is.
    pub space: &'a SpaceId,
    /// This Station's own identity, which the opening must name as responder.
    pub local_station: Key,
    /// The identity QUIC negotiated. The opening's initiator claim must equal
    /// it — a claim is a statement, and this is the thing that makes it true.
    pub peer: Key,
    /// The ALPN the connection was negotiated on, which fixes the plane.
    pub plane: Plane,
}

/// What an accepted peer turned out to be.
///
/// Not called a "standing": that word names the flat per-World grant list the
/// clean break removed, and reusing its vocabulary for a different idea is
/// exactly the confusion the gate that rejects it exists to prevent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdmittedPeer {
    pub station: Key,
    pub actor: mechanics::ids::ActorId,
    /// The frontier the admission decision was made at, pinned so every later
    /// question on this connection is answered against the same view.
    pub authority_frontier: AuthorityFrontier,
    pub granted_lanes: Vec<u8>,
    /// The session this connection is, and which reconnect of it.
    ///
    /// Carried because the driver is the only thing that sees them and the
    /// service is the only thing that needs them: the driver judges the
    /// opening, writes the accept, and then had nothing to hand on. A transient
    /// item's epoch is checked for equality against *this* one, so a plane
    /// service that could not see it could not tell a live datagram from one
    /// belonging to a session that has already reconnected.
    pub connection_id: [u8; 16],
    pub connection_epoch: [u8; 16],
    /// What both sides agreed on: the peer's offer intersected with
    /// `feature::LOCAL_SUPPORTED`.
    ///
    /// Carried rather than recomputed, because the intersection is the accept's
    /// own answer and recomputing it somewhere else is two answers that can
    /// disagree. A plane that honours a capability has to be able to tell
    /// whether this peer negotiated it.
    pub features: u64,
}

/// Local operator policy for a plane.
///
/// Separate from authority because it answers a different question. Authority
/// says whether this peer *may*; policy says whether this Station *will*. An
/// operator who does not want to serve bytes from a laptop on a metered link is
/// not making a statement about anyone's membership.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlanePolicy {
    /// Whether this Station answers requests for content it holds.
    pub serve_enabled: bool,
    /// Whether this Station fetches content it does not hold.
    pub fetch_enabled: bool,
    /// Whether this Station answers on the Live plane at all.
    ///
    /// An operator switch, not an authorization decision. A Station with no
    /// browser attached and no interest in other people's cursors should be
    /// able to say so without refusing them one at a time — and a plane that
    /// cannot be turned off is a plane whose cost an operator cannot decline.
    pub live_enabled: bool,
    /// Whether a file offered by one of this identity's own devices may land on
    /// disk without anyone clicking.
    ///
    /// Off unless an operator says otherwise, and it is the second of the three
    /// gates in the docket's auto-accept rule — the one the docket said had no
    /// representation anywhere. Policy rather than authority, because it answers
    /// "will this Station" and not "may that peer": somebody who does not want
    /// files appearing on a laptop is not making a claim about anyone's
    /// membership.
    pub auto_accept_offers: bool,
}

impl Default for PlanePolicy {
    fn default() -> Self {
        Self {
            serve_enabled: true,
            fetch_enabled: true,
            live_enabled: true,
            // The one switch here that defaults to off. Every other field
            // enables something a peer asked for; this one writes to a disk
            // nobody asked about at the moment it happens.
            auto_accept_offers: false,
        }
    }
}

/// The decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Admission {
    Accept(Box<Accept>, Box<AdmittedPeer>),
    Refuse(Refusal),
}

impl Admission {
    fn refuse() -> Self {
        Admission::Refuse(Refusal::Refused)
    }
}

/// Judge one opening.
///
/// The order is not incidental. Cheap structural checks that need no authority
/// lookup come first, so a malformed or misaddressed opening costs a comparison
/// rather than a resolution; the authority question — the expensive one, and
/// the one that touches shared state — is asked last and only about a peer that
/// has already proved it is talking to the right Station about the right Space.
pub fn judge(
    open: &Open,
    context: &OpeningContext<'_>,
    authority: &dyn AuthorityView,
    policy: &PlanePolicy,
) -> Admission {
    if open.plane != context.plane {
        // The ALPN already fixed the plane, so an opening that disagrees with
        // it is not confused — it is trying something.
        return Admission::Refuse(Refusal::Malformed);
    }
    if open.protocol_version != context.plane.protocol_version() {
        return Admission::Refuse(Refusal::UnsupportedVersion {
            supported: context.plane.protocol_version(),
        });
    }
    if open.space.as_slice() != context.space.as_str().as_bytes() {
        return Admission::Refuse(Refusal::Malformed);
    }
    if open.initiator_station != context.peer.key_bytes() {
        // A claim that does not match the negotiated peer. Malformed rather
        // than refused: this one is not about standing at all.
        return Admission::Refuse(Refusal::Malformed);
    }
    if open.responder_station != context.local_station.key_bytes() {
        return Admission::Refuse(Refusal::Malformed);
    }
    if !policy_admits(context.plane, policy) {
        return Admission::refuse();
    }

    let resolution = match context.plane {
        Plane::Contact => authority.admit_contact_peer(&context.peer),
        Plane::Freight | Plane::Live => authority.admit_peer(&context.peer),
    };
    let Some(resolution) = resolution else {
        return Admission::refuse();
    };

    // A lane is granted only if the plane serves lanes at all, this build
    // implements it, and the peer asked. Granting one nobody asked for would be
    // a lane with no owner; a peer that opens an ungranted flow is refused at
    // the flow rather than here.
    //
    // The plane check is the part that is easy to leave out. Freight reads no
    // stream-kind byte — the ALPN types the connection — so a granted lane there
    // is a promise nothing can keep: a peer taking it at its word would write a
    // kind byte that Freight's reader consumes as the first quarter of its
    // length prefix, and the flow desynchronises on the first frame.
    let accept_features = open.features & crate::plane::feature::LOCAL_SUPPORTED;
    let media_pair = accept_features & crate::plane::feature::NATIVE_LIVE_MEDIA != 0
        && open.requested_lanes.contains(&stream_kind::MEDIA_GROUP)
        && open.requested_lanes.contains(&stream_kind::MEDIA_CONTROL);
    let granted: Vec<u8> = if context.plane.serves_lanes() {
        open.requested_lanes
            .iter()
            .copied()
            .filter(|lane| {
                stream_kind::is_implemented(*lane) && (!stream_kind::is_media(*lane) || media_pair)
            })
            .collect()
    } else {
        Vec::new()
    };
    // Asking for lanes and getting none is a refusal — but only on a plane that
    // has lanes to give. On Freight an empty grant is the correct answer to any
    // request, and refusing there would turn a peer's harmless mistake into a
    // failed connection.
    if context.plane.serves_lanes() && granted.is_empty() && !open.requested_lanes.is_empty() {
        return Admission::refuse();
    }

    let accept = Accept {
        connection_id: open.connection_id,
        connection_epoch: open.connection_epoch,
        capability: Capability {
            plane: context.plane,
            protocol_version: context.plane.protocol_version(),
            // Bits both sides set: what the peer offered, intersected with
            // what this build implements. Echoing back a capability we merely
            // have a name for would be advertising a constant, and a peer
            // acting on it would be right to be annoyed.
            features: accept_features,
        },
        granted_lanes: granted.clone(),
    };
    Admission::Accept(
        Box::new(accept),
        Box::new(AdmittedPeer {
            station: context.peer.clone(),
            actor: resolution.actor,
            authority_frontier: resolution.authority_frontier,
            granted_lanes: granted,
            connection_id: open.connection_id,
            connection_epoch: open.connection_epoch,
            features: accept_features,
        }),
    )
}

fn policy_admits(plane: Plane, policy: &PlanePolicy) -> bool {
    match plane {
        // A Station that serves nothing still accepts a Freight connection: the
        // peer may be about to ask for something we would refuse anyway, and
        // refusing at the connection rather than the request tells it more.
        Plane::Freight => policy.serve_enabled || policy.fetch_enabled,
        Plane::Live => policy.live_enabled,
        Plane::Contact => true,
    }
}

/// How long an accepted opening is remembered for replay recognition.
///
/// Long enough to cover a handshake and the network's idea of "recently",
/// short enough that the table is not a place to put things. A replay arriving
/// after this is treated as a new session, which is safe: it will be judged
/// again from scratch and either admitted on its own merits or refused.
pub const ACCEPTED_OPENING_TTL: Duration = Duration::from_secs(120);

/// How many accepted openings are remembered at once.
pub const MAX_ACCEPTED_OPENINGS: usize = 2048;

/// The replay ledger.
///
/// 0.5-RTT data is replayable by anyone who can intercept handshake packets, so
/// accepting an opening has to be idempotent: a replay must return the answer
/// the first one got, allocate no second session, and consume no session budget
/// twice. `connection_id` and `connection_epoch` together are what make a replay
/// recognisable without any other state — a reconnect mints a new epoch and is
/// therefore a new session, which is the distinction that matters.
///
/// What identifies one opening: the peer, the session, and which reconnect.
///
/// A named tuple because it is the replay key — all three together, because a
/// peer that reconnects mints a new epoch and must not be answered from the
/// previous session's accept.
type OpeningKey = ([u8; 32], [u8; 16], [u8; 16]);

/// Bounded and swept, because it is a table keyed by remote input.
#[derive(Debug, Default)]
pub struct AcceptedOpenings {
    seen: BTreeMap<OpeningKey, (Accept, Instant)>,
}

/// What the ledger says about an opening.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Replay {
    /// Not seen. Judge it.
    Fresh,
    /// Seen. Return this answer again and mint nothing.
    Repeat(Box<Accept>),
}

impl AcceptedOpenings {
    fn key(open: &Open) -> ([u8; 32], [u8; 16], [u8; 16]) {
        (
            open.initiator_station,
            open.connection_id,
            open.connection_epoch,
        )
    }

    pub fn lookup(&mut self, open: &Open, now: Instant) -> Replay {
        self.sweep(now);
        match self.seen.get(&Self::key(open)) {
            Some((accept, _)) => Replay::Repeat(Box::new(accept.clone())),
            None => Replay::Fresh,
        }
    }

    /// Record an accept so a replay of the same opening gets the same answer.
    pub fn remember(&mut self, open: &Open, accept: &Accept, now: Instant) {
        self.sweep(now);
        if self.seen.len() >= MAX_ACCEPTED_OPENINGS {
            // Drop the oldest rather than refuse to record. Forgetting an
            // opening only costs a replay being judged afresh; refusing to
            // record would make the table useless exactly when it is under
            // pressure, which is when replays are most likely.
            if let Some(oldest) = self
                .seen
                .iter()
                .min_by_key(|(_, (_, at))| *at)
                .map(|(k, _)| *k)
            {
                self.seen.remove(&oldest);
            }
        }
        self.seen.insert(Self::key(open), (accept.clone(), now));
    }

    fn sweep(&mut self, now: Instant) {
        self.seen
            .retain(|_, (_, at)| now.duration_since(*at) < ACCEPTED_OPENING_TTL);
    }

    pub fn len(&self) -> usize {
        self.seen.len()
    }

    pub fn is_empty(&self) -> bool {
        self.seen.is_empty()
    }
}
