#![allow(
    clippy::arithmetic_side_effects,
    clippy::as_conversions,
    reason = "signal frames and schema fields are bounded before fixed-width conversion"
)]
//! Reliable signals: bounded one-message events between peers.
//!
//! A signal is a thing that happened — somebody offered you a file, invited you
//! to collaborate, asked for your attention. It is reliable in the sense that it
//! is delivered or it fails loudly, and it is *not* durable in every other
//! sense: no journal entry, nothing replayed after a restart, and nothing that
//! becomes activity.
//!
//! That negative is the whole contract, and it is easy to break by accident.
//! One line — a `with_replica` here, a `publish` there — would journal nothing
//! and still emit an Observation, and `StationHost::frame_for` turns any
//! Observation carrying scopes into `activity_advanced`. So the enforcement is
//! structural: **this module may not name the Replica writer or the Observation
//! ring**, and `tests/signal_is_not_durable.rs` parses this file and fails if it
//! does.
//!
//! Privacy cannot do that job. `Broadcaster::publish` is `pub(crate)` and this
//! module is inside that crate; `StationCore::with_replica` is outright `pub`.
//! The gate is what makes the rule real, which is why it lands before the code
//! it guards.

use replica::body::WorldId;

use crate::budget::deadline;

/// Why a signal did not happen.
///
/// Local diagnostics, in this module rather than a shared error type — the same
/// shape `FreightError`, `FetchError`, `TransferError` and `ContentHostError`
/// all take. What crosses to a peer is deliberately coarser.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// No declaration for this selector. A signal nobody declared is one
    /// nothing knows how to bound, which is why it is refused rather than
    /// forwarded.
    NotRegistered,
    /// Authorization refused.
    Denied,
    /// Past the declaration's own ceiling.
    TooLarge,
    /// Did not decode, or decoded to something the declaration forbids.
    Malformed,
    /// The peer did not answer inside the budget.
    Deadline,
    /// This connection is sending signals faster than it may.
    OverBudget,
    /// The peer said no, in its own words.
    PeerRefused,
    /// This connection was not granted the signal lane.
    LaneNotGranted,
}

impl Refusal {
    /// The stable kebab-case code, for a client that needs to branch.
    ///
    /// A second function rather than an arm on `ErrorDto::code_for`, which is
    /// monomorphic on `Rejection` — a signal failure is not a World failure and
    /// widening that function would make it one.
    pub fn code(&self) -> &'static str {
        match self {
            Self::NotRegistered => "signal-not-registered",
            Self::Denied => "signal-denied",
            Self::TooLarge => "signal-too-large",
            Self::Malformed => "signal-malformed",
            Self::Deadline => "signal-deadline",
            Self::OverBudget => "signal-over-budget",
            Self::PeerRefused => "signal-peer-refused",
            Self::LaneNotGranted => "signal-lane-not-granted",
        }
    }
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}

impl std::error::Error for Refusal {}

/// Whether a signal may be answered, and how.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResponsePolicy {
    /// One-way. The sender learns only that it was delivered.
    ///
    /// Most signals are this. An answer is a second round trip and a second
    /// deadline, and a signal that does not need one should not pay for one.
    Forbidden,
    /// The receiver acknowledges, within the response deadline.
    Acknowledge,
}

/// What a peer must satisfy to send this signal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignalDemand {
    /// Being admitted to the session is enough. For signals that say nothing
    /// about any particular thing — a liveness ping.
    Session,
    /// A World's own demand, evaluated by mechanics at the pinned frontier.
    World { world: WorldId, demand: Vec<u8> },
}

/// Everything the substrate needs to know about one kind of signal.
///
/// A declaration is what makes a signal bounded, authorized, and answerable.
/// Nothing is sent or accepted without one — an undeclared signal is one
/// nothing knows how large it may be or who may send it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignalDeclaration {
    /// The wire discriminant. Distinct across every declaration, and never
    /// reused once published.
    pub selector: u16,
    /// This signal's own ceiling, at or under the plane's.
    pub max_bytes: usize,
    pub demand: SignalDemand,
    pub response: ResponsePolicy,
}

/// The selectors the substrate itself owns.
///
/// A World's signals are registered separately and may not collide with these.
pub mod selector {
    pub const PING: u16 = 0x0001;
    pub const ACKNOWLEDGE: u16 = 0x0002;
    pub const ATTENTION: u16 = 0x0003;
    pub const SESSION_INVITE: u16 = 0x0004;
    pub const FILE_OFFER: u16 = 0x0005;
    /// A World's own signal, discriminated further inside the payload.
    pub const WORLD: u16 = 0x0006;
}

/// Every declaration the substrate ships.
///
/// Written as an exhaustive list rather than a lookup built at runtime, so a
/// selector added without a declaration is a compile error here rather than a
/// `NotRegistered` somebody meets in the field.
pub fn core_declarations() -> Vec<SignalDeclaration> {
    vec![
        SignalDeclaration {
            selector: selector::PING,
            max_bytes: 64,
            demand: SignalDemand::Session,
            // The one signal that exists to be answered.
            response: ResponsePolicy::Acknowledge,
        },
        SignalDeclaration {
            selector: selector::ACKNOWLEDGE,
            max_bytes: 64,
            demand: SignalDemand::Session,
            // Answering an answer is how a ping becomes a loop.
            response: ResponsePolicy::Forbidden,
        },
        SignalDeclaration {
            selector: selector::ATTENTION,
            max_bytes: 1024,
            demand: SignalDemand::Session,
            response: ResponsePolicy::Forbidden,
        },
        SignalDeclaration {
            selector: selector::SESSION_INVITE,
            max_bytes: 2048,
            demand: SignalDemand::Session,
            response: ResponsePolicy::Forbidden,
        },
        SignalDeclaration {
            selector: selector::FILE_OFFER,
            max_bytes: 4096,
            demand: SignalDemand::Session,
            // An offer is not an acceptance. Whether the receiver wants the
            // file is a decision a person makes later, not a protocol answer
            // due inside a deadline.
            response: ResponsePolicy::Forbidden,
        },
        SignalDeclaration {
            selector: selector::WORLD,
            max_bytes: crate::plane::bounds::MAX_SIGNAL_BYTES,
            demand: SignalDemand::Session,
            response: ResponsePolicy::Forbidden,
        },
    ]
}

/// Look up a declaration. `None` is `NotRegistered`, and refusing is correct:
/// a signal nobody declared is one nothing knows how to bound.
pub fn declaration_for(selector: u16) -> Option<SignalDeclaration> {
    core_declarations()
        .into_iter()
        .find(|declaration| declaration.selector == selector)
}

/// Read one reliable signal off its flow.
///
/// The wire is `stream_kind | u16 selector | u32 length | canonical body`, and
/// the **selector precedes the length** for one reason: a declaration's
/// `max_bytes` is only a pre-allocation ceiling if it is known before the
/// length is. Behind the length, the schema is known only after a buffer
/// already exists, and the per-signal maximum becomes decoration.
///
/// `max_bytes` is floored against the plane's own ceiling, so a bad declaration
/// table can lower the limit and never raise it.
///
/// The stream kind is read by the caller, which is what decides this is a
/// signal flow at all.
pub async fn read_signal(flow: &mut dyn comms::RecvFlow) -> Result<crate::plane::Signal, Refusal> {
    let selector = flow.read_exact(2).await.map_err(|_| Refusal::Malformed)?;
    let selector = u16::from_le_bytes(
        <[u8; 2]>::try_from(selector.as_slice()).map_err(|_| Refusal::Malformed)?,
    );
    // Resolved before the length is read. An unknown selector is refused here,
    // with nothing allocated and no length even consulted.
    let declaration = declaration_for(selector).ok_or(Refusal::NotRegistered)?;
    let ceiling = declaration
        .max_bytes
        .min(crate::plane::bounds::MAX_SIGNAL_BYTES);

    let header = flow.read_exact(4).await.map_err(|_| Refusal::Malformed)?;
    let len =
        u32::from_le_bytes(<[u8; 4]>::try_from(header.as_slice()).map_err(|_| Refusal::Malformed)?)
            as usize;
    if len > ceiling {
        // Refused by the declared length, before a buffer that size exists.
        return Err(Refusal::TooLarge);
    }
    let body = flow.read_exact(len).await.map_err(|_| Refusal::Malformed)?;
    let signal = crate::plane::Signal::decode_canonical(&body).map_err(|_| Refusal::Malformed)?;
    // The body decoded, and it has to be the signal the selector promised —
    // otherwise a small declaration's ceiling could be used to smuggle a
    // different, larger-bounded shape past it.
    if signal.selector() != selector {
        return Err(Refusal::Malformed);
    }
    Ok(signal)
}

/// Frame one signal for sending, or refuse.
///
/// A `Result` rather than a substituted refusal. `FreightFrame::encode_bounded`
/// substitutes because the alternative there is telling a peer nothing at all;
/// here the alternative is sending something other than what was asked for,
/// which is worse than an error the caller can see.
pub fn frame_signal(signal: &crate::plane::Signal) -> Result<Vec<u8>, Refusal> {
    let selector = signal.selector();
    let declaration = declaration_for(selector).ok_or(Refusal::NotRegistered)?;
    // A bounds failure is a size failure and says so. Collapsing it into
    // `Malformed` would tell a caller its signal was ill-formed when what it
    // actually was is too big — two different things to do about it.
    signal.validate().map_err(|error| match error {
        crate::plane::WireError::Bounds => Refusal::TooLarge,
        _ => Refusal::Malformed,
    })?;
    let body = signal.encode();
    if body.len()
        > declaration
            .max_bytes
            .min(crate::plane::bounds::MAX_SIGNAL_BYTES)
    {
        return Err(Refusal::TooLarge);
    }
    let mut out = Vec::with_capacity(1 + 2 + 4 + body.len());
    out.push(crate::plane::stream_kind::RELIABLE_SIGNAL);
    out.extend_from_slice(&selector.to_le_bytes());
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(&body);
    Ok(out)
}

/// Frame one signal *without* the leading stream kind.
///
/// What an answer looks like. The kind byte declares what a flow is, and a flow
/// is opened once — so the opener writes it and the responder does not. Writing
/// it on the way back shifts every subsequent field by one and the selector is
/// then read out of the kind byte and half of itself, which decodes as a
/// plausible unregistered selector rather than as an error anyone can read.
pub fn frame_answer(signal: &crate::plane::Signal) -> Result<Vec<u8>, Refusal> {
    let mut framed = frame_signal(signal)?;
    framed.remove(0);
    Ok(framed)
}

/// The coarse close code every signal refusal uses.
///
/// One code for every reason, like the rest of the delivery planes: a peer
/// learns it was refused, never which check refused it. `Refusal` is the
/// local diagnosis and stays local.
const REFUSED: u32 = 1;

/// Refuse a flow, on both halves.
///
/// **The pair is the point.** Resetting the send half alone does not stop an
/// inbound writer: the peer keeps writing into a flow nobody is reading, and a
/// refused peer can still drain a full `MAX_SIGNAL_BYTES` past a refusal that
/// already happened. `stop` is what tells the sender to stop sending, so the
/// refusal costs the refused rather than the refuser.
pub fn refuse_flow(send: &mut dyn comms::SendFlow, recv: &mut dyn comms::RecvFlow) {
    recv.stop(REFUSED);
    send.reset(REFUSED);
}

/// One delivered signal, as a listener sees it.
///
/// Carries the session it arrived on. A listener that reconnected between
/// hearing about a thing and acting on it can tell — two epochs are compared,
/// never ordered, so the only answerable question is whether this is the
/// session that is still open.
#[derive(Debug, Clone, PartialEq)]
pub struct DeliveredSignal {
    pub from: mechanics::station::Key,
    pub connection_id: [u8; 16],
    pub connection_epoch: [u8; 16],
    pub signal: crate::plane::Signal,
}

/// What a peer may say on this connection's signal lane, and who is asking.
///
/// Owned and cloneable rather than borrowed, with no lifetime parameter, so it
/// survives being handed to a per-flow task.
///
/// **It holds an `Arc<dyn AuthorityView>` and nothing that can commit.** Every
/// authority question a signal asks already exists on that trait, so there is
/// no reason to reach further — and reaching further is precisely what
/// `tests/signal_is_not_durable.rs` fails the build over.
#[derive(Clone)]
pub struct SignalPolicy {
    pub peer: mechanics::station::Key,
    pub actor: mechanics::ids::ActorId,
    /// The frontier this session was admitted at. Pinned, so every question on
    /// this connection is answered against one view.
    pub frontier: replica::frontier::AuthorityFrontier,
    pub granted_lanes: Vec<u8>,
    pub authority: std::sync::Arc<dyn crate::world::AuthorityView>,
    pub worlds: crate::registry::Catalog,
}

impl SignalPolicy {
    /// Whether this connection holds the signal lane at all.
    pub fn holds_lane(&self) -> bool {
        self.granted_lanes
            .contains(&crate::plane::stream_kind::RELIABLE_SIGNAL)
    }

    /// Whether this peer may send this signal.
    ///
    /// A `Session` demand is satisfied by being here: the connection was
    /// admitted, and a liveness ping says nothing about any particular thing. A
    /// `World` demand is evaluated by Mechanics at the pinned frontier, which is
    /// the same evaluation a read of that World would get.
    pub fn permits(&self, declaration: &SignalDeclaration) -> Result<(), Refusal> {
        match &declaration.demand {
            SignalDemand::Session => Ok(()),
            SignalDemand::World { world, demand } => {
                self.world_is_live(world)?;
                // A could-not-evaluate error refuses like a denial at this
                // peer boundary: an unevaluable demand must not admit.
                match self
                    .authority
                    .evaluate_read(&self.actor, &self.frontier, demand)
                {
                    Ok(true) => Ok(()),
                    Ok(false) | Err(_) => Err(Refusal::Denied),
                }
            }
        }
    }

    /// Whether this build can interpret a signal's own contents, and whether
    /// this peer may have sent them.
    ///
    /// Distinct from `permits`, which asks about the *substrate's* declaration.
    /// A `WorldSignal` names its World and its schema inside the payload, so
    /// neither the World's registered ceiling nor its demand can be consulted
    /// until the body is decoded — which is why the substrate's own declaration
    /// for `selector::WORLD` is deliberately permissive and this is where the
    /// real bound lives.
    ///
    /// Without this the descriptor's signal section would be decoration: a
    /// World could declare a 64-byte nudge requiring a capability, and a peer
    /// could send sixteen kilobytes of it with nothing but session membership.
    pub fn admits_contents(&self, signal: &crate::plane::Signal) -> Result<(), Refusal> {
        let crate::plane::Signal::WorldSignal {
            world,
            schema,
            payload,
        } = signal
        else {
            return Ok(());
        };
        let world_id = replica::body::WorldId::parse(world).ok_or(Refusal::Malformed)?;
        self.world_is_live(&world_id)?;

        let schema = replica::body::SchemaId::parse(schema).ok_or(Refusal::Malformed)?;
        let registration = self
            .worlds
            .descriptor(&world_id)
            .ok_or(Refusal::NotRegistered)?;
        // A schema this World never declared is not registered, whatever the
        // World would do with it. Reaching a World's `submit` with an
        // undeclared schema is exactly what the reviewed descriptor exists to
        // prevent.
        let declared = registration
            .signal_schemas
            .iter()
            .find(|candidate| candidate.name == schema)
            .ok_or(Refusal::NotRegistered)?;

        if payload.len() > declared.max_payload_bytes as usize {
            return Err(Refusal::TooLarge);
        }
        // The World's own demand, evaluated by Mechanics at the pinned frontier
        // — the same evaluation a read of that World would get.
        //
        // Not defended against being empty: `Builder::build` parses every
        // declared demand through `AuthorizationDemand::decode_canonical`, which
        // refuses empty input, so a registered signal schema always states one.
        // A fallback here would be a second, more permissive answer to a
        // question registration has already refused to leave open.
        // As in `permits`: an unevaluable demand refuses like a denial here.
        match self
            .authority
            .evaluate_read(&self.actor, &self.frontier, &declared.demand)
        {
            Ok(true) => Ok(()),
            Ok(false) | Err(_) => Err(Refusal::Denied),
        }
    }

    /// A World this build hosts, with an implementation active at the pinned
    /// frontier.
    ///
    /// Both halves, and both are `NotRegistered` rather than `Denied`. A World
    /// we do not host and one whose implementation nobody approved are the same
    /// answer to a peer: this build cannot interpret that, and interpreting it
    /// anyway is how a schema nobody reviewed gets acted on.
    fn world_is_live(&self, world: &replica::body::WorldId) -> Result<(), Refusal> {
        if !self.worlds.contains(world) {
            return Err(Refusal::NotRegistered);
        }
        // An unanswerable ledger refuses like a missing activation at this
        // peer boundary: this build cannot vouch for the interpretation.
        match self.authority.active_implementation(world, &self.frontier) {
            Ok(Some(_)) => Ok(()),
            Ok(None) | Err(_) => Err(Refusal::NotRegistered),
        }
    }
}

/// What happened to a signal this Station sent.
#[derive(Debug, Clone, PartialEq)]
pub enum SignalOutcome {
    /// The plane took it: framed, bounded, written and finished.
    ///
    /// **Not a delivery receipt.** It says the bytes left; it says nothing
    /// about whether a person saw them, and a caller that treats it as
    /// confirmation is building a read receipt out of a transport ack.
    Accepted,
    /// The peer answered, for a declaration whose response policy allows one.
    Answered(crate::plane::Signal),
}

/// Send one signal on its own flow.
///
/// **Every signal rides a bidirectional flow, whatever its response policy.**
/// Signals keep one flow shape regardless of response policy: the lane handler
/// can answer a Ping on the same flow, while a one-way signal simply ignores
/// the receive half. Uni streams on this connection belong to media Groups.
///
/// So the response policy governs what it is actually about — whether an answer
/// is read and a second deadline is spent — and not which flow is opened. A
/// caller still cannot choose to wait for an answer to something nobody
/// promised to answer.
pub async fn send_signal(
    connection: &dyn comms::Connection,
    signal: &crate::plane::Signal,
) -> Result<SignalOutcome, Refusal> {
    let framed = frame_signal(signal)?;
    let declaration = declaration_for(signal.selector()).ok_or(Refusal::NotRegistered)?;

    let (mut send, mut recv) = tokio::time::timeout(deadline::SIGNAL_OPEN, connection.open_bi())
        .await
        .map_err(|_| Refusal::Deadline)?
        .map_err(|_| Refusal::PeerRefused)?;
    // Written and finished before anything is read. A flow does not exist for
    // the peer until the opener writes to it, so accepting first and waiting is
    // a hang on both transports rather than a failed assertion.
    write_and_finish(send.as_mut(), &framed).await?;

    match declaration.response {
        ResponsePolicy::Forbidden => {
            // Nothing is coming, so nothing is waited for. The read half is
            // stopped rather than dropped: dropping it leaves the peer able to
            // write into a flow this side will never read.
            recv.stop(REFUSED);
            Ok(SignalOutcome::Accepted)
        }
        ResponsePolicy::Acknowledge => {
            let answered =
                tokio::time::timeout(deadline::SIGNAL_RESPONSE, read_signal(recv.as_mut()))
                    .await
                    .map_err(|_| Refusal::Deadline)?;
            match answered {
                Ok(answer) => Ok(SignalOutcome::Answered(answer)),
                Err(error) => {
                    recv.stop(REFUSED);
                    Err(error)
                }
            }
        }
    }
}

async fn write_and_finish(send: &mut dyn comms::SendFlow, framed: &[u8]) -> Result<(), Refusal> {
    tokio::time::timeout(deadline::SIGNAL_WRITE, send.write_all(framed))
        .await
        .map_err(|_| Refusal::Deadline)?
        .map_err(|_| Refusal::PeerRefused)?;
    send.finish().map_err(|_| Refusal::PeerRefused)
}

/// Serve one inbound signal flow, from the stream kind onwards.
///
/// The caller has read the kind byte and decided this is a signal flow. What is
/// left is the lane, the declaration, the body, and whether this peer may have
/// sent it — in that order, because each one bounds the next.
pub async fn serve_signal(
    send: &mut dyn comms::SendFlow,
    recv: &mut dyn comms::RecvFlow,
    policy: &SignalPolicy,
) -> Result<crate::plane::Signal, Refusal> {
    if !policy.holds_lane() {
        refuse_flow(send, recv);
        return Err(Refusal::LaneNotGranted);
    }
    let signal = match tokio::time::timeout(deadline::SIGNAL_READ, read_signal(recv)).await {
        Ok(Ok(signal)) => signal,
        Ok(Err(error)) => {
            refuse_flow(send, recv);
            return Err(error);
        }
        Err(_) => {
            refuse_flow(send, recv);
            return Err(Refusal::Deadline);
        }
    };

    let declaration = declaration_for(signal.selector()).ok_or(Refusal::NotRegistered)?;
    if let Err(error) = policy.permits(&declaration) {
        refuse_flow(send, recv);
        return Err(error);
    }
    if let Err(error) = policy.admits_contents(&signal) {
        refuse_flow(send, recv);
        return Err(error);
    }

    match declaration.response {
        ResponsePolicy::Forbidden => {
            // Nothing to say back. Finished rather than reset: a reset after a
            // successful read tells a peer its signal failed.
            let _ = send.finish();
        }
        // `frame_answer`, not `frame_signal`: the flow's kind was fixed when
        // the opener wrote it, and repeating it here would shift every field
        // the reader is about to parse.
        ResponsePolicy::Acknowledge => match acknowledgement(&signal).as_ref().map(frame_answer) {
            Some(Ok(framed)) => {
                let _ = write_and_finish(send, &framed).await;
            }
            _ => {
                let _ = send.finish();
            }
        },
    }
    Ok(signal)
}

/// The answer to a signal that expects one.
///
/// Only `Ping` does, and the nonce comes back unchanged: that is what makes the
/// answer an answer to *this* ping rather than to any ping. `Acknowledge` is
/// itself `Forbidden`, which is how a ping does not become a loop.
fn acknowledgement(signal: &crate::plane::Signal) -> Option<crate::plane::Signal> {
    match signal {
        crate::plane::Signal::Ping { nonce } => {
            Some(crate::plane::Signal::Acknowledge { nonce: *nonce })
        }
        _ => None,
    }
}

/// How many unanswered offers this Station will hold.
///
/// Person-scale. An offer waits for somebody to decide about it, and a hundred
/// of them waiting is not a backlog anyone works through — it is a Station being
/// used as somebody else's queue.
pub const MAX_PENDING_OFFERS: usize = 32;

/// A file somebody offered, waiting for a decision.
///
/// Holding an offer costs a name and a content id. It does **not** cost the
/// file: receiving one starts no transfer and touches no filesystem, because
/// whether the receiver wants a gigabyte is a decision a person makes and not a
/// protocol answer due inside a deadline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingOffer {
    pub from: mechanics::station::Key,
    /// The session it arrived on. An offer from a session that has since
    /// reconnected is not stale — the file is still there — but a caller
    /// answering it has to dial again rather than reply on a connection that is
    /// gone.
    pub connection_epoch: [u8; 16],
    pub content: [u8; 32],
    pub plaintext_len: u64,
    /// What the sender calls it. Peer-supplied, never sanitised here: a name is
    /// sanitised at the point it becomes a path, and rewriting it on arrival
    /// would mean the thing shown to a person is not the thing that was sent.
    pub display_name: String,
    pub media_type: String,
}

/// Why an offer was not queued.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OfferOutcome {
    /// Held, waiting for a decision.
    Queued,
    /// Already here. An offer is identified by its content and its sender, so a
    /// peer repeating itself does not fill the queue.
    Duplicate,
    /// The queue is full.
    ///
    /// The **newest** is refused rather than the oldest evicted. This is an
    /// inbox: what is already in it may be about to be acted on, and silently
    /// dropping that to make room for something newer loses the decision
    /// somebody was in the middle of making.
    Full,
}

/// The bounded set of offers nobody has answered.
#[derive(Debug, Default)]
pub struct OfferQueue {
    pending: Vec<PendingOffer>,
    refused: u64,
}

impl OfferQueue {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn admit(&mut self, offer: PendingOffer) -> OfferOutcome {
        if self
            .pending
            .iter()
            .any(|held| held.content == offer.content && held.from == offer.from)
        {
            return OfferOutcome::Duplicate;
        }
        if self.pending.len() >= MAX_PENDING_OFFERS {
            self.refused = self.refused.saturating_add(1);
            return OfferOutcome::Full;
        }
        self.pending.push(offer);
        OfferOutcome::Queued
    }

    pub fn pending(&self) -> &[PendingOffer] {
        &self.pending
    }

    /// How many were refused for want of room. Not reported to any sender:
    /// `Accepted` says the bytes arrived and nothing about what happened next,
    /// and a peer that could tell a full queue from an empty one would learn
    /// whether anyone is using this Station.
    pub fn refused(&self) -> u64 {
        self.refused
    }

    /// Take one offer out, by content and sender.
    pub fn take(
        &mut self,
        from: &mechanics::station::Key,
        content: &[u8; 32],
    ) -> Option<PendingOffer> {
        let at = self
            .pending
            .iter()
            .position(|held| &held.content == content && &held.from == from)?;
        Some(self.pending.remove(at))
    }

    /// Drop everything one peer offered. What a revocation does — a file offered
    /// by somebody who is no longer a member is not an offer anyone should be
    /// shown a button for.
    pub fn forget(&mut self, from: &mechanics::station::Key) -> usize {
        let before = self.pending.len();
        self.pending.retain(|held| &held.from != from);
        before - self.pending.len()
    }
}

/// What this layer can decide about taking an offer without asking a person.
///
/// §7.3 names three gates. Two of them are answerable here and the third is
/// not, and saying so in the type is better than pretending otherwise: the only
/// thing that can resolve a local destination is `LocalDestination`, which lives
/// in `world-interface` — a crate that depends on this one. Answering gate three
/// here would mean inverting that edge and pulling a CLI's dependencies into the
/// engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OfferGates {
    /// Both gates this layer owns are open. The caller still has to resolve a
    /// destination, which is the third and is not this crate's question.
    DestinationRemains,
    /// The sender is not one of this identity's own devices.
    ///
    /// The strictest of the three, and the reason automatic acceptance is
    /// defensible at all: a file that lands on disk without anyone clicking
    /// came from another device belonging to the same person.
    NotOurDevice,
    /// This Station has not opted this Space into automatic acceptance.
    SpaceNotOptedIn,
}

/// Evaluate the two gates that belong to this layer.
///
/// `ours` is the actor this Station acts as. An offer auto-accepts only from a
/// device resolving to that same actor — which is what makes it a file moving
/// between somebody's own machines rather than a stranger writing to their disk.
pub fn offer_gates(
    from: &crate::admission::AdmittedPeer,
    ours: &mechanics::ids::ActorId,
    policy: &crate::admission::PlanePolicy,
) -> OfferGates {
    if &from.actor != ours {
        return OfferGates::NotOurDevice;
    }
    if !policy.auto_accept_offers {
        return OfferGates::SpaceNotOptedIn;
    }
    OfferGates::DestinationRemains
}
