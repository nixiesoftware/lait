//! The Live plane: one session, and what a peer may make it hold.
//!
//! Freight moves bytes somebody asked for. This moves what people are *doing* —
//! where a cursor is, who is looking at an issue, who is typing — and the whole
//! difference is that none of it is worth retransmitting. A caret that arrives
//! late is wrong, not delayed, so it goes on a datagram and the next one
//! supersedes it.
//!
//! **Flow kinds are not mixed on one connection after the opening.** MemNet
//! has one handoff queue for both uni and bi flows, and `accept_bi` errors when
//! the next handoff is a uni flow — which the accept loop reads as end of
//! connection. So this plane accepts bidirectional flows only: the control
//! stream is bidirectional, subscriptions arrive on it, signal flows are
//! bidirectional whether or not they expect an answer, and everything else is a
//! datagram. That is a constraint the transport imposes rather than a
//! preference, and it is written here rather than discovered in a test — which
//! is the whole reason it is worth writing down, because a one-way signal on a
//! unidirectional flow succeeds locally, reports success, and reaches nobody.
//!
//! Three things are bounded before anything is spent, and the order is the
//! bound: a permit before the stream kind is read, the gate before the message,
//! the message's declared length before its buffer.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Instant;

use mechanics::ids::StationId;

use crate::admission::AdmittedPeer;
use crate::budget::{deadline, gates, slots, ByteGate, Gate, Verdict};
use crate::plane_stream::{read_framed, read_stream_kind, StreamError};
use crate::planes::{bounds, datagram_fits, stream_kind};
use crate::transient::{
    AdmitOutcome, LiveControl, TransientItem, TransientScope, TransientStore,
    MAX_TRANSIENT_ITEM_BYTES,
};

/// The close code every Live refusal uses. Coarse on purpose: a peer learns it
/// was refused, never which check refused it.
const REFUSED: u32 = 1;

/// What this session has dropped, and why.
///
/// "Dropped and counted" needs something to read, or it is just "dropped". None
/// of these are errors — every one is a bound doing its job — but an operator
/// looking at a Station that feels wrong needs to know which bound is being
/// hit, and a peer that is hitting one constantly is a peer with a bug.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct TransientCounters {
    /// Items past the per-item ceiling, refused before decoding.
    pub oversize_items: u64,
    /// Anchors past their own ceiling, refused before resolving.
    pub oversize_anchors: u64,
    /// Items refused because the table was full.
    pub evictions: u64,
    /// Payloads that did not fit the path's datagram capacity.
    ///
    /// Never truncated — a transient payload has no retransmit, so half of one
    /// arrives as corruption rather than as a gap.
    pub capacity_drops: u64,
    /// Items superseded before they were sent, which is coalescing working.
    ///
    /// Written by nothing yet: `Coalescer` keeps its own count, and the send
    /// side that would join the two has no production caller. Kept here rather
    /// than deleted because the field is where the number belongs the moment
    /// something publishes, and its absence is recorded on `Coalescer` itself.
    pub supersessions: u64,
    /// Items from a session epoch this connection is not admitted at.
    pub wrong_epoch: u64,
    /// Items at or below a retirement watermark.
    pub retired: u64,
}

/// What one peer is currently telling this Station.
pub struct LiveSession {
    peer: StationId,
    store: TransientStore,
    counters: TransientCounters,
    /// The scopes this connection asked to hear about.
    ///
    /// Replace-all rather than incremental: a subscription is a snapshot of
    /// what a client is looking at, and a client that adds and removes views
    /// faster than its messages arrive would otherwise end up subscribed to a
    /// set neither side agrees on.
    subscriptions: Vec<TransientScope>,
}

impl LiveSession {
    pub fn new(peer: StationId) -> Self {
        // The *per-connection* ceiling, which is what this table is. Sizing it
        // at `MAX_TRANSIENT_SLOTS` — the Station-wide number — gave every
        // connection the whole Station's budget and left
        // `MAX_SLOTS_PER_CONNECTION` with no reader at all, so the bound whose
        // derivation the legality table exists to justify bounded nothing.
        Self::with_capacity(peer, slots::MAX_SLOTS_PER_CONNECTION)
    }

    /// A session whose table holds `capacity` slots.
    ///
    /// Exposed so a flood can be driven against a table small enough to fill.
    /// Proving the eviction escalation at the shipped ceiling means sending four
    /// thousand distinct scopes through a real connection, and a bound nobody
    /// can afford to test is a bound nobody tests.
    pub fn with_capacity(peer: StationId, capacity: usize) -> Self {
        Self {
            peer,
            store: TransientStore::with_capacity(capacity),
            counters: TransientCounters::default(),
            subscriptions: Vec::new(),
        }
    }

    pub fn peer(&self) -> &StationId {
        &self.peer
    }

    pub fn counters(&self) -> &TransientCounters {
        &self.counters
    }

    pub fn subscriptions(&self) -> &[TransientScope] {
        &self.subscriptions
    }

    /// Adopt a subscription snapshot.
    pub fn subscribe(&mut self, scopes: Vec<TransientScope>) {
        self.subscriptions = scopes;
    }

    /// What this session currently believes, for a scope it subscribed to.
    ///
    /// Intersected with the subscription set rather than answered from the
    /// store alone: a peer that stopped watching something must stop hearing
    /// about it, and a store lookup that ignored the subscription would keep
    /// delivering.
    pub fn is_watching(&self, scope: &TransientScope) -> bool {
        self.subscriptions.contains(scope)
    }

    /// Offer one item this peer sent.
    pub fn admit(&mut self, item: &TransientItem, epoch: &[u8; 16], now: Instant) -> AdmitOutcome {
        let outcome = self.store.admit(item, epoch, now);
        match &outcome {
            AdmitOutcome::Evicted => self.counters.evictions += 1,
            AdmitOutcome::WrongEpoch => self.counters.wrong_epoch += 1,
            AdmitOutcome::Retired => self.counters.retired += 1,
            AdmitOutcome::Refused(crate::transient::TransientError::Bounds) => {
                self.counters.oversize_anchors += 1
            }
            _ => {}
        }
        outcome
    }

    pub fn sweep(&mut self, now: Instant) -> usize {
        self.store.sweep(now)
    }

    pub fn store(&self) -> &TransientStore {
        &self.store
    }
}

/// The Replica read a caret needs, and nothing else.
///
/// Narrow on purpose. The Live plane could hold an `Arc<StationCore>` and reach
/// everything, and then the cost of every caret would be invisible at the seam
/// that pays it. Two methods make the price legible: both take the exclusive
/// commit lock, which is not a choice — `RwLock<T>: Sync` requires `T: Sync`
/// and the Replica holds a `dyn Fabric + Send` that is not. `with_replica`
/// records the arithmetic that bounds it.
pub trait AnchorSource: Send + Sync {
    /// Mint an anchor at a position, so a browser that can only send an offset
    /// has something that survives concurrent edits.
    fn anchor_in_body(
        &self,
        key: &replica::ids::BodyKey,
        path: &str,
        position: u64,
    ) -> Option<replica::FabricAnchor>;

    /// Where that position is now. Total: never an error, never a mutation,
    /// and never a silently wrong index.
    fn resolve_anchor(
        &self,
        key: &replica::ids::BodyKey,
        anchor: &replica::FabricAnchor,
    ) -> replica::AnchorResolution;
}

/// Where a peer's position is, as of this read.
///
/// Computed on every read and never stored in a slot. A resolution is only true
/// against the Body as it stands, and a cached one is a number that was right
/// once — which is exactly the silently-wrong index the algebra exists to
/// prevent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum CaretState {
    /// A position in the Body as it stands now.
    At(u64),
    /// The material this position was attached to is gone, or the anchor
    /// predates what this Replica retains.
    Drifted,
    /// Nothing was available to resolve against.
    ///
    /// Distinct from `Drifted`, which is an answer. This is the absence of one,
    /// and a renderer that conflated them would show a live caret as lost.
    Unresolved,
}

/// One thing a peer is currently doing, resolved for a reader.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct LiveEntry {
    pub station: StationId,
    pub scope: TransientScope,
    pub kind: crate::transient::TransientKind,
    /// How long ago this Station saw it. Ours, not theirs — a peer's clock is a
    /// peer's claim.
    pub age_ms: u64,
    /// Past `CARET_GRACE`. Still shown, and shown as uncertain: a caret whose
    /// Body has moved under it since it arrived is not wrong yet, but it is no
    /// longer known to be right.
    pub uncertain: bool,
    pub caret: Option<CaretState>,
    /// A selection's far end.
    pub focus: Option<CaretState>,
}

/// What a Station currently believes about who is doing what.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct LiveView {
    /// Bumped on every change. A reader that sees the same number saw the same
    /// view, and does not have to diff to find that out.
    pub generation: u64,
    /// This Station is not hearing from everyone it could be — over the session
    /// cap, or dropping scopes at a gate.
    ///
    /// Load-bearing rather than diagnostic. Awareness is allowed to be
    /// incomplete and durable convergence is not, so the surface that can be
    /// partial has to say when it is; a viewer showing three of five people
    /// with no indication is telling a confident lie.
    pub partial: bool,
    pub entries: Vec<LiveEntry>,
}

/// The cross-thread half of the Live plane.
///
/// The driver writes it from its own thread; the daemon and the browser bridge
/// read it from theirs. Everything in it is plain data — scopes, sequence
/// numbers, encoded anchors — so a `Mutex` here is a `Mutex` over bytes and
/// never over a collaborative document.
///
/// **Lock order: this table is never held across a Replica read.** `view`
/// snapshots under the lock, releases it, and only then resolves anchors. Doing
/// it the other way would put the commit lock underneath a lock the browser can
/// take, which is a deadlock waiting for a busy afternoon.
pub struct LiveHandle {
    table: std::sync::Mutex<PublishTable>,
    signals: SignalSink,
    anchors: Option<std::sync::Arc<dyn AnchorSource>>,
    residency: std::sync::Arc<dyn ResidencyOracle>,
    /// Files somebody offered, waiting for a person.
    ///
    /// Beside the transient table rather than in it: a slot expires on a TTL
    /// because a cursor that stopped moving is stale, and an offer that has been
    /// sitting for an hour is exactly as valid as it was when it arrived.
    offers: std::sync::Mutex<crate::signal::OfferQueue>,
}

#[derive(Default)]
struct PublishTable {
    generation: u64,
    /// At the inbound session ceiling. Owned by `LiveService`.
    accepting_capped: bool,
    /// At the outbound session ceiling. Owned by the dialer.
    ///
    /// Two flags rather than one, because they have two owners computing them
    /// from disjoint counts. A single flag meant each side unconditionally
    /// overwrote the other's answer, so a Station at its dial ceiling reported
    /// itself complete the moment any inbound session ended.
    dialling_capped: bool,
    /// Something was dropped at a gate, and this is when it stops mattering.
    ///
    /// A decaying cause rather than a standing one. A dropped datagram matters
    /// until the slot it would have written is either overwritten by a later
    /// item or expires, and `CURSOR_TTL` is that bound — so the view says
    /// "incomplete" for exactly as long as the drop could still be the reason
    /// something is missing, and then stops.
    dropped_until: Option<Instant>,
    slots: std::collections::BTreeMap<
        (StationId, TransientScope, u8),
        crate::transient::TransientSlot,
    >,
}

impl PublishTable {
    fn partial(&self, now: Instant) -> bool {
        self.accepting_capped
            || self.dialling_capped
            || self.dropped_until.is_some_and(|until| now < until)
    }
}

impl LiveHandle {
    pub fn new(anchors: Option<std::sync::Arc<dyn AnchorSource>>) -> Self {
        Self::with_residency(anchors, std::sync::Arc::new(NoResidency))
    }

    pub fn with_residency(
        anchors: Option<std::sync::Arc<dyn AnchorSource>>,
        residency: std::sync::Arc<dyn ResidencyOracle>,
    ) -> Self {
        Self {
            table: std::sync::Mutex::new(PublishTable::default()),
            signals: tokio::sync::broadcast::channel(SIGNAL_QUEUE).0,
            anchors,
            residency,
            offers: std::sync::Mutex::new(crate::signal::OfferQueue::new()),
        }
    }

    /// Hold a file somebody offered.
    ///
    /// Queueing is the whole of what receiving an offer does. No fetch starts,
    /// no path is resolved and no byte is written — the three auto-accept gates
    /// are asked by whoever decides, not by the plane that carried the message.
    pub fn offer(&self, offer: crate::signal::PendingOffer) -> crate::signal::OfferOutcome {
        self.offers().admit(offer)
    }

    /// What is waiting for a decision.
    pub fn pending_offers(&self) -> Vec<crate::signal::PendingOffer> {
        self.offers().pending().to_vec()
    }

    pub fn take_offer(
        &self,
        from: &StationId,
        content: &[u8; 32],
    ) -> Option<crate::signal::PendingOffer> {
        self.offers().take(from, content)
    }

    /// Drop everything one peer offered. What a revocation does — and only a
    /// revocation: a peer whose laptop slept is still somebody whose file offer
    /// is worth keeping.
    pub fn forget_offers(&self, from: &StationId) -> usize {
        self.offers().forget(from)
    }

    fn offers(&self) -> std::sync::MutexGuard<'_, crate::signal::OfferQueue> {
        self.offers.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// How much of a content this Station holds, for a peer that asked.
    ///
    /// A hint, and the answer to an unknown content is indistinguishable from
    /// the answer to one nobody here holds — otherwise this is an oracle for
    /// what a Space contains, answerable by guessing content ids.
    pub fn residency(&self, content: &[u8; 32], wanted: &[u32]) -> ResidencyState {
        self.residency.residency(content, wanted)
    }

    fn table(&self) -> std::sync::MutexGuard<'_, PublishTable> {
        self.table.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Listen for signals this Station receives.
    ///
    /// Subscribing before anything arrives is the only way to hear anything: a
    /// signal is an event rather than a state anyone can re-read.
    pub fn signals(&self) -> tokio::sync::broadcast::Receiver<crate::signal::DeliveredSignal> {
        self.signals.subscribe()
    }

    /// Hand a received signal to whoever is listening.
    ///
    /// Nobody listening is not a failure, and neither is a subscriber that fell
    /// behind. Both are local facts, and if either changed the wire outcome a
    /// peer could learn whether a viewer is open by pinging with an `Attention`.
    ///
    /// Public, and it takes a whole `DeliveredSignal`: provenance is a required
    /// field rather than something the plane fills in, so nothing can hand a
    /// listener a signal without saying who it came from.
    pub fn deliver(&self, delivered: crate::signal::DeliveredSignal) {
        let _ = self.signals.send(delivered);
    }

    /// Record one item a peer sent, after the per-session store admitted it.
    ///
    /// After, not instead: the session store is what bounds a single connection,
    /// and this is what a reader sees. An item that failed the session's own
    /// ceilings never reaches here.
    /// Returns whether it was stored. `false` means the shared table is full.
    pub fn record(&self, station: &StationId, item: &TransientItem, now: Instant) -> bool {
        let mut table = self.table();
        let key = (
            station.clone(),
            item.scope.clone(),
            item.payload.kind() as u8,
        );
        // Replacing an existing slot is always allowed; only a *new* one can
        // grow the table. Refusing a replacement at the ceiling would freeze
        // every cursor already in it at whatever position it happened to hold.
        if !table.slots.contains_key(&key) && table.slots.len() >= slots::MAX_PUBLISHED_SLOTS {
            // The reader is told the view is incomplete for as long as the
            // refused item could still have been the reason something is
            // missing, which is the same window a gate drop earns.
            table.dropped_until = Some(now + deadline::CURSOR_TTL);
            return false;
        }
        table.generation = table.generation.wrapping_add(1);
        table.slots.insert(
            key,
            crate::transient::TransientSlot {
                session_epoch: item.session_epoch,
                seq: item.seq,
                arrived_at: now,
                retired_at: None,
                payload: item.payload.clone(),
            },
        );
        true
    }

    /// Drop what one *session* held.
    ///
    /// Keyed by the session epoch as well as the Station, and that is not
    /// fussiness: `MAX_LIVE_SESSIONS_PER_STATION` is two, so a peer with a
    /// laptop and a phone has two sessions writing into slots keyed by Station
    /// alone. Forgetting by Station meant the laptop closing deleted what the
    /// phone was still saying.
    ///
    /// Slots stay keyed by Station, which is what makes a second tab supersede
    /// the first rather than appear beside it. Only the *removal* is per
    /// session.
    pub fn forget_session(&self, station: &StationId, session_epoch: &[u8; 16]) -> usize {
        let mut table = self.table();
        let before = table.slots.len();
        table
            .slots
            .retain(|(held, _, _), slot| held != station || &slot.session_epoch != session_epoch);
        let dropped = before - table.slots.len();
        if dropped > 0 {
            table.generation = table.generation.wrapping_add(1);
        }
        dropped
    }

    /// Drop everything a Station held, whichever session put it there.
    ///
    /// What a revocation does. Standing is per peer, not per connection, so a
    /// peer that lost it keeps nothing on any of its sessions.
    pub fn forget(&self, station: &StationId) -> usize {
        let mut table = self.table();
        let before = table.slots.len();
        table.slots.retain(|(held, _, _), _| held != station);
        let dropped = before - table.slots.len();
        if dropped > 0 {
            table.generation = table.generation.wrapping_add(1);
        }
        dropped
    }

    /// Drop one peer's slots for one scope.
    pub fn retire(&self, station: &StationId, scope: &TransientScope) {
        let mut table = self.table();
        let before = table.slots.len();
        table
            .slots
            .retain(|(held, held_scope, _), _| held != station || held_scope != scope);
        if table.slots.len() != before {
            table.generation = table.generation.wrapping_add(1);
        }
    }

    /// Say whether this Station is at its ceiling for sessions it *accepts*.
    pub fn set_accepting_capped(&self, capped: bool) {
        let mut table = self.table();
        if table.accepting_capped != capped {
            table.accepting_capped = capped;
            table.generation = table.generation.wrapping_add(1);
        }
    }

    /// Say whether this Station is at its ceiling for sessions it *dials*.
    pub fn set_dialling_capped(&self, capped: bool) {
        let mut table = self.table();
        if table.dialling_capped != capped {
            table.dialling_capped = capped;
            table.generation = table.generation.wrapping_add(1);
        }
    }

    /// Record that something was refused at a gate.
    ///
    /// Called from the drop paths rather than inferred from a counter, because
    /// the two questions are different: a counter says how much was dropped
    /// ever, and this says whether what a reader is looking at right now might
    /// be missing something. The reader needs the second one.
    pub fn note_dropped(&self, now: Instant) {
        let mut table = self.table();
        let until = now + deadline::CURSOR_TTL;
        if !table.partial(now) {
            table.generation = table.generation.wrapping_add(1);
        }
        table.dropped_until = Some(until);
    }

    /// Drop what has expired. Nothing depends on when this runs.
    pub fn sweep(&self, now: Instant) -> usize {
        let mut table = self.table();
        let before = table.slots.len();
        table.slots.retain(|(_, scope, _), slot| {
            let ttl = match scope {
                TransientScope::IssueView { .. }
                | TransientScope::DocumentView { .. }
                | TransientScope::CustomWorld { .. } => deadline::PRESENCE_TTL,
                _ => deadline::CURSOR_TTL,
            };
            now.duration_since(slot.arrived_at) < ttl
        });
        let dropped = before - table.slots.len();
        if dropped > 0 {
            table.generation = table.generation.wrapping_add(1);
        }
        dropped
    }

    /// The generation alone, for a reader deciding whether to build a view.
    pub fn generation(&self) -> u64 {
        self.table().generation
    }

    /// Mint an anchor at a position inside a Body.
    ///
    /// `None` when the World id does not parse, when there is no Replica, or
    /// when the position names nothing the algebra can bind — all three are the
    /// same answer to a caller: there is no anchor to send.
    pub fn anchor(
        &self,
        world: &str,
        body: [u8; 16],
        field: &str,
        position: u64,
    ) -> Option<Vec<u8>> {
        let key = body_key(world, body)?;
        let anchor = self
            .anchors
            .as_ref()?
            .anchor_in_body(&key, field, position)?;
        Some(anchor.encode())
    }

    /// Everything currently believed, resolved against the Bodies as they stand.
    ///
    /// `scope` narrows it; `None` is the whole table. Resolution happens here,
    /// per read, and the answer is never written back into a slot.
    pub fn view(&self, scope: Option<&TransientScope>, now: Instant) -> LiveView {
        // Snapshotted, then the lock is released. Resolving under it would take
        // the commit lock while holding a lock the browser can take.
        let (generation, partial, held) = {
            let table = self.table();
            let held: Vec<_> = table
                .slots
                .iter()
                .filter(|((_, held_scope, _), _)| scope.is_none_or(|want| held_scope == want))
                .map(|((station, held_scope, _), slot)| {
                    (station.clone(), held_scope.clone(), slot.clone())
                })
                .collect();
            (table.generation, table.partial(now), held)
        };

        let entries = held
            .into_iter()
            .map(|(station, scope, slot)| {
                let age = now.saturating_duration_since(slot.arrived_at);
                let (caret, focus) = self.resolve(&scope, &slot.payload);
                LiveEntry {
                    station,
                    kind: slot.payload.kind(),
                    scope,
                    age_ms: age.as_millis() as u64,
                    uncertain: age > deadline::CARET_GRACE,
                    caret,
                    focus,
                }
            })
            .collect();
        LiveView {
            generation,
            partial,
            entries,
        }
    }

    fn resolve(
        &self,
        scope: &TransientScope,
        payload: &crate::transient::TransientPayload,
    ) -> (Option<CaretState>, Option<CaretState>) {
        use crate::transient::TransientPayload;
        let (anchor, focus) = match payload {
            TransientPayload::Caret { anchor } => (Some(anchor), None),
            TransientPayload::Selection { anchor, focus } => (Some(anchor), Some(focus)),
            _ => return (None, None),
        };
        let Some((world, body)) = scope_body(scope) else {
            return (
                Some(CaretState::Unresolved),
                focus.map(|_| CaretState::Unresolved),
            );
        };
        let Some(key) = body_key(&world, body) else {
            return (
                Some(CaretState::Unresolved),
                focus.map(|_| CaretState::Unresolved),
            );
        };
        let one = |raw: &Vec<u8>| -> CaretState {
            let Some(source) = self.anchors.as_ref() else {
                return CaretState::Unresolved;
            };
            // A stored anchor was validated on the way in, so a decode failure
            // here is this Station's bug rather than a peer's — and the honest
            // answer is still "no position", never a guess.
            let Ok(decoded) = replica::FabricAnchor::decode_canonical(raw) else {
                return CaretState::Unresolved;
            };
            match source.resolve_anchor(&key, &decoded) {
                replica::AnchorResolution::Resolved(at) => CaretState::At(at),
                replica::AnchorResolution::Drifted => CaretState::Drifted,
            }
        };
        (anchor.map(&one), focus.map(&one))
    }
}

/// The Body a scope names, when it names one.
fn scope_body(scope: &TransientScope) -> Option<(String, [u8; 16])> {
    match scope {
        TransientScope::IssueView { world, body }
        | TransientScope::DocumentView { world, body }
        | TransientScope::TextCaret { world, body, .. }
        | TransientScope::Typing { world, body, .. } => Some((world.clone(), *body)),
        _ => None,
    }
}

fn body_key(world: &str, body: [u8; 16]) -> Option<replica::ids::BodyKey> {
    Some(replica::ids::BodyKey::new(
        replica::ids::WorldId::parse(world)?,
        replica::ids::BodyId::from_bytes(body),
    ))
}

/// Where a received signal goes.
///
/// A broadcast rather than a callback, because a signal has no single owner: a
/// file offer is for a person, an invite may be for a viewer and a log, and the
/// plane should not have to know which. Lagging is fine and does not need
/// reporting — a subscriber that fell behind on invitations missed
/// invitations, which is a thing that happens to people too.
pub type SignalSink = tokio::sync::broadcast::Sender<crate::signal::DeliveredSignal>;

/// How many received signals are held for a subscriber that is not reading.
///
/// Small. Signals are person-scale and rate-limited at four a second per
/// connection, so a subscriber this far behind is not going to catch up by
/// being given more room.
const SIGNAL_QUEUE: usize = 32;

/// Serve one admitted Live connection until it ends or the driver stops.
///
/// Everything here is `Rc`/`RefCell` and never a lock: `run_driver` is a
/// current-thread runtime with a `LocalSet`, so per-session state has one
/// owner and contention is not a thing that can happen.
pub struct SessionContext {
    /// Where what this peer says becomes readable to anyone else.
    pub handle: Option<std::sync::Arc<LiveHandle>>,
    /// What this connection may say on the signal lane. `None` means this build
    /// is not serving the lane here, and a signal flow is refused rather than
    /// ignored.
    pub signals: Option<crate::signal::SignalPolicy>,
    /// Re-asked on a beat and before every subscription change.
    ///
    /// A session pins the authority view it was admitted at, which is what makes
    /// every later question on it answerable consistently — and also what makes
    /// a revocation invisible to it. The driver closes the connection when the
    /// tick fires; this is the same question asked from inside, so a revocation
    /// that arrives between ticks cannot buy a subscription.
    pub authority: Option<std::sync::Arc<dyn crate::world::AuthorityView>>,
}

pub async fn serve_session(
    connection: std::sync::Arc<dyn comms::Connection>,
    peer: AdmittedPeer,
    cancel: crate::lifecycle::CancelToken,
    context: SessionContext,
) {
    let SessionContext {
        handle,
        signals,
        authority,
    } = context;
    let session = Rc::new(RefCell::new(LiveSession::new(peer.station.clone())));
    let epoch = peer.session_epoch;
    // Whatever ends this connection — idle, cancel, a gate, a peer hanging up —
    // the slots go with it. Presence has no goodbye it can rely on, so the
    // session ending *is* the goodbye.
    let leaving = Leaving {
        station: peer.station.clone(),
        session_epoch: epoch,
        handle: handle.clone(),
    };
    let _leaving = leaving;

    // Three gates: control messages, datagrams, and new flows. Separate because
    // a peer that opens flows and sends nothing on them never reaches the
    // message gates, and one that floods datagrams never opens a flow.
    let mut control_gate = Gate::from_spec(Instant::now(), gates::LIVE_CONTROL);
    let mut control_bytes = ByteGate::from_spec(Instant::now(), gates::LIVE_CONTROL_BYTES);
    let mut datagram_gate = Gate::from_spec(Instant::now(), gates::LIVE_DATAGRAMS);
    let mut datagram_bytes = ByteGate::from_spec(Instant::now(), gates::LIVE_DATAGRAM_BYTES);
    let mut accept_gate = Gate::from_spec(Instant::now(), gates::STREAM_ACCEPT);
    // Evictions are charged on their own ledger, which never decays. A `Gate`
    // would be wrong here and subtly so: `Gate::check` decrements the same
    // strike counter `penalise` adds to, so a peer alternating one eviction with
    // eight honest datagrams sits at zero strikes forever while steadily
    // displacing everybody else from a bounded table.
    let mut evictions = crate::budget::Evictions::new(slots::MAX_EVICTIONS_PER_CONNECTION);
    // Signals get their own pair. A person-scale event arrives four a second at
    // most, and a peer sending them faster than that is not a person.
    let mut signal_gate = Gate::from_spec(Instant::now(), gates::SIGNAL_RATE);
    let mut signal_bytes = ByteGate::from_spec(Instant::now(), gates::SIGNAL_BYTES);

    // The permit before the stream kind is read, mirroring Freight: a peer that
    // opens flows faster than they are served queues on a semaphore rather than
    // on the task scheduler.
    // A plain semaphore, not an `Arc` one: `run_driver` is a current-thread
    // runtime with a `LocalSet`, so this session has one owner and the permit
    // is held for the arm that took it.
    let workers = tokio::sync::Semaphore::new(bounds::MAX_STREAM_WORKERS);

    let mut last_seen = Instant::now();
    let mut sweep_at = Instant::now();
    let mut revalidated_at = Instant::now();

    // Asked on the beat, and again on any frame that acquires something.
    // `admit_peer` is the same question the admission asked, and asking it again
    // is the whole mechanism: a membership that went away has no other way to
    // reach a session that pinned the view it was admitted at.
    let still_admitted = |authority: &Option<std::sync::Arc<dyn crate::world::AuthorityView>>| {
        authority
            .as_ref()
            .is_none_or(|view| view.admit_peer(&peer.station).is_some())
    };

    loop {
        if cancel.is_cancelled() {
            break;
        }
        // A session with nothing on it at all. Longer than Freight's, because a
        // person reading emits nothing for a while and is still there — and
        // bounded just above the presence TTL, because past that their presence
        // has already expired and this is a connection saying nothing about
        // somebody nobody can see.
        if last_seen.elapsed() > deadline::LIVE_IDLE {
            break;
        }
        if revalidated_at.elapsed() > deadline::AUTHORITY_REVALIDATION {
            revalidated_at = Instant::now();
            if !still_admitted(&authority) {
                // Immediately, not at TTL. A peer that lost standing keeps
                // nothing, and a cursor lingering for thirty seconds after a
                // removal is a person still visibly in a room they were asked
                // to leave. Its offers go too — a file offered by somebody who
                // is no longer a member is not one anyone should be shown a
                // button for.
                if let Some(handle) = &handle {
                    handle.forget_offers(&peer.station);
                }
                connection.close(REFUSED, b"");
                break;
            }
        }
        if sweep_at.elapsed() > deadline::CURSOR_COALESCE {
            sweep_at = Instant::now();
            let now = Instant::now();
            session.borrow_mut().sweep(now);
            if let Some(handle) = &handle {
                handle.sweep(now);
            }
        }

        tokio::select! {
            biased;

            datagram = connection.read_datagram() => {
                let Ok(Some(payload)) = datagram else { break };
                last_seen = Instant::now();
                // Gated before decoded. A datagram costs a decode and a table
                // lookup, and both are work a peer can ask for without asking
                // anyone.
                match datagram_gate.check(Instant::now()) {
                    Verdict::Allow => {}
                    Verdict::Drop => {
                        // Dropped, and the view says so. A gate refusing a
                        // cursor is the plane working, but a reader shown four
                        // of five people with no indication is being told a
                        // confident lie.
                        if let Some(handle) = &handle {
                            handle.note_dropped(Instant::now());
                        }
                        continue;
                    }
                    Verdict::Close => {
                        connection.close(REFUSED, b"");
                        break;
                    }
                }
                if matches!(
                    datagram_bytes.check(Instant::now(), payload.len()),
                    Verdict::Close
                ) {
                    connection.close(REFUSED, b"");
                    break;
                }
                if payload.len() > MAX_TRANSIENT_ITEM_BYTES {
                    session.borrow_mut().counters.oversize_items += 1;
                    if let Some(handle) = &handle {
                        handle.note_dropped(Instant::now());
                    }
                    continue;
                }
                let Ok(item) = TransientItem::decode_canonical(&payload) else {
                    continue;
                };
                let mut session = session.borrow_mut();
                // Only for something this peer said it was watching. A peer
                // publishing into a scope it never subscribed to is asking this
                // Station to hold state on its behalf.
                if !session.is_watching(&item.scope) {
                    continue;
                }
                if matches!(item.scope, TransientScope::ContentResidency { .. })
                    && peer.features & crate::planes::feature::RESIDENCY_HINTS == 0
                {
                    // Negotiated or not carried. A peer that did not offer the
                    // bit cannot be sent hints it has no way to read, and it
                    // must not be able to publish them either — a capability is
                    // a two-sided agreement, and honouring it in one direction
                    // only is how one side ends up acting on state the other
                    // never agreed to keep.
                    continue;
                }
                let now = Instant::now();
                let outcome = session.admit(&item, &epoch, now);
                if outcome == AdmitOutcome::Evicted {
                    // A full table is not this peer's fault once; it is this
                    // peer's fault repeatedly. Charged rather than merely
                    // counted, because the table is shared and displacement is
                    // what a scope flood is *for*.
                    drop(session);
                    if evictions.charge(1) == Verdict::Close {
                        connection.close(REFUSED, b"");
                        break;
                    }
                    continue;
                }
                if outcome == AdmitOutcome::Stored {
                    // Only what the session store took. The per-connection
                    // ceilings are what bound one peer, and an item that failed
                    // them must not appear to a reader as though it had not.
                    if let Some(handle) = &handle {
                        // A full *shared* table is charged like a full session
                        // one: it is the same displacement, one level up.
                        if !handle.record(&peer.station, &item, now)
                            && evictions.charge(1) == Verdict::Close
                        {
                            connection.close(REFUSED, b"");
                            break;
                        }
                    }
                }
            }

            accepted = connection.accept_bi() => {
                let Ok(Some((mut send, mut recv))) = accepted else { break };
                last_seen = Instant::now();
                match accept_gate.check(Instant::now()) {
                    Verdict::Allow => {}
                    Verdict::Drop => {
                        // The flow is reset, not the connection. A peer over
                        // its flow rate is refused and stays.
                        drop(send);
                        continue;
                    }
                    Verdict::Close => {
                        connection.close(REFUSED, b"");
                        break;
                    }
                }
                let Ok(_permit) = workers.try_acquire() else {
                    drop(send);
                    continue;
                };

                // Deadlined, and this is the bound that keeps the session
                // alive rather than a nicety. Flows are served *inline* in this
                // loop — Freight spawns per request; this does not — so a peer
                // that opens a flow and goes quiet would park every other arm:
                // no datagram read, no sweep, no revalidation beat, and
                // `LIVE_IDLE` never fires because the loop never reaches its own
                // check.
                let kind = match tokio::time::timeout(
                    deadline::LIVE_FLOW_READ,
                    read_stream_kind(recv.as_mut()),
                )
                .await
                {
                    Err(_) => {
                        drop(send);
                        continue;
                    }
                    Ok(Ok(kind)) => kind,
                    // A reserved kind is a peer using a reservation we
                    // published and have not built: the flow resets and the
                    // connection stays up, because the peer is not wrong.
                    Ok(Err(StreamError::ReservedKind(_))) | Ok(Err(StreamError::UnknownKind(_))) => {
                        drop(send);
                        continue;
                    }
                    Ok(Err(_)) => {
                        drop(send);
                        continue;
                    }
                };
                if kind == stream_kind::RELIABLE_SIGNAL {
                    match signal_gate.check(Instant::now()) {
                        Verdict::Allow => {}
                        Verdict::Drop => {
                            drop(send);
                            continue;
                        }
                        Verdict::Close => {
                            connection.close(REFUSED, b"");
                            break;
                        }
                    }
                    // One bounded message per stream, served under the
                    // declaration's own ceiling — resolved from the selector
                    // before the length is consulted, which is what makes that
                    // ceiling a pre-allocation bound rather than a comment.
                    let Some(policy) = &signals else {
                        // No policy means this build is not serving the lane on
                        // this connection. The flow is refused on both halves,
                        // not merely dropped: a dropped send half leaves the
                        // peer writing into something nobody reads.
                        crate::signal::refuse_flow(send.as_mut(), recv.as_mut());
                        continue;
                    };
                    let served =
                        crate::signal::serve_signal(send.as_mut(), recv.as_mut(), policy).await;
                    let Ok(signal) = served else { continue };
                    if matches!(
                        signal_bytes.check(Instant::now(), signal.encode().len()),
                        Verdict::Close
                    ) {
                        connection.close(REFUSED, b"");
                        break;
                    }
                    if let Some(handle) = &handle {
                        if let crate::planes::Signal::FileOffer {
                            content,
                            plaintext_len,
                            display_name,
                            media_type,
                        } = &signal
                        {
                            // Queued, and that is all. An offer names content
                            // the sender holds; starting a transfer here would
                            // let any member spend this Station's disk by
                            // sending a message.
                            handle.offer(crate::signal::PendingOffer {
                                from: peer.station.clone(),
                                session_epoch: peer.session_epoch,
                                content: *content,
                                plaintext_len: *plaintext_len,
                                display_name: display_name.clone(),
                                media_type: media_type.clone(),
                            });
                        }
                        handle.deliver(crate::signal::DeliveredSignal {
                            from: peer.station.clone(),
                            session_id: peer.session_id,
                            session_epoch: peer.session_epoch,
                            signal,
                        });
                    }
                    continue;
                }
                if kind != stream_kind::CONTROL {
                    drop(send);
                    continue;
                }
                // Before the body is read, not after. The module doc has always
                // claimed "the gate before the message" and the control lane
                // read the whole framed message first, so a peer over its
                // control rate still made this Station buffer up to the frame
                // ceiling before being refused. The byte gate necessarily comes
                // after — it is about the size, which is not known until then.
                match control_gate.check(Instant::now()) {
                    Verdict::Allow => {}
                    Verdict::Drop => {
                        drop(send);
                        continue;
                    }
                    Verdict::Close => {
                        connection.close(REFUSED, b"");
                        break;
                    }
                }

                let Ok(Ok(body)) = tokio::time::timeout(
                    deadline::LIVE_FLOW_READ,
                    read_framed(recv.as_mut(), bounds::MAX_CONTROL_FRAME_BYTES),
                )
                .await
                else {
                    drop(send);
                    continue;
                };
                if matches!(
                    control_bytes.check(Instant::now(), body.len()),
                    Verdict::Close
                ) {
                    connection.close(REFUSED, b"");
                    break;
                }
                let Ok(control) = LiveControl::decode_canonical(&body) else {
                    drop(send);
                    continue;
                };
                let mut session = session.borrow_mut();
                match control {
                    LiveControl::Subscribe { scopes } => {
                        // Checked here as well as on the beat, because this is
                        // the frame that *acquires* something. Waiting up to a
                        // full revalidation interval to refuse a subscription is
                        // a window in which a revoked peer picks up new scopes.
                        if !still_admitted(&authority) {
                            drop(session);
                            connection.close(REFUSED, b"");
                            break;
                        }
                        session.subscribe(scopes)
                    }
                    LiveControl::Retire { scope, seq } => {
                        // Retirement covers every kind that scope admits: a
                        // peer saying it is done with a caret means the caret
                        // and the selection, not whichever one it named.
                        for kind in [
                            crate::transient::TransientKind::Presence,
                            crate::transient::TransientKind::Caret,
                            crate::transient::TransientKind::Selection,
                            crate::transient::TransientKind::Typing,
                            crate::transient::TransientKind::Residency,
                        ] {
                            session.store.retire(&scope, kind, epoch, seq, Instant::now());
                        }
                        if let Some(handle) = &handle {
                            handle.retire(&peer.station, &scope);
                        }
                    }
                }
            }

            _ = tokio::time::sleep(deadline::DRIVER_POLL) => {}
        }
    }
}

/// Send one transient item, or drop it and say why.
///
/// **No production caller.** Nothing in a shipped Station publishes its own
/// cursor: the plane receives, stores and serves what peers say, and the local
/// half — deciding what this Station is doing and when to say so — belongs to
/// whatever drives a person's view, which is the browser bridge and is not this
/// crate. Stated here because a reader finding `datagram_fits` and
/// `TransientCounters::capacity_drops` should know they are exercised by tests
/// and by nothing else, rather than assuming a path exists and looking for it.
///
/// The answer is never "truncate". A transient payload has no retransmit, so
/// half of one arrives as corruption rather than as a gap — and a peer that
/// negotiated no datagram support at all gets nothing rather than a fragment.
pub fn publish(
    connection: &dyn comms::Connection,
    item: &TransientItem,
    counters: &mut TransientCounters,
) -> bool {
    let encoded = item.encode();
    if !datagram_fits(encoded.len(), connection.datagram_capacity()) {
        counters.capacity_drops += 1;
        return false;
    }
    connection.send_datagram(&encoded).is_ok()
}

/// The Live plane's half of the driver contract.
///
/// Thin on purpose. `run_driver` already owns accept, the slot ceilings, the
/// opening read, replay through `AcceptedOpenings`, the judgement, the accept
/// write, the revocation race and the shutdown ladder — all of it plane-neutral
/// and all of it already proven by Freight. What is genuinely Live is one
/// function, and this is it.
pub struct LiveService {
    sessions: std::cell::RefCell<Vec<StationId>>,
    handle: std::sync::Arc<LiveHandle>,
    /// What a signal is asked about, and never anything that can commit.
    ///
    /// Held here rather than on the handle because these are the driver's, not
    /// the reader's: a browser looking at who is present has no business
    /// holding an authority view.
    authority: std::sync::Arc<dyn crate::world::AuthorityView>,
    worlds: crate::registry::WorldRegistry,
}

impl LiveService {
    pub fn new(
        handle: std::sync::Arc<LiveHandle>,
        authority: std::sync::Arc<dyn crate::world::AuthorityView>,
        worlds: crate::registry::WorldRegistry,
    ) -> Self {
        Self {
            sessions: std::cell::RefCell::new(Vec::new()),
            handle,
            authority,
            worlds,
        }
    }

    /// Which peers currently hold a session. A Station-local answer, and not a
    /// membership claim — a member with nothing open is not here.
    pub fn present(&self) -> Vec<StationId> {
        self.sessions.borrow().clone()
    }
}

impl crate::plane_driver::PlaneService for LiveService {
    async fn serve(
        &self,
        connection: std::sync::Arc<dyn comms::Connection>,
        peer: AdmittedPeer,
        cancel: crate::lifecycle::CancelToken,
    ) {
        let station = peer.station.clone();
        {
            let mut sessions = self.sessions.borrow_mut();
            sessions.push(station.clone());
            // Over the cap, awareness is incomplete and says so. Durable
            // convergence is unaffected — that is Contact's job and it does not
            // ride this plane.
            self.handle
                .set_accepting_capped(sessions.len() >= slots::MAX_LIVE_SESSIONS);
        }
        // A guard rather than the two lines this used to be after the await.
        // `run_driver` races `serve` against `watch_for_revocation`, so on a
        // revocation the serve future is *dropped* and nothing after the await
        // runs — which left the revoked peer in this list forever and the
        // partial flag computed from a count that only ever grew.
        let _present = Present {
            station: station.clone(),
            sessions: &self.sessions,
            handle: &self.handle,
        };
        // Built per connection, from what the admission decided. The frontier
        // is the pinned one, so every question this connection asks is answered
        // against the view it was admitted at.
        let signals = crate::signal::SignalPolicy {
            peer: peer.station.clone(),
            actor: peer.actor.clone(),
            frontier: peer.authority_frontier.clone(),
            granted_lanes: peer.granted_lanes.clone(),
            authority: self.authority.clone(),
            worlds: self.worlds.clone(),
        };
        serve_session(
            connection,
            peer,
            cancel,
            SessionContext {
                handle: Some(self.handle.clone()),
                signals: Some(signals),
                authority: Some(self.authority.clone()),
            },
        )
        .await;
    }
}

/// Removes a peer from the present list when its session ends, however it ends.
///
/// Borrowed rather than cloned because `LiveService` outlives every session it
/// serves and both fields live on it.
struct Present<'a> {
    station: StationId,
    sessions: &'a std::cell::RefCell<Vec<StationId>>,
    handle: &'a std::sync::Arc<LiveHandle>,
}

impl Drop for Present<'_> {
    fn drop(&mut self) {
        let mut sessions = self.sessions.borrow_mut();
        // Exactly one entry, because `serve` pushes exactly one per connection.
        // `retain` removed every entry for the peer, so the first of a peer's
        // two sessions to end removed both and the second removed none —
        // leaving `present()` and the cap wrong while a session was still open.
        if let Some(at) = sessions.iter().position(|held| held == &self.station) {
            sessions.remove(at);
        }
        self.handle
            .set_accepting_capped(sessions.len() >= slots::MAX_LIVE_SESSIONS);
    }
}

/// Drops one peer's slots when its session ends, however it ends.
///
/// A guard rather than a line at the bottom of the loop: `serve_session` leaves
/// through more `break`s than anyone will keep counting, plus a cancellation and
/// a dropped future, and the one path that forgot to clean up would leave a
/// cursor on screen belonging to somebody who closed their laptop.
struct Leaving {
    station: StationId,
    session_epoch: [u8; 16],
    handle: Option<std::sync::Arc<LiveHandle>>,
}

impl Drop for Leaving {
    fn drop(&mut self) {
        if let Some(handle) = &self.handle {
            // Per session, not per Station. A peer may hold two — a laptop and
            // a phone — and forgetting by Station meant closing one deleted
            // what the other was still saying.
            handle.forget_session(&self.station, &self.session_epoch);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transient::TransientPayload;

    fn station() -> StationId {
        StationId::from_device(&mechanics::crypto::device_from_seed(&[9u8; 32])).expect("station")
    }

    fn scope(n: u8) -> TransientScope {
        TransientScope::IssueView {
            world: "com.example.notes".into(),
            body: [n; 16],
        }
    }

    #[test]
    fn a_peer_hears_only_about_what_it_subscribed_to() {
        // A subscription is a snapshot of what a client is looking at. A store
        // lookup that ignored it would keep delivering to a peer that stopped
        // watching, which is both a leak and a waste.
        let mut session = LiveSession::new(station());
        assert!(!session.is_watching(&scope(1)));
        session.subscribe(vec![scope(1), scope(2)]);
        assert!(session.is_watching(&scope(1)));
        assert!(!session.is_watching(&scope(3)));

        // Replace-all, not additive: a client that navigated away sends the new
        // snapshot, and the old scopes go with it.
        session.subscribe(vec![scope(3)]);
        assert!(!session.is_watching(&scope(1)));
        assert!(session.is_watching(&scope(3)));
    }

    #[test]
    fn every_drop_is_counted_because_dropped_and_counted_needs_a_reader() {
        let mut session = LiveSession::new(station());
        let epoch = [7u8; 16];
        let now = Instant::now();

        let mut stray = TransientItem {
            session_epoch: [8u8; 16],
            seq: 1,
            scope: scope(1),
            payload: TransientPayload::Presence,
        };
        session.admit(&stray, &epoch, now);
        assert_eq!(session.counters().wrong_epoch, 1);

        stray.session_epoch = epoch;
        session.admit(&stray, &epoch, now);
        assert_eq!(session.counters().wrong_epoch, 1, "and only the wrong ones");
    }

    /// A connection that reports a datagram capacity and refuses to send.
    ///
    /// Only the two methods `publish` touches do anything; the rest of the
    /// trait is unreachable from it, and a stub that panicked would be a
    /// stronger claim than this test can make.
    struct Narrow(Option<usize>);

    #[async_trait::async_trait]
    impl comms::Connection for Narrow {
        fn peer(&self) -> comms::PeerId {
            mechanics::crypto::device_from_seed(&[1u8; 32])
        }
        fn alpn(&self) -> Vec<u8> {
            b"lait/session/1".to_vec()
        }
        fn datagram_capacity(&self) -> Option<usize> {
            self.0
        }
        fn send_datagram(&self, _payload: &[u8]) -> anyhow::Result<()> {
            Ok(())
        }
        fn close(&self, _code: u32, _reason: &[u8]) {}
        async fn open_bi(
            &self,
        ) -> anyhow::Result<(Box<dyn comms::SendFlow>, Box<dyn comms::RecvFlow>)> {
            Err(anyhow::anyhow!("not used"))
        }
        async fn accept_bi(
            &self,
        ) -> anyhow::Result<Option<(Box<dyn comms::SendFlow>, Box<dyn comms::RecvFlow>)>> {
            Ok(None)
        }
        async fn open_uni(&self) -> anyhow::Result<Box<dyn comms::SendFlow>> {
            Err(anyhow::anyhow!("not used"))
        }
        async fn accept_uni(&self) -> anyhow::Result<Option<Box<dyn comms::RecvFlow>>> {
            Ok(None)
        }
        async fn read_datagram(&self) -> anyhow::Result<Option<Vec<u8>>> {
            Ok(None)
        }
        async fn closed(&self) {
            std::future::pending().await
        }
    }

    #[test]
    fn a_payload_the_path_cannot_carry_is_dropped_rather_than_cut() {
        // The one thing that must never happen: a transient payload has no
        // retransmit, so half of one is corruption rather than a gap.
        //
        // Driven through `publish` rather than by incrementing the counter this
        // then asserts. The first version of this did the latter, which proves
        // that `+= 1` makes a number one larger.
        // A maximal residency hint: 256 chunk indices, each large enough to need
        // a full varint, which is the largest thing this plane can legally
        // produce and comfortably past a real path's measured capacity.
        let item = TransientItem {
            session_epoch: [1u8; 16],
            seq: 1,
            scope: TransientScope::ContentResidency { content: [4u8; 32] },
            payload: TransientPayload::Residency {
                chunks: (0..crate::transient::MAX_RESIDENCY_CHUNKS as u32)
                    .map(|n| u32::MAX - n)
                    .collect(),
            },
        };
        let encoded = item.encode().len();
        assert!(
            encoded > 1_162,
            "a payload larger than a measured real path, not {encoded} bytes"
        );

        // A peer that negotiated no datagram support at all gets nothing rather
        // than a fragment.
        let mut counters = TransientCounters::default();
        assert!(!publish(&Narrow(None), &item, &mut counters));
        assert_eq!(counters.capacity_drops, 1);

        // And a payload past what the measured path carries.
        assert!(!publish(&Narrow(Some(1_162)), &item, &mut counters));
        assert_eq!(counters.capacity_drops, 2);

        // Something that fits does leave, and costs no drop.
        let small = TransientItem {
            session_epoch: [1u8; 16],
            seq: 2,
            scope: scope(1),
            payload: TransientPayload::Presence,
        };
        assert!(publish(&Narrow(Some(1_162)), &small, &mut counters));
        assert_eq!(counters.capacity_drops, 2);
    }
}

/// Why a Live dial did not become a session.
///
/// A type rather than a log line, for the same reason `ProviderRefusal` is one:
/// most refusals are coarse and mean "not now", but `UnsupportedVersion` is the
/// one a peer can act on, and collapsing it into the rest is how a generation
/// mismatch presents as an intermittent network problem for a week.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DialRefusal {
    /// Never answered, or the transport said no.
    Unreachable,
    /// Answered with a refusal it could spell.
    Refused(crate::planes::SessionRefusal),
    /// Neither an accept nor a refusal. Our problem to explain rather than
    /// theirs to have sent.
    Unintelligible,
    /// This Station does not consider the peer a member, so there is nothing to
    /// dial about. Checked before the transport is touched.
    NotAdmitted,
}

/// A dialled Live session, and what the responder granted it.
pub struct LivePeer {
    pub connection: std::sync::Arc<dyn comms::Connection>,
    pub peer: AdmittedPeer,
}

/// Dial one peer on the Live plane.
///
/// Modelled on `fetch::connect_provider`, and different in three ways that
/// matter. The plane is `Live`. The opening names lanes, because on this plane
/// the ALPN does not type the conversation — Freight carries none. And it offers
/// `feature::LOCAL_SUPPORTED` rather than a hand-picked list, so the offer moves
/// with the build: a bit joins that constant in the same commit as the code that
/// honours it, and offering one that is not there is a promise a peer would be
/// right to be annoyed about.
///
/// **The peer's identity is resolved locally, never taken from the accept.** The
/// responder tells us which lanes it granted, and nothing else it says about who
/// it is is load-bearing: the actor and the frontier come from this Station's own
/// `AuthorityView`, keyed by the Station id the transport authenticated.
pub async fn dial(
    transport: &dyn comms::Transport,
    authority: &dyn crate::world::AuthorityView,
    space: &mechanics::ids::SpaceId,
    local: &StationId,
    peer: &StationId,
    session_id: [u8; 16],
) -> Result<LivePeer, DialRefusal> {
    // Asked before the transport is touched. Dialling a peer we would refuse on
    // arrival is a round trip spent to be told what we already knew.
    let resolution = authority.admit_peer(peer).ok_or(DialRefusal::NotAdmitted)?;

    let mut space_bytes = [0u8; crate::planes::SPACE_ID_LEN];
    let raw = space.as_str().as_bytes();
    if raw.len() != crate::planes::SPACE_ID_LEN {
        return Err(DialRefusal::Unreachable);
    }
    space_bytes.copy_from_slice(raw);
    let mut epoch = [0u8; 16];
    getrandom::fill(&mut epoch).map_err(|_| DialRefusal::Unreachable)?;

    let connection = tokio::time::timeout(
        deadline::LIVE_DIAL,
        transport.connect_session(peer.as_device(), crate::planes::LIVE_ALPN),
    )
    .await
    .map_err(|_| DialRefusal::Unreachable)?
    .map_err(|_| DialRefusal::Unreachable)?;

    let open = crate::planes::SessionOpen {
        plane: crate::planes::Plane::Live,
        protocol_version: crate::planes::Plane::Live.protocol_version(),
        features: crate::planes::feature::LOCAL_SUPPORTED,
        space: space_bytes,
        initiator_station: local.key_bytes(),
        responder_station: peer.key_bytes(),
        session_id,
        session_epoch: epoch,
        authority_frontier: Vec::new(),
        requested_lanes: vec![stream_kind::CONTROL, stream_kind::RELIABLE_SIGNAL],
    };

    let mut flow = connection
        .open_uni()
        .await
        .map_err(|_| DialRefusal::Unreachable)?;
    flow.write_all(&open.encode())
        .await
        .map_err(|_| DialRefusal::Unreachable)?;
    flow.finish().map_err(|_| DialRefusal::Unreachable)?;

    let answer = tokio::time::timeout(deadline::LIVE_DIAL, async {
        let mut recv = connection.accept_uni().await.ok()??;
        recv.read_to_end(bounds::MAX_OPENING_BYTES).await.ok()
    })
    .await
    .map_err(|_| DialRefusal::Unreachable)?
    .ok_or(DialRefusal::Unreachable)?;

    match crate::planes::SessionAccept::decode_canonical(&answer) {
        Ok(accept) => Ok(LivePeer {
            connection: std::sync::Arc::from(connection),
            peer: AdmittedPeer {
                station: peer.clone(),
                actor: resolution.actor,
                authority_frontier: resolution.authority_frontier,
                // The one thing taken from the accept, because it is the one
                // thing only the responder knows: what it is willing to serve.
                granted_lanes: accept.granted_lanes,
                session_id,
                session_epoch: epoch,
                // Intersected locally, never taken on the peer's word. The
                // accept is the peer telling us what *it* agreed to, and a peer
                // is free to claim a bit this build does not implement — at
                // which point this Station would honour residency hints it has
                // no oracle behind. `judge` does this intersection on the
                // inbound path; the outbound path has to do it too, or the same
                // field means two different things depending on who dialled.
                features: accept.capability.features & crate::planes::feature::LOCAL_SUPPORTED,
            },
        }),
        Err(_) => Err(
            match crate::planes::SessionRefusal::decode_canonical(&answer) {
                Ok(refusal) => DialRefusal::Refused(refusal),
                Err(_) => DialRefusal::Unintelligible,
            },
        ),
    }
}

/// Who may be dialled, how often, and how many at once.
///
/// Three separate ceilings because they answer three questions: how many
/// sessions this Station will hold, how many of those any one peer may take, and
/// how many dials may be outstanding while none of them has answered yet. The
/// third is the one that is easy to forget and the one that matters under a
/// partition, where every dial is in flight and none of them is a session.
#[derive(Default)]
pub struct DialLedger {
    /// Per peer, the earliest next attempt, and how many attempts have failed.
    ///
    /// A Station sitting at its own ceiling refuses every dial with a bare
    /// close, which reaches the dialer as an ordinary transport failure. Without
    /// a cooldown that is a hot loop against a Station that is doing nothing
    /// wrong.
    cooldown: std::collections::BTreeMap<StationId, (Instant, u32)>,
    in_flight: std::collections::BTreeSet<StationId>,
    sessions: std::collections::BTreeMap<StationId, usize>,
}

impl DialLedger {
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether to dial this peer now.
    pub fn may_dial(&self, peer: &StationId, now: Instant) -> bool {
        if self.in_flight.contains(peer) {
            return false;
        }
        if self.in_flight.len() >= slots::MAX_LIVE_DIALS_IN_FLIGHT {
            return false;
        }
        if self.total_sessions() >= slots::MAX_LIVE_SESSIONS {
            return false;
        }
        if self.sessions.get(peer).copied().unwrap_or(0) >= slots::MAX_LIVE_SESSIONS_PER_STATION {
            return false;
        }
        match self.cooldown.get(peer) {
            Some((until, _)) => now >= *until,
            None => true,
        }
    }

    pub fn total_sessions(&self) -> usize {
        self.sessions.values().sum()
    }

    pub fn dials_in_flight(&self) -> usize {
        self.in_flight.len()
    }

    pub fn begin(&mut self, peer: &StationId) {
        self.in_flight.insert(peer.clone());
    }

    /// A dial that became a session. The cooldown is cleared: what it was
    /// protecting against is a peer that will not talk to us, and this one just
    /// did.
    pub fn established(&mut self, peer: &StationId) {
        self.in_flight.remove(peer);
        self.cooldown.remove(peer);
        *self.sessions.entry(peer.clone()).or_insert(0) += 1;
    }

    /// A dial that did not.
    ///
    /// Backoff doubles from `LIVE_DIAL` and is capped at `PRESENCE_TTL`, both
    /// named rather than invented: the floor is how long one dial is allowed to
    /// take, and the ceiling is how long a peer can be silent before its
    /// presence has expired anyway — past that, retrying faster buys nothing
    /// anyone can see.
    pub fn failed(&mut self, peer: &StationId, now: Instant) {
        self.in_flight.remove(peer);
        let entry = self.cooldown.entry(peer.clone()).or_insert((now, 0));
        entry.1 = entry.1.saturating_add(1);
        let doubled = deadline::LIVE_DIAL
            .checked_mul(1u32 << entry.1.min(5))
            .unwrap_or(deadline::PRESENCE_TTL);
        entry.0 = now + doubled.min(deadline::PRESENCE_TTL);
    }

    /// A session that ended. Not a failure: it says nothing about whether the
    /// peer would answer again, so it earns no cooldown.
    pub fn ended(&mut self, peer: &StationId) {
        if let Some(count) = self.sessions.get_mut(peer) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                self.sessions.remove(peer);
            }
        }
    }

    /// Whether this Station is at a ceiling, and therefore not hearing from
    /// everyone it could be.
    pub fn is_capped(&self) -> bool {
        self.total_sessions() >= slots::MAX_LIVE_SESSIONS
    }

    /// Forget cooldowns that have elapsed, so the map does not grow with every
    /// peer this Station has ever failed to reach.
    pub fn prune(&mut self, now: Instant) {
        self.cooldown.retain(|_, (until, _)| now < *until);
    }
}

/// Everything the dial loop needs, and nothing that can commit.
pub struct DialContext {
    pub space: mechanics::ids::SpaceId,
    pub local_station: StationId,
    pub transport: std::sync::Arc<dyn comms::Transport>,
    /// Who might be worth dialling, asked afresh every round.
    ///
    /// A closure rather than a snapshot taken once: a Neighbor learned a minute
    /// after activation should be dialled a minute after activation, not at the
    /// next restart.
    pub candidates: std::sync::Arc<dyn Fn() -> Vec<StationId> + Send + Sync>,
    pub handle: std::sync::Arc<LiveHandle>,
    pub authority: std::sync::Arc<dyn crate::world::AuthorityView>,
    pub worlds: crate::registry::WorldRegistry,
    pub cancel: crate::lifecycle::CancelToken,
}

/// Dial peers on the Live plane until cancelled. Blocking; call it on its own
/// thread.
///
/// Its own thread and its own current-thread runtime for the same reason
/// `run_driver` has them: `serve_session` is not `Send`, and a dialled session
/// is served by exactly the same function an accepted one is. There is no
/// second implementation of a Live session and there must not be.
pub fn run_dialer(context: DialContext) {
    let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        return;
    };
    let local = tokio::task::LocalSet::new();
    local.block_on(&runtime, dial_loop(context));
}

async fn dial_loop(context: DialContext) {
    let ledger = Rc::new(RefCell::new(DialLedger::new()));
    let mut sessions = tokio::task::JoinSet::new();
    // Zero, so the first round happens on entry rather than one interval later.
    // A Station that has just come up is exactly when somebody is waiting to see
    // whether anyone else is there.
    let mut last_round: Option<Instant> = None;

    loop {
        if context.cancel.is_cancelled() {
            break;
        }
        let now = Instant::now();
        if last_round.is_none_or(|at| now.duration_since(at) >= deadline::LIVE_DIAL) {
            last_round = Some(now);
            let mut led = ledger.borrow_mut();
            led.prune(now);
            context.handle.set_dialling_capped(led.is_capped());
            let candidates = (context.candidates)();
            for peer in candidates {
                if peer == context.local_station {
                    // Dialling ourselves would be a session with our own
                    // presence in it, which reads to a viewer as a second
                    // person who agrees with everything you do.
                    continue;
                }
                if !led.may_dial(&peer, now) {
                    continue;
                }
                led.begin(&peer);
                let mut session_id = [0u8; 16];
                if getrandom::fill(&mut session_id).is_err() {
                    led.failed(&peer, now);
                    continue;
                }
                sessions.spawn_local(dial_and_serve(
                    DialTask {
                        transport: context.transport.clone(),
                        authority: context.authority.clone(),
                        worlds: context.worlds.clone(),
                        handle: context.handle.clone(),
                        cancel: context.cancel.clone(),
                        space: context.space.clone(),
                        local_station: context.local_station.clone(),
                        ledger: ledger.clone(),
                    },
                    peer,
                    session_id,
                ));
            }
        }

        // Reaped without waiting, so the set does not grow with the number of
        // sessions this dialer has ever opened.
        while sessions.try_join_next().is_some() {}
        tokio::time::sleep(deadline::DRIVER_POLL).await;
    }

    // Every dialled session is cancelled with the Station, and joined rather
    // than abandoned: a session outliving the drain holds a connection whose
    // Station is already gone.
    sessions.shutdown().await;
}

/// One dial's worth of shared state. A struct because eight positional
/// arguments is where a call site stops being readable.
struct DialTask {
    transport: std::sync::Arc<dyn comms::Transport>,
    authority: std::sync::Arc<dyn crate::world::AuthorityView>,
    worlds: crate::registry::WorldRegistry,
    handle: std::sync::Arc<LiveHandle>,
    cancel: crate::lifecycle::CancelToken,
    space: mechanics::ids::SpaceId,
    local_station: StationId,
    ledger: Rc<RefCell<DialLedger>>,
}

async fn dial_and_serve(task: DialTask, peer: StationId, session_id: [u8; 16]) {
    let dialled = dial(
        task.transport.as_ref(),
        task.authority.as_ref(),
        &task.space,
        &task.local_station,
        &peer,
        session_id,
    )
    .await;

    let live = match dialled {
        Ok(live) => live,
        Err(_) => {
            // Every refusal earns the same cooldown. They differ in what an
            // operator should be told and not in what the dialer should do:
            // there is no refusal a dialer can fix by trying again sooner.
            task.ledger.borrow_mut().failed(&peer, Instant::now());
            return;
        }
    };

    task.ledger.borrow_mut().established(&peer);
    let signals = crate::signal::SignalPolicy {
        peer: live.peer.station.clone(),
        actor: live.peer.actor.clone(),
        frontier: live.peer.authority_frontier.clone(),
        granted_lanes: live.peer.granted_lanes.clone(),
        authority: task.authority.clone(),
        worlds: task.worlds.clone(),
    };
    serve_session(
        live.connection,
        live.peer,
        task.cancel.clone(),
        SessionContext {
            handle: Some(task.handle.clone()),
            signals: Some(signals),
            authority: Some(task.authority.clone()),
        },
    )
    .await;

    let mut ledger = task.ledger.borrow_mut();
    ledger.ended(&peer);
    task.handle.set_dialling_capped(ledger.is_capped());
}

/// How much of a content this Station holds.
///
/// Three states rather than a chunk bitmap, and that is the whole design. A hint
/// says *who to ask first*; it is not an inventory, and a peer that could read
/// a complete bitmap off a hint would be able to reconstruct which parts of a
/// file somebody had opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ResidencyState {
    /// None of the chunks asked about are here.
    ///
    /// The same answer a content this Station has never heard of gets, because
    /// `resident_among` returns an empty list for both — a caller that could
    /// tell them apart would hold an oracle for what a Space contains,
    /// answerable by guessing content ids.
    Absent,
    /// Some of them.
    Partial,
    /// All of them.
    Complete,
}

/// Whether this Station holds parts of a content, for a peer that asked.
///
/// A trait rather than a direct `ContentHost` call, so the Live plane holds the
/// question rather than the content plane's whole surface — and so a Station
/// with no content host still compiles into a working Live plane, answering
/// `Absent` because that is the truth.
pub trait ResidencyOracle: Send + Sync {
    /// Which of `wanted` are here.
    ///
    /// Keyed by the full 32-byte content id. A prefix would let a peer probe
    /// "do you hold anything under these bits" without knowing a content id at
    /// all, which is weaker than Freight's exact `Have` and strictly worse than
    /// asking.
    fn residency(&self, content: &[u8; 32], wanted: &[u32]) -> ResidencyState;
}

/// The oracle over a real content host.
///
/// Holds what a `ContentPolicy` needs, because that policy is not something the
/// Live plane can invent: the space, the epoch key source and the operator
/// ceiling all belong to the composition root, and a plane that constructed its
/// own would be a plane deciding who may be served.
pub struct HostResidency {
    host: std::sync::Arc<crate::content_host::ContentHost>,
    keys: std::sync::Arc<dyn crate::content_host::ContentKeys>,
    space: mechanics::ids::SpaceId,
    max_content_len: u64,
}

impl HostResidency {
    pub fn new(
        host: std::sync::Arc<crate::content_host::ContentHost>,
        keys: std::sync::Arc<dyn crate::content_host::ContentKeys>,
        space: mechanics::ids::SpaceId,
        max_content_len: u64,
    ) -> Self {
        Self {
            host,
            keys,
            space,
            max_content_len,
        }
    }
}

impl ResidencyOracle for HostResidency {
    fn residency(&self, content: &[u8; 32], wanted: &[u32]) -> ResidencyState {
        if wanted.is_empty() {
            return ResidencyState::Absent;
        }
        // `resident_among` and never `stat`. `stat` walks every chunk and its own
        // comment forbids read-path use; this asks about the indices named and
        // costs one existence check each, so a request cannot be turned into
        // work by being about something large.
        let authorize = |_action: crate::content_host::ContentAction| Ok(());
        let policy = crate::content_host::ContentPolicy {
            space: &self.space,
            keys: self.keys.clone(),
            authorize: &authorize,
            max_content_len: self.max_content_len,
        };
        let held = match self.host.resident_among(
            &policy,
            &replica::content::ContentRef {
                content_id: *content,
            },
            wanted,
        ) {
            Ok(held) => held,
            // Our problem, not the asker's, and the honest hint is the one that
            // sends them elsewhere.
            Err(_) => return ResidencyState::Absent,
        };
        let mut asked: Vec<u32> = wanted.to_vec();
        asked.sort_unstable();
        asked.dedup();
        if held.is_empty() {
            ResidencyState::Absent
        } else if held.len() == asked.len() {
            ResidencyState::Complete
        } else {
            ResidencyState::Partial
        }
    }
}

/// A Station with nothing to answer from.
///
/// `Absent` is the truth here, not a placeholder: a Station holding no content
/// holds none of this content either, and a hint saying so sends the asker to
/// somebody who can help.
pub struct NoResidency;

impl ResidencyOracle for NoResidency {
    fn residency(&self, _content: &[u8; 32], _wanted: &[u32]) -> ResidencyState {
        ResidencyState::Absent
    }
}

/// Send-side coalescing: hold a value briefly, and send the newest.
///
/// Like [`publish`], this has no production caller — the two are the send side,
/// and the send side is driven from above this crate.
///
/// A caret moves as fast as a person types and is superseded by its own next
/// position, so sending each one spends a packet to deliver a number that is
/// already wrong. Holding for a coalescing window and sending the last one is
/// not a loss — the intermediate positions were never the answer to anything.
///
/// Keyed by scope and kind, because two scopes coalescing into one another
/// would be a cursor in one document overwriting a cursor in another.
pub struct Coalescer {
    pending: std::collections::BTreeMap<(TransientScope, u8), (TransientItem, Instant)>,
    superseded: u64,
}

impl Coalescer {
    pub fn new() -> Self {
        Self {
            pending: std::collections::BTreeMap::new(),
            superseded: 0,
        }
    }

    /// Offer an item. It replaces whatever is waiting for the same slot.
    pub fn offer(&mut self, item: TransientItem, now: Instant) {
        let key = (item.scope.clone(), item.payload.kind() as u8);
        if self.pending.contains_key(&key) {
            self.superseded += 1;
        }
        self.pending.insert(key, (item, now));
    }

    /// Everything whose window has elapsed.
    ///
    /// Presence and typing wait longer than a caret, because "somebody is
    /// typing" has no intermediate values worth sending and a caret does.
    pub fn due(&mut self, now: Instant) -> Vec<TransientItem> {
        let ready: Vec<_> = self
            .pending
            .iter()
            .filter(|(_, (item, at))| {
                let window = match item.payload.kind() {
                    crate::transient::TransientKind::Typing => deadline::TYPING_COALESCE,
                    _ => deadline::CURSOR_COALESCE,
                };
                now.duration_since(*at) >= window
            })
            .map(|(key, _)| key.clone())
            .collect();
        ready
            .into_iter()
            .filter_map(|key| self.pending.remove(&key).map(|(item, _)| item))
            .collect()
    }

    /// How many were replaced before they were sent. Coalescing working, not a
    /// loss — but the number an operator wants when a link looks busier than
    /// the people on it.
    pub fn superseded(&self) -> u64 {
        self.superseded
    }

    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }
}

impl Default for Coalescer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod coalescing {
    use super::*;
    use crate::transient::{TransientKind, TransientPayload};
    use std::time::Duration;

    fn scope(n: u8) -> TransientScope {
        TransientScope::IssueView {
            world: "com.example.notes".into(),
            body: [n; 16],
        }
    }

    fn item(scope: TransientScope, seq: u64) -> TransientItem {
        TransientItem {
            session_epoch: [1u8; 16],
            seq,
            scope,
            payload: TransientPayload::Presence,
        }
    }

    #[test]
    fn the_newest_value_replaces_the_one_waiting() {
        // Not a loss: the intermediate positions were never the answer to
        // anything, and sending each one spends a packet to deliver a number
        // that is already wrong.
        let mut coalescer = Coalescer::new();
        let now = Instant::now();
        for seq in 1..=5 {
            coalescer.offer(item(scope(1), seq), now);
        }
        assert_eq!(coalescer.superseded(), 4);

        let due = coalescer.due(now + deadline::CURSOR_COALESCE + Duration::from_millis(1));
        assert_eq!(due.len(), 1, "one slot, one send");
        assert_eq!(due[0].seq, 5, "and it is the newest");
        assert!(coalescer.is_empty());
    }

    #[test]
    fn two_scopes_do_not_coalesce_into_each_other() {
        // A cursor in one document overwriting a cursor in another would be
        // the obvious bug in a single-slot coalescer.
        let mut coalescer = Coalescer::new();
        let now = Instant::now();
        coalescer.offer(item(scope(1), 1), now);
        coalescer.offer(item(scope(2), 1), now);
        assert_eq!(coalescer.superseded(), 0);
        let due = coalescer.due(now + deadline::CURSOR_COALESCE + Duration::from_millis(1));
        assert_eq!(due.len(), 2);
    }

    #[test]
    fn nothing_leaves_before_its_window_elapses() {
        let mut coalescer = Coalescer::new();
        let now = Instant::now();
        coalescer.offer(item(scope(1), 1), now);
        assert!(coalescer.due(now).is_empty(), "sent immediately");
        assert!(!coalescer.is_empty());
    }

    #[test]
    fn typing_waits_longer_than_a_caret() {
        // "Somebody is typing" has no intermediate values worth sending; a
        // caret does. Same mechanism, two windows.
        let mut coalescer = Coalescer::new();
        let now = Instant::now();
        coalescer.offer(
            TransientItem {
                session_epoch: [1u8; 16],
                seq: 1,
                scope: TransientScope::Typing {
                    world: "com.example.notes".into(),
                    body: [1u8; 16],
                    field: "text".into(),
                },
                payload: TransientPayload::Typing,
            },
            now,
        );
        coalescer.offer(item(scope(2), 1), now);

        // A caret's window elapses first, and only the caret leaves.
        let due = coalescer.due(now + deadline::CURSOR_COALESCE + Duration::from_millis(1));
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].payload.kind(), TransientKind::Presence);

        let due = coalescer.due(now + deadline::TYPING_COALESCE + Duration::from_millis(1));
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].payload.kind(), TransientKind::Typing);
    }
}
