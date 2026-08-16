#![allow(
    clippy::arithmetic_side_effects,
    clippy::as_conversions,
    reason = "Live sequence and budget arithmetic is bounded by negotiated protocol limits"
)]
//! The Live plane: one session, and what a peer may make it hold.
//!
//! Freight moves bytes somebody asked for. This moves what people are *doing* —
//! where a cursor is, who is looking at an issue, who is typing — and the whole
//! difference is that none of it is worth retransmitting. A caret that arrives
//! late is wrong, not delayed, so it goes on a datagram and the next one
//! supersedes it.
//!
//! QUIC's bidirectional and unidirectional accept queues are independent. The
//! Live loop polls both: control, signals, and media feedback use bounded bi
//! flows; each media Group gets its own uni stream; transient presence remains
//! a datagram.
//!
//! Three things are bounded before anything is spent, and the order is the
//! bound: a permit before the stream kind is read, the gate before the message,
//! the message's declared length before its buffer.

pub mod media;

use crate::poison::LockRecovering;
use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;
// `tokio::time::Instant`, not `tokio::time::Instant`. Without the `test-util`
// feature it IS `tokio::time::Instant::now()` — same call, same value, no
// indirection — so production pays nothing. With it, `tokio::time::pause()`
// stops the clock for every site at once, which is what lets a test drive a
// sweep interval or a probation window without waiting for one.
use tokio::time::Instant;

use mechanics::station::Key;

use crate::admission::AdmittedPeer;
use crate::budget::{deadline, gates, slots, ByteGate, Gate, Verdict};
use crate::plane::{bounds, datagram_fits, stream_kind};
use crate::plane_stream::{read_framed, read_stream_kind, Invalid as StreamInvalid};
use crate::transient::{
    AdmitOutcome, LiveControl, Target, TransientItem, TransientStore, MAX_TRANSIENT_ITEM_BYTES,
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
pub struct Connection {
    peer: Key,
    store: TransientStore,
    counters: TransientCounters,
    /// The scopes this connection asked to hear about.
    ///
    /// Replace-all rather than incremental: a subscription is a snapshot of
    /// what a client is looking at, and a client that adds and removes views
    /// faster than its messages arrive would otherwise end up subscribed to a
    /// set neither side agrees on.
    subscriptions: Vec<Target>,
}

impl Connection {
    pub fn new(peer: Key) -> Self {
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
    pub fn with_capacity(peer: Key, capacity: usize) -> Self {
        Self {
            peer,
            store: TransientStore::with_capacity(capacity),
            counters: TransientCounters::default(),
            subscriptions: Vec::new(),
        }
    }

    pub fn peer(&self) -> &Key {
        &self.peer
    }

    pub fn counters(&self) -> &TransientCounters {
        &self.counters
    }

    pub fn subscriptions(&self) -> &[Target] {
        &self.subscriptions
    }

    /// Adopt a subscription snapshot.
    pub fn subscribe(&mut self, scopes: Vec<Target>) {
        self.subscriptions = scopes;
    }

    /// What this session currently believes, for a scope it subscribed to.
    ///
    /// Intersected with the subscription set rather than answered from the
    /// store alone: a peer that stopped watching something must stop hearing
    /// about it, and a store lookup that ignored the subscription would keep
    /// delivering.
    pub fn is_watching(&self, scope: &Target) -> bool {
        self.subscriptions.contains(scope)
    }

    /// Offer one item this peer sent.
    pub fn admit(&mut self, item: &TransientItem, epoch: &[u8; 16], now: Instant) -> AdmitOutcome {
        let outcome = self.store.admit(item, epoch, now);
        match &outcome {
            AdmitOutcome::Evicted => self.counters.evictions += 1,
            AdmitOutcome::WrongEpoch => self.counters.wrong_epoch += 1,
            AdmitOutcome::Retired => self.counters.retired += 1,
            AdmitOutcome::Refused(crate::transient::Invalid::Bounds) => {
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
/// and the Replica holds a `dyn Engine + Send` that is not. `with_replica`
/// records the arithmetic that bounds it.
pub trait AnchorSource: Send + Sync {
    /// Mint an anchor at a position, so a browser that can only send an offset
    /// has something that survives concurrent edits.
    fn anchor_in_body(
        &self,
        key: &replica::body::BodyKey,
        path: &str,
        position: u64,
    ) -> Option<fabric::Anchor>;

    /// Where that position is now. Total: never an error, never a mutation,
    /// and never a silently wrong index.
    fn resolve_anchor(
        &self,
        key: &replica::body::BodyKey,
        anchor: &fabric::Anchor,
    ) -> fabric::AnchorResolution;
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
    pub station: Key,
    pub scope: Target,
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
    /// A display-only text splice, resolved by the viewer against its durable
    /// Markdown revision rather than by the CRDT anchor source.
    pub preview: Option<crate::transient::TextPreview>,
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
    /// The next age/partial transition that changes the projection without a
    /// new item. A streaming reader sleeps exactly this long rather than
    /// polling to discover that a caret became uncertain.
    pub refresh_in: Option<Duration>,
}

/// The cross-thread half of the Live plane.
///
/// The driver writes it from its own thread; the daemon and browser session
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
    table_updates: tokio::sync::watch::Sender<u64>,
    signals: SignalSink,
    /// Validated native-media records received on the media lane pair.
    media: media::Inbox,
    anchors: Option<std::sync::Arc<dyn AnchorSource>>,
    residency: std::sync::Arc<dyn ResidencyOracle>,
    /// What this Station is currently doing, for peers to be told about.
    ///
    /// Separate from `table`, which holds what *others* are doing. One map for
    /// both would make "who is here" a question whose answer includes us, and
    /// every viewer would draw itself a second time.
    local: std::sync::Mutex<LocalPresence>,
    local_updates: tokio::sync::watch::Sender<u64>,
    /// Peers holding a session right now, refcounted.
    ///
    /// Refcounted because `MAX_LIVE_SESSIONS_PER_STATION` is two: a peer with a
    /// laptop and a phone is here twice, and the first one to hang up has not
    /// left. On the handle rather than on the service because both the accepting
    /// and the dialling path make sessions, and a reader asking "who is here"
    /// must not get a different answer depending on who dialled.
    connected: std::sync::Mutex<std::collections::BTreeMap<Key, usize>>,
    /// Signals waiting for the session that can carry them.
    ///
    /// Addressed by Station, because a signal is for a person and a person is
    /// reachable only through the connections they hold. A signal for a peer
    /// with no session is not queued for later: presence is the whole gate, and
    /// somebody who is not here has the durable record as their path.
    outbox: std::sync::Mutex<std::collections::BTreeMap<Key, Vec<crate::plane::Signal>>>,
    /// Files somebody offered, waiting for a person.
    ///
    /// Beside the transient table rather than in it: a slot expires on a TTL
    /// because a cursor that stopped moving is stale, and an offer that has been
    /// sitting for an hour is exactly as valid as it was when it arrived.
    offers: std::sync::Mutex<crate::signal::OfferQueue>,
}

/// One thing this Station currently wants its peers to see.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalPublication {
    pub scope: Target,
    pub payload: crate::transient::TransientPayload,
}

/// What this Station is doing, as the thing that decides it sees it.
#[derive(Default)]
struct LocalPresence {
    publications: Vec<LocalPublication>,
    generation: u64,
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
    slots: std::collections::BTreeMap<(Key, Target, u8), crate::transient::TransientSlot>,
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
            table_updates: tokio::sync::watch::Sender::new(0),
            signals: tokio::sync::broadcast::channel(SIGNAL_QUEUE).0,
            media: media::Inbox::new(),
            anchors,
            residency,
            local: std::sync::Mutex::new(LocalPresence::default()),
            local_updates: tokio::sync::watch::Sender::new(0),
            connected: std::sync::Mutex::new(std::collections::BTreeMap::new()),
            outbox: std::sync::Mutex::new(std::collections::BTreeMap::new()),
            offers: std::sync::Mutex::new(crate::signal::OfferQueue::new()),
        }
    }

    /// Listen for validated native-media control records and complete Groups.
    ///
    /// The receiver is bounded. Falling behind yields broadcast lag rather
    /// than making the Live driver retain an unbounded history; a consumer then
    /// resubscribes at the newest Group.
    pub fn media(&self) -> tokio::sync::broadcast::Receiver<media::Event> {
        self.media.subscribe()
    }

    /// Listen for native media and atomically include every already-connected
    /// session. State-oriented consumers such as the display coordinator use
    /// this form so attaching after a source starts does not require that
    /// source to reconnect.
    pub fn media_with_sessions(
        &self,
    ) -> (
        Vec<media::Session>,
        tokio::sync::broadcast::Receiver<media::Event>,
    ) {
        self.media.subscribe_with_sessions()
    }

    /// Say what this Station is looking at.
    ///
    /// **Replace-all**, like a peer's `Subscribe` and for the same reason: this
    /// is a snapshot of what somebody has open, and an incremental protocol
    /// would let a client that navigates faster than its messages arrive end up
    /// publishing a set neither side agrees on.
    ///
    /// Declaring is not sending. Each live session works out its own difference
    /// against what *it* has published and mints its own items, because an item
    /// carries the epoch of the connection it crosses and two sessions do not
    /// share one.
    pub fn declare_local(&self, scopes: Vec<Target>) {
        self.declare_local_publications(
            scopes
                .into_iter()
                .map(|scope| LocalPublication {
                    scope,
                    payload: crate::transient::TransientPayload::Presence,
                })
                .collect(),
        );
    }

    /// Replace everything this Station publishes: presence, carets, selections,
    /// and typing. The caller has already minted anchors and bounded the set.
    pub fn declare_local_publications(&self, publications: Vec<LocalPublication>) {
        let mut local = self.local();
        if local.publications == publications {
            return;
        }
        local.publications = publications;
        // A counter a session can compare against cheaply. Without it every
        // session would clone and compare the whole set on every beat, at the
        // beat rate, for a set that changes when somebody opens a tab.
        local.generation = local.generation.wrapping_add(1);
        self.local_updates.send_replace(local.generation);
    }

    /// The number that moves when the declaration changes.
    ///
    /// Separate from `local_publications` so the common case — a beat on which
    /// nothing changed — costs a `u64` read rather than cloning the whole scope
    /// set, which is the cost the counter was added to avoid and which reading
    /// them together reintroduced.
    pub fn local_generation(&self) -> u64 {
        self.local().generation
    }

    /// Wake a Live session as soon as this Station's declaration changes.
    pub fn subscribe_local_generation(&self) -> tokio::sync::watch::Receiver<u64> {
        self.local_updates.subscribe()
    }

    /// Wake a local projection as soon as a peer's Live table changes.
    pub fn subscribe_generation(&self) -> tokio::sync::watch::Receiver<u64> {
        self.table_updates.subscribe()
    }

    fn bump_table(&self, table: &mut PublishTable) {
        table.generation = table.generation.wrapping_add(1);
        self.table_updates.send_replace(table.generation);
    }

    /// Which scopes this Station is looking at.
    ///
    /// Kept as the scope-only inspection API it was before cursors became a
    /// local publication. Session code uses `local_publications` to retain the
    /// payload paired with each scope.
    pub fn declared(&self) -> Vec<Target> {
        self.local()
            .publications
            .iter()
            .map(|publication| publication.scope.clone())
            .collect()
    }

    fn local_publications(&self) -> Vec<LocalPublication> {
        self.local().publications.clone()
    }

    fn local(&self) -> std::sync::MutexGuard<'_, LocalPresence> {
        self.local.lock_recovering()
    }

    /// A session for this peer opened.
    pub fn arrived(&self, peer: &Key) {
        *self
            .connected
            .lock_recovering()
            .entry(peer.clone())
            .or_insert(0) += 1;
    }

    /// A session for this peer ended.
    ///
    /// **Clears the outbox when the last one goes**, in the same place rather
    /// than in a second call somebody has to remember. A queue for a peer with
    /// no session is store-and-forward: nothing drains it, it fills to its
    /// ceiling and starts refusing real nudges, and the next session that peer
    /// opens delivers the backlog — which is the mailbox this plane must not
    /// become. Keeping the two in one method is what stops the invariant being
    /// half-maintained, which is exactly how it was.
    pub fn departed(&self, peer: &Key) {
        let gone = {
            let mut connected = self.connected.lock_recovering();
            match connected.get_mut(peer) {
                Some(count) => {
                    *count = count.saturating_sub(1);
                    if *count == 0 {
                        connected.remove(peer);
                        true
                    } else {
                        false
                    }
                }
                None => false,
            }
        };
        if gone {
            self.outbox.lock_recovering().remove(peer);
        }
    }

    /// Who is reachable right now.
    ///
    /// Not "who is a member" and not "who has said something" — who this Station
    /// currently holds a session with. That is the only set a signal can be
    /// delivered to, and the reason presence can gate delivery at all.
    pub fn present_stations(&self) -> Vec<Key> {
        self.connected.lock_recovering().keys().cloned().collect()
    }

    /// Hand a signal to whichever session reaches that peer.
    ///
    /// Returns whether it was taken. `false` is a full outbox, and the *newest*
    /// is refused rather than the oldest evicted: an outbox is not a cursor
    /// stream, and dropping what is already queued loses the older fact to keep
    /// the newer one — which is backwards when both are facts.
    ///
    /// A peer with no session is refused rather than queued. Presence is the
    /// gate: somebody who is not here has the durable record as their path, and
    /// holding signals for them would make this a mailbox, which is the one
    /// thing a plane that keeps nothing must not become.
    ///
    /// The check is here and not only at the call site, because this is where the
    /// queue is. The doc said this for a while before the code did.
    pub fn nudge(&self, peer: &Key, signal: crate::plane::Signal) -> bool {
        if !self.connected.lock_recovering().contains_key(peer) {
            return false;
        }
        // An outbox is one-way by construction, and a signal that expects an
        // answer has nobody here to give it one: the queue is drained by a
        // session beat, so waiting on a round trip would park that session for a
        // full response deadline — no datagram read, no presence published, no
        // revalidation — on behalf of a caller that has already returned.
        //
        // Every signal a World produces is one-way, so this refuses nothing that
        // is sent today. It is here because the queue accepts a `Signal` and the
        // next person to reach for it should not have to discover this.
        if crate::signal::declaration_for(signal.selector()).is_none_or(|declaration| {
            declaration.response != crate::signal::ResponsePolicy::Forbidden
        }) {
            return false;
        }
        let mut outbox = self.outbox.lock_recovering();
        let queued = outbox.entry(peer.clone()).or_default();
        if queued.len() >= MAX_OUTBOUND_SIGNALS {
            return false;
        }
        queued.push(signal);
        true
    }

    /// `take_outbound`, for the fixtures that pin the outbox rules.
    ///
    /// The real one is private because draining belongs to the session that can
    /// carry what it drains — a second caller would take signals nothing then
    /// sends.
    pub fn take_outbound_for_test(&self, peer: &Key) -> Vec<crate::plane::Signal> {
        self.take_outbound(peer, MAX_OUTBOUND_SIGNALS)
    }

    /// Take at most `max` of what is waiting for one peer.
    ///
    /// Bounded because the caller sends them **inline on its session beat**, and
    /// each send is deadlined: draining a full outbox in one pass would put
    /// sixteen deadlines end to end, which is longer than the session's own idle
    /// timeout and many times the Station's drain deadline. A beat that spends
    /// minutes is a beat that reads no datagram, publishes no presence and
    /// revalidates no authority.
    ///
    /// Oldest first, and what is not taken stays queued for the next beat.
    fn take_outbound(&self, peer: &Key, max: usize) -> Vec<crate::plane::Signal> {
        let mut outbox = self.outbox.lock_recovering();
        let Some(queued) = outbox.get_mut(peer) else {
            return Vec::new();
        };
        let taken: Vec<_> = queued.drain(..queued.len().min(max)).collect();
        if queued.is_empty() {
            outbox.remove(peer);
        }
        taken
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
        from: &Key,
        content: &[u8; 32],
    ) -> Option<crate::signal::PendingOffer> {
        self.offers().take(from, content)
    }

    /// Drop everything one peer offered. What a revocation does — and only a
    /// revocation: a peer whose laptop slept is still somebody whose file offer
    /// is worth keeping.
    pub fn forget_offers(&self, from: &Key) -> usize {
        self.offers().forget(from)
    }

    fn offers(&self) -> std::sync::MutexGuard<'_, crate::signal::OfferQueue> {
        self.offers.lock_recovering()
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
        self.table.lock_recovering()
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
    pub fn record(&self, station: &Key, item: &TransientItem, now: Instant) -> bool {
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
        self.bump_table(&mut table);
        table.slots.insert(
            key,
            crate::transient::TransientSlot {
                connection_epoch: item.connection_epoch,
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
    pub fn forget_session(&self, station: &Key, connection_epoch: &[u8; 16]) -> usize {
        let mut table = self.table();
        let before = table.slots.len();
        table.slots.retain(|(held, _, _), slot| {
            held != station || &slot.connection_epoch != connection_epoch
        });
        let dropped = before - table.slots.len();
        if dropped > 0 {
            self.bump_table(&mut table);
        }
        dropped
    }

    /// Drop everything a Station held, whichever session put it there.
    ///
    /// What a revocation does. Membership is per peer rather than per
    /// connection, so a peer that lost it keeps nothing on any of its sessions.
    pub fn forget(&self, station: &Key) -> usize {
        let mut table = self.table();
        let before = table.slots.len();
        table.slots.retain(|(held, _, _), _| held != station);
        let dropped = before - table.slots.len();
        if dropped > 0 {
            self.bump_table(&mut table);
        }
        dropped
    }

    /// Drop one peer's slots for one scope.
    pub fn retire(&self, station: &Key, scope: &Target) {
        let mut table = self.table();
        let before = table.slots.len();
        table
            .slots
            .retain(|(held, held_scope, _), _| held != station || held_scope != scope);
        if table.slots.len() != before {
            self.bump_table(&mut table);
        }
    }

    /// Say whether this Station is at its ceiling for sessions it *accepts*.
    pub fn set_accepting_capped(&self, capped: bool) {
        let mut table = self.table();
        if table.accepting_capped != capped {
            table.accepting_capped = capped;
            self.bump_table(&mut table);
        }
    }

    /// Say whether this Station is at its ceiling for sessions it *dials*.
    pub fn set_dialling_capped(&self, capped: bool) {
        let mut table = self.table();
        if table.dialling_capped != capped {
            table.dialling_capped = capped;
            self.bump_table(&mut table);
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
            self.bump_table(&mut table);
        }
        table.dropped_until = Some(until);
    }

    /// Drop what has expired. Nothing depends on when this runs.
    pub fn sweep(&self, now: Instant) -> usize {
        let mut table = self.table();
        let before = table.slots.len();
        table.slots.retain(|(_, scope, _), slot| {
            let ttl = match scope {
                Target::Body { .. } | Target::Material { .. } | Target::World { .. } => {
                    deadline::PRESENCE_TTL
                }
                _ => deadline::CURSOR_TTL,
            };
            now.duration_since(slot.arrived_at) < ttl
        });
        let dropped = before - table.slots.len();
        if dropped > 0 {
            self.bump_table(&mut table);
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
    /// `scope` narrows to that **exact** scope, which is rarely what a reader
    /// looking at a document wants — see [`LiveNarrow`]. Kept because a caller
    /// that genuinely wants one kind should not have to say so twice.
    pub fn view(&self, scope: Option<&Target>, now: Instant) -> LiveView {
        self.view_narrowed(scope.map_or(LiveNarrow::Everything, LiveNarrow::Scope), now)
    }

    /// Everything currently believed about what a reader asked for.
    ///
    /// Resolution happens here, per read, and the answer is never written back
    /// into a slot.
    pub fn view_narrowed(&self, narrow: LiveNarrow<'_>, now: Instant) -> LiveView {
        // Snapshotted, then the lock is released. Resolving under it would take
        // the commit lock while holding a lock the browser can take.
        let (generation, partial, held, refresh_in) = {
            let table = self.table();
            let held: Vec<_> = table
                .slots
                .iter()
                .filter(|((_, held_scope, _), _)| narrow.admits(held_scope))
                .map(|((station, held_scope, _), slot)| {
                    (station.clone(), held_scope.clone(), slot.clone())
                })
                .collect();
            let age_refresh = table
                .slots
                .values()
                .filter_map(|slot| {
                    let age = now.saturating_duration_since(slot.arrived_at);
                    deadline::CARET_GRACE
                        .checked_sub(age)
                        .map(|remaining| remaining + Duration::from_nanos(1))
                })
                .min();
            let partial_refresh = table
                .dropped_until
                .and_then(|until| (until > now).then(|| until - now));
            (
                table.generation,
                table.partial(now),
                held,
                age_refresh.into_iter().chain(partial_refresh).min(),
            )
        };

        let entries = held
            .into_iter()
            .map(|(station, scope, slot)| {
                let age = now.saturating_duration_since(slot.arrived_at);
                let (caret, focus, preview) = self.resolve(&scope, &slot.payload);
                LiveEntry {
                    station,
                    kind: slot.payload.kind(),
                    scope,
                    age_ms: age.as_millis() as u64,
                    uncertain: age > deadline::CARET_GRACE,
                    caret,
                    focus,
                    preview,
                }
            })
            .collect();
        LiveView {
            generation,
            partial,
            entries,
            refresh_in,
        }
    }

    fn resolve(
        &self,
        scope: &Target,
        payload: &crate::transient::TransientPayload,
    ) -> (
        Option<CaretState>,
        Option<CaretState>,
        Option<crate::transient::TextPreview>,
    ) {
        use crate::transient::TransientPayload;
        let (anchor, focus) = match payload {
            TransientPayload::Caret { anchor } => (Some(anchor), None),
            TransientPayload::Selection { anchor, focus } => (Some(anchor), Some(focus)),
            TransientPayload::Preview { preview } => {
                return (None, None, Some(preview.clone()));
            }
            _ => return (None, None, None),
        };
        let Some((world, body)) = scope_body(scope) else {
            return (
                Some(CaretState::Unresolved),
                focus.map(|_| CaretState::Unresolved),
                None,
            );
        };
        let Some(key) = body_key(&world, body) else {
            return (
                Some(CaretState::Unresolved),
                focus.map(|_| CaretState::Unresolved),
                None,
            );
        };
        let one = |raw: &Vec<u8>| -> CaretState {
            let Some(source) = self.anchors.as_ref() else {
                return CaretState::Unresolved;
            };
            // A stored anchor was validated on the way in, so a decode failure
            // here is this Station's bug rather than a peer's — and the honest
            // answer is still "no position", never a guess.
            let Ok(decoded) = fabric::Anchor::decode_canonical(raw) else {
                return CaretState::Unresolved;
            };
            match source.resolve_anchor(&key, &decoded) {
                fabric::AnchorResolution::Resolved(at) => CaretState::At(at),
                fabric::AnchorResolution::Drifted => CaretState::Drifted,
            }
        };
        (anchor.map(&one), focus.map(&one), None)
    }
}

/// The Body a scope names, when it names one.
fn scope_body(scope: &Target) -> Option<(String, [u8; 16])> {
    match scope {
        Target::Body { world, body }
        | Target::Material { world, body }
        | Target::Field { world, body, .. }
        | Target::Preview { world, body, .. }
        | Target::Typing { world, body, .. } => Some((world.clone(), *body)),
        _ => None,
    }
}

/// What a reader is asking about.
///
/// The distinction that matters is `Body` versus `Scope`, and getting it wrong
/// is silent. A viewer looking at an issue wants everything about that Body —
/// who is present, where their carets are, who is typing — and those live under
/// three *different* scopes. Narrowing by scope equality answers with presence
/// alone and looks exactly like a document nobody has a cursor in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiveNarrow<'a> {
    /// Everything this Station believes.
    Everything,
    /// Everything about one Body, whatever scope carries it.
    Body { world: &'a str, body: [u8; 16] },
    /// One exact scope, for a reader that genuinely wants one kind.
    Scope(&'a Target),
}

impl LiveNarrow<'_> {
    /// Whether this narrowing gathers that scope.
    ///
    /// Public because the daemon narrows a browser's question the same way and
    /// tests it there. Two implementations of "is this scope about that Body"
    /// is how the browser and the plane come to disagree about which document
    /// somebody is looking at.
    pub fn admits(&self, scope: &Target) -> bool {
        match self {
            Self::Everything => true,
            Self::Scope(want) => &scope == want,
            Self::Body { world, body } => {
                scope_body(scope).is_some_and(|(w, b)| w == *world && b == *body)
            }
        }
    }
}

fn body_key(world: &str, body: [u8; 16]) -> Option<replica::body::BodyKey> {
    Some(replica::body::BodyKey::new(
        replica::body::WorldId::parse(world)?,
        replica::body::BodyId::from_bytes(body),
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

/// How many queued signals one session beat will send.
///
/// Two, because each send is deadlined and they run inline: this bounds one beat
/// at two deadlines rather than sixteen. What is left waits for the next beat,
/// which is twenty-five milliseconds away, so a backlog still clears in well
/// under a second.
const SIGNALS_PER_BEAT: usize = 2;

/// How many signals wait for one peer before the newest are refused.
///
/// Small, because these are person-scale facts about one person and a peer that
/// has fallen this far behind is not going to be caught up by a longer queue.
/// The durable record behind each one is unaffected either way.
const MAX_OUTBOUND_SIGNALS: usize = 16;

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
pub struct Context {
    /// Where what this peer says becomes readable to anyone else.
    pub handle: Option<std::sync::Arc<LiveHandle>>,
    /// What this connection may say on the signal lane. `None` means this build
    /// is not serving the lane here, and a signal flow is refused rather than
    /// ignored.
    pub signals: Option<crate::signal::SignalPolicy>,
    /// The Worlds this build hosts, for checking a `World` scope against
    /// what its World actually declared.
    ///
    /// `None` means no check, which is the shape a MemNet harness with no Space
    /// behind it runs in. It is not a licence: a Station always has a registry,
    /// so the permissive case never reaches production.
    pub worlds: Option<crate::registry::Catalog>,
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
    context: Context,
) {
    let Context {
        handle,
        signals,
        worlds,
        authority,
    } = context;
    let session = Rc::new(RefCell::new(Connection::new(peer.station.clone())));
    let epoch = peer.connection_epoch;
    // Whatever ends this connection — idle, cancel, a gate, a peer hanging up —
    // the slots go with it. Presence has no goodbye it can rely on, so the
    // session ending *is* the goodbye.
    if let Some(handle) = &handle {
        handle.arrived(&peer.station);
    }
    let leaving = Leaving {
        station: peer.station.clone(),
        connection_epoch: epoch,
        handle: handle.clone(),
    };
    let _leaving = leaving;

    let media_enabled = peer.features & crate::plane::feature::NATIVE_LIVE_MEDIA != 0
        && peer.granted_lanes.contains(&stream_kind::MEDIA_GROUP)
        && peer.granted_lanes.contains(&stream_kind::MEDIA_CONTROL);
    let media_session = media::Session::new(
        peer.station.clone(),
        peer.connection_id,
        std::sync::Arc::clone(&connection),
        media_enabled,
    );
    if media_enabled
        && media_session
            .ensure_setup(media::Setup {
                protocol_version: media::PROTOCOL_VERSION,
                max_group_duration_ms: media::DEFAULT_MAX_GROUP_DURATION_MS,
                max_latency_ms: media::DEFAULT_MAX_LATENCY_MS,
            })
            .await
            .is_err()
    {
        connection.close(media::RESET_MEDIA, b"");
        return;
    }
    let _media_registration = if media_enabled {
        handle
            .as_ref()
            .map(|handle| handle.media.register(media_session.clone()))
    } else {
        None
    };

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
    // Owned permits let a media Group move into its own local task *after* the
    // permit was taken. Taking it before spawning is the bound: a stream flood
    // cannot create an unbounded ready-task queue in front of the semaphore.
    let workers = std::sync::Arc::new(tokio::sync::Semaphore::new(bounds::MAX_STREAM_WORKERS));

    let mut last_seen = Instant::now();
    let mut sweep_at = Instant::now();
    let mut revalidated_at = Instant::now();
    let mut local_updates = handle
        .as_ref()
        .map(|handle| handle.subscribe_local_generation());

    // What this session has told its peer we are looking at, and the
    // declaration generation it was built from.
    //
    // Per session rather than on the handle. Two sessions publish the same
    // scopes at different sequence numbers under different epochs, so the
    // difference between "what we are doing" and "what this peer has been told"
    // is a fact about the connection and belongs with it.
    let mut published: std::collections::BTreeMap<
        (Target, u8),
        crate::transient::TransientPayload,
    > = std::collections::BTreeMap::new();
    let mut published_generation = u64::MAX;
    // `Instant::now() - PRESENCE_REFRESH` panics when the machine has been up for
    // less than the refresh interval, which is a real state on a freshly booted
    // node and on the CI runners this is tested on. The saturating form makes the
    // first beat due, which is what was wanted.
    let mut refreshed_at = Instant::now()
        .checked_sub(deadline::PRESENCE_REFRESH)
        .unwrap_or_else(Instant::now);
    // While set, the declared scopes are re-published on a fast beat. See
    // `deadline::PRESENCE_SETTLE` for why a one-shot publish is not enough.
    let mut settle_until: Option<Instant> = None;
    let mut settled_at = Instant::now();
    let mut outbound_seq: u64 = 0;

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
        // Signals waiting for this peer. Sent before presence, because a signal
        // is a fact somebody acts on and presence is a picture that the next
        // beat redraws anyway.
        if let Some(handle) = &handle {
            for signal in handle.take_outbound(&peer.station, SIGNALS_PER_BEAT) {
                // A failure is not requeued. The durable record behind every
                // signal is already committed and already converging, so a lost
                // nudge costs timeliness and nothing else — and a retry queue
                // here is the mailbox this plane must not become.
                let _ = crate::signal::send_signal(connection.as_ref(), &signal).await;
            }
        }
        // Told when it changes, and told again before it expires. The second
        // half is not belt and braces: a slot dies on the *receiver's* clock, so
        // a peer that heard once and never again watches everybody vanish after
        // a minute and a half.
        if let Some(handle) = &handle {
            let generation = handle.local_generation();
            let due = refreshed_at.elapsed() >= deadline::PRESENCE_REFRESH;
            if generation != published_generation || due {
                let want: std::collections::BTreeMap<_, _> = handle
                    .local_publications()
                    .into_iter()
                    .map(|publication| {
                        (
                            (publication.scope, publication.payload.kind() as u8),
                            publication.payload,
                        )
                    })
                    .collect();
                let want_scopes: std::collections::BTreeSet<_> =
                    want.keys().map(|(scope, _)| scope.clone()).collect();
                let published_scopes: std::collections::BTreeSet<_> =
                    published.keys().map(|(scope, _)| scope.clone()).collect();
                // The subscription *is* the declaration, and it has to go first.
                // A receiver drops a datagram for a scope this connection never
                // subscribed to — that is what stops a peer making a Station
                // hold state on its behalf — so presence published without it is
                // presence silently discarded.
                //
                // One set for both directions because on this plane they are the
                // same fact: having a document open is both "tell me about this"
                // and "I am here".
                //
                // Re-sent on every refresh as well as on every change, and that
                // is not redundancy. A subscription this side believes it sent
                // and the peer never received is otherwise permanent: presence
                // keeps being published, keeps being dropped on arrival, and
                // nothing ever re-states the subscription because from here
                // nothing changed. Repeating it on the refresh beat bounds that
                // to one interval.
                // Nothing sent means nothing recorded as sent: leaving
                // `published_generation` alone makes the next beat try again,
                // rather than this session spending its life publishing into a
                // subscription that was never made.
                //
                // `false` rather than `continue`, which is what this was. The
                // only thing in this loop that sleeps is the `select!` below, so
                // continuing past it turned a connection whose subscribe fails
                // fast into a busy spin for the whole idle timeout.
                let subscribed = !(want_scopes != published_scopes || due)
                    || subscribe_remotely(connection.as_ref(), &want_scopes).await;
                if subscribed {
                    // Retirement is scope-wide. When a collapsed caret becomes a
                    // selection (or back), retire the old kind first and then
                    // republish the current one so a peer never draws both.
                    let changed_kinds: std::collections::BTreeSet<_> = want_scopes
                        .intersection(&published_scopes)
                        .filter(|scope| {
                            let wanted: Vec<_> = want
                                .keys()
                                .filter(|(held, _)| held == *scope)
                                .map(|(_, kind)| *kind)
                                .collect();
                            let sent: Vec<_> = published
                                .keys()
                                .filter(|(held, _)| held == *scope)
                                .map(|(_, kind)| *kind)
                                .collect();
                            wanted != sent
                        })
                        .cloned()
                        .collect();
                    for scope in published_scopes
                        .difference(&want_scopes)
                        .chain(changed_kinds.iter())
                    {
                        outbound_seq += 1;
                        retire_remotely(connection.as_ref(), scope, outbound_seq).await;
                    }
                    for ((scope, _), payload) in &want {
                        let changed =
                            published.get(&(scope.clone(), payload.kind() as u8)) != Some(payload);
                        if due || changed || changed_kinds.contains(scope) {
                            outbound_seq += 1;
                            publish_payload(
                                connection.as_ref(),
                                scope,
                                payload,
                                &epoch,
                                outbound_seq,
                                &mut session.borrow_mut().counters,
                            );
                        }
                    }
                    if due {
                        refreshed_at = Instant::now();
                    }
                    published = want;
                    published_generation = generation;
                    if !published.is_empty() {
                        settle_until = Some(Instant::now() + deadline::PRESENCE_SETTLE);
                    }
                }
            }

            // The settle beat. Cheap, bounded, and the only thing standing
            // between a lost race and a colleague who does not appear for
            // twenty-five seconds.
            if settle_until.is_some_and(|until| Instant::now() < until)
                && settled_at.elapsed() >= deadline::TYPING_COALESCE
            {
                settled_at = Instant::now();
                for ((scope, _), payload) in &published {
                    outbound_seq += 1;
                    publish_payload(
                        connection.as_ref(),
                        scope,
                        payload,
                        &epoch,
                        outbound_seq,
                        &mut session.borrow_mut().counters,
                    );
                }
            }
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
                if admits_scope(&worlds, &item.scope).is_err() {
                    continue;
                }
                if matches!(item.scope, Target::Content { .. })
                    && peer.features & crate::plane::feature::RESIDENCY_HINTS == 0
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

            accepted = connection.accept_uni() => {
                let Ok(Some(mut recv)) = accepted else { break };
                last_seen = Instant::now();
                match accept_gate.check(Instant::now()) {
                    Verdict::Allow => {}
                    Verdict::Drop => {
                        recv.stop(REFUSED);
                        continue;
                    }
                    Verdict::Close => {
                        connection.close(REFUSED, b"");
                        break;
                    }
                }
                let Ok(permit) = std::sync::Arc::clone(&workers).try_acquire_owned() else {
                    recv.stop(REFUSED);
                    continue;
                };
                let kind = match tokio::time::timeout(
                    deadline::LIVE_FLOW_READ,
                    read_stream_kind(recv.as_mut()),
                )
                .await
                {
                    Ok(Ok(kind)) => kind,
                    _ => {
                        recv.stop(REFUSED);
                        continue;
                    }
                };
                if kind != stream_kind::MEDIA_GROUP
                    || !peer.granted_lanes.contains(&stream_kind::MEDIA_GROUP)
                {
                    recv.stop(REFUSED);
                    continue;
                }
                let Some(handle) = handle.clone() else {
                    recv.stop(REFUSED);
                    continue;
                };
                let station = peer.station.clone();
                let connection_id = peer.connection_id;
                let media_session = media_session.clone();
                tokio::task::spawn_local(async move {
                    let _permit = permit;
                    match tokio::time::timeout(
                        deadline::MEDIA_GROUP_READ,
                        async {
                            let header = media::read_group_header(recv.as_mut()).await?;
                            let mut active = media_session.begin_received_group(&header).await?;
                            tokio::select! {
                                group = media::read_group_frames(recv.as_mut(), header) => group,
                                () = active.until_stale() => Err(media::Invalid::StaleGroup),
                            }
                        },
                    )
                    .await
                    {
                        Ok(Ok(group)) => {
                            if group.header.track_kind == media::TrackKind::Catalog
                                && media_session.accept_catalog(&group).is_err()
                            {
                                recv.stop(media::RESET_MEDIA);
                                return;
                            }
                            handle.media.publish(media::Event {
                                peer: station,
                                connection_id,
                                session: media_session,
                                body: media::EventBody::Group(std::sync::Arc::new(group)),
                            });
                        }
                        _ => recv.stop(media::RESET_MEDIA),
                    }
                });
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
                let Ok(_permit) = std::sync::Arc::clone(&workers).try_acquire_owned() else {
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
                    Ok(Err(StreamInvalid::UnknownKind(_))) => {
                        drop(send);
                        continue;
                    }
                    Ok(Err(_)) => {
                        drop(send);
                        continue;
                    }
                };
                if !peer.granted_lanes.contains(&kind) {
                    crate::signal::refuse_flow(send.as_mut(), recv.as_mut());
                    continue;
                }
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
                        if let crate::plane::Signal::FileOffer {
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
                                connection_epoch: peer.connection_epoch,
                                content: *content,
                                plaintext_len: *plaintext_len,
                                display_name: display_name.clone(),
                                media_type: media_type.clone(),
                            });
                        }
                        handle.deliver(crate::signal::DeliveredSignal {
                            from: peer.station.clone(),
                            connection_id: peer.connection_id,
                            connection_epoch: peer.connection_epoch,
                            signal,
                        });
                    }
                    continue;
                }
                if kind == stream_kind::MEDIA_CONTROL {
                    match control_gate.check(Instant::now()) {
                        Verdict::Allow => {}
                        Verdict::Drop => {
                            crate::signal::refuse_flow(send.as_mut(), recv.as_mut());
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
                        crate::signal::refuse_flow(send.as_mut(), recv.as_mut());
                        continue;
                    };
                    if matches!(
                        control_bytes.check(Instant::now(), body.len()),
                        Verdict::Close
                    ) {
                        connection.close(REFUSED, b"");
                        break;
                    }
                    let Ok(control) = media::Control::decode_canonical(&body) else {
                        crate::signal::refuse_flow(send.as_mut(), recv.as_mut());
                        continue;
                    };
                    match control {
                        media::Control::Fetch(request) => {
                            if !matches!(
                                tokio::time::timeout(
                                    deadline::LIVE_FLOW_READ,
                                    recv.read_chunk(1),
                                )
                                .await,
                                Ok(Ok(None))
                            ) {
                                crate::signal::refuse_flow(send.as_mut(), recv.as_mut());
                                continue;
                            }
                            let Ok(responder) = media_session.accept_fetch(request, send) else {
                                recv.stop(media::RESET_MEDIA);
                                continue;
                            };
                            if let Some(handle) = &handle {
                                handle.media.publish(media::Event {
                                    peer: peer.station.clone(),
                                    connection_id: peer.connection_id,
                                    session: media_session.clone(),
                                    body: media::EventBody::Fetch(responder),
                                });
                            } else {
                                let _ = responder.refuse().await;
                            }
                        }
                        control => {
                            if media_session.accept_control(&control).is_err() {
                                crate::signal::refuse_flow(send.as_mut(), recv.as_mut());
                                continue;
                            }
                            if let Some(handle) = &handle {
                                handle.media.publish(media::Event {
                                    peer: peer.station.clone(),
                                    connection_id: peer.connection_id,
                                    session: media_session.clone(),
                                    body: media::EventBody::Control(control),
                                });
                            }
                            let _ = send.finish();
                        }
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
                        // A scope its World never declared is refused here
                        // rather than at publish time, so it never occupies one
                        // of this connection's subscription slots.
                        session.subscribe(
                            scopes
                                .into_iter()
                                .filter(|scope| admits_scope(&worlds, scope).is_ok())
                                .collect(),
                        )
                    }
                    LiveControl::Retire { scope, seq } => {
                        // Retirement covers every kind that scope admits: a
                        // peer saying it is done with a caret means the caret
                        // and the selection, not whichever one it named.
                        for kind in [
                            crate::transient::TransientKind::Presence,
                            crate::transient::TransientKind::Caret,
                            crate::transient::TransientKind::Selection,
                            crate::transient::TransientKind::Preview,
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

            changed = async {
                match local_updates.as_mut() {
                    Some(updates) => updates.changed().await,
                    None => std::future::pending::<
                        Result<(), tokio::sync::watch::error::RecvError>
                    >().await,
                }
            } => {
                if changed.is_err() {
                    break;
                }
            }

            // Housekeeping fallback for signals, authority revalidation,
            // expiry, and idle connections. Publication itself wakes above.
            _ = tokio::time::sleep(deadline::DRIVER_POLL) => {}
        }
    }
}

/// `admits_scope`, for the fixtures that pin its rules.
///
/// A thin re-export rather than making the function public: the check belongs to
/// the session that runs it, and a caller outside this module reaching for it
/// would be a second place deciding what a World declared.
pub fn admits_scope_for_test(
    worlds: &Option<crate::registry::Catalog>,
    scope: &Target,
) -> Result<(), crate::transient::Invalid> {
    admits_scope(worlds, scope)
}

/// Whether a World's own scope is one that World declared.
///
/// The scope half of what `SignalPolicy::admits_contents` does for signals, and
/// it exists for the same reason: without it a declaration moves the
/// implementation id — every peer sees a different reviewed build — and buys no
/// enforcement at all. A World could declare a 32-byte board key and a peer
/// could publish a 128-byte one under a schema that World never named.
///
/// Only `World` is checked. Every other scope is the substrate's own
/// shape, bounded by the substrate's own numbers, and a World does not get to
/// widen or narrow what `Body` means.
fn admits_scope(
    worlds: &Option<crate::registry::Catalog>,
    scope: &Target,
) -> Result<(), crate::transient::Invalid> {
    let Target::World { world, schema, key } = scope else {
        return Ok(());
    };
    let Some(worlds) = worlds else {
        return Ok(());
    };
    let world = replica::body::WorldId::parse(world).ok_or(crate::transient::Invalid::Malformed)?;
    let schema =
        replica::body::SchemaId::parse(schema).ok_or(crate::transient::Invalid::Malformed)?;
    let registration = worlds
        .descriptor(&world)
        .ok_or(crate::transient::Invalid::NotDeclared)?;
    let declared = registration
        .scope_schemas
        .iter()
        .find(|candidate| candidate.name == schema)
        .ok_or(crate::transient::Invalid::NotDeclared)?;
    if key.len() > declared.max_key_bytes as usize {
        return Err(crate::transient::Invalid::Bounds);
    }
    Ok(())
}

/// Tell one peer which scopes this connection is about.
///
/// Replace-all, matching what the receiver does with it.
///
/// Returns whether it was written and finished. The caller must not record a
/// subscription it could not send: presence published into a subscription the
/// peer never received is dropped on arrival, silently, for as long as the
/// declaration does not change — which on a document somebody leaves open is the
/// whole session.
async fn subscribe_remotely(
    connection: &dyn comms::Connection,
    scopes: &std::collections::BTreeSet<Target>,
) -> bool {
    // Deadlined like every other flow this loop touches. Opening is local
    // bookkeeping on both contractors today, but a transport that made it wait
    // for stream credit would park the whole session here — no datagram read, no
    // sweep, no revalidation — which is exactly what `LIVE_FLOW_READ` exists to
    // prevent on the inbound half.
    let Ok(Ok((mut send, _recv))) =
        tokio::time::timeout(deadline::LIVE_FLOW_READ, connection.open_bi()).await
    else {
        return false;
    };
    let body = LiveControl::Subscribe {
        scopes: scopes.iter().cloned().collect(),
    }
    .encode();
    let mut framed = Vec::with_capacity(1 + 4 + body.len());
    framed.push(stream_kind::CONTROL);
    framed.extend_from_slice(&(body.len() as u32).to_le_bytes());
    framed.extend_from_slice(&body);
    // Both results, not just the deadline. `timeout(..).is_ok()` says only that
    // five seconds did not elapse — it reports a *failed write* as a success, so
    // the caller that was given this to check learns nothing.
    match tokio::time::timeout(deadline::LIVE_FLOW_READ, send.write_all(&framed)).await {
        Ok(Ok(())) => send.finish().is_ok(),
        _ => false,
    }
}

/// Tell one peer this Station has stopped looking at something.
///
/// Best effort, and silent when it fails. The peer's slot expires on its own
/// clock either way, so a retirement that does not arrive costs a stale face
/// for the rest of a TTL rather than a wrong one for ever — and a send that
/// retried would be a queue on a plane whose whole contract is that it has none.
///
/// The sequence number is this session's current high-water, so a presence
/// datagram already in flight cannot rebuild the slot behind the retirement.
/// That race is the reason `Retire` carries a sequence at all.
async fn retire_remotely(connection: &dyn comms::Connection, scope: &Target, seq: u64) {
    let Ok(Ok((mut send, _recv))) =
        tokio::time::timeout(deadline::LIVE_FLOW_READ, connection.open_bi()).await
    else {
        return;
    };
    let body = LiveControl::Retire {
        scope: scope.clone(),
        seq,
    }
    .encode();
    let mut framed = Vec::with_capacity(1 + 4 + body.len());
    framed.push(stream_kind::CONTROL);
    framed.extend_from_slice(&(body.len() as u32).to_le_bytes());
    framed.extend_from_slice(&body);
    if tokio::time::timeout(deadline::LIVE_FLOW_READ, send.write_all(&framed))
        .await
        .is_ok()
    {
        let _ = send.finish();
    }
}

/// Tell one peer this Station is looking at something.
///
/// A failure is not reported anywhere and that is deliberate: a newer
/// transient item or the next refresh supersedes it, and a transient send that
/// retried would be a transient send with a queue.
fn publish_payload(
    connection: &dyn comms::Connection,
    scope: &Target,
    payload: &crate::transient::TransientPayload,
    epoch: &[u8; 16],
    seq: u64,
    counters: &mut TransientCounters,
) {
    let item = TransientItem {
        connection_epoch: *epoch,
        seq,
        scope: scope.clone(),
        payload: payload.clone(),
    };
    publish(connection, &item, counters);
}

/// Send one transient item, or drop it and say why.
///
/// Called by `publish_presence` on the session beat, which is the plane's whole
/// send side. What decides *what* to publish is not in this crate — it is the
/// declaration a browser makes, carried down through `Request::Watching` — and
/// that split is deliberate: the plane moves what it is told to move, and
/// deciding what this Station is doing belongs to whatever drives a person's
/// view.
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
    sessions: std::cell::RefCell<Vec<Key>>,
    handle: std::sync::Arc<LiveHandle>,
    /// What a signal is asked about, and never anything that can commit.
    ///
    /// Held here rather than on the handle because these are the driver's, not
    /// the reader's: a browser looking at who is present has no business
    /// holding an authority view.
    authority: std::sync::Arc<dyn crate::world::AuthorityView>,
    worlds: crate::registry::Catalog,
}

impl LiveService {
    pub fn new(
        handle: std::sync::Arc<LiveHandle>,
        authority: std::sync::Arc<dyn crate::world::AuthorityView>,
        worlds: crate::registry::Catalog,
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
    pub fn present(&self) -> Vec<Key> {
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
            Context {
                handle: Some(self.handle.clone()),
                signals: Some(signals),
                worlds: Some(self.worlds.clone()),
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
    station: Key,
    sessions: &'a std::cell::RefCell<Vec<Key>>,
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
    station: Key,
    connection_epoch: [u8; 16],
    handle: Option<std::sync::Arc<LiveHandle>>,
}

impl Drop for Leaving {
    fn drop(&mut self) {
        if let Some(handle) = &self.handle {
            // Per session, not per Station. A peer may hold two — a laptop and
            // a phone — and forgetting by Station meant closing one deleted
            // what the other was still saying.
            handle.forget_session(&self.station, &self.connection_epoch);
            // The other half, and it was missing. `serve_session` counts a peer
            // in on the way past; nothing counted it out, so "who is here"
            // meant "who has ever been here" and signals queued for people who
            // had gone home.
            handle.departed(&self.station);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transient::TransientPayload;

    fn station() -> Key {
        Key::from_device(&mechanics::actor::device_from_seed(&[9u8; 32])).expect("station")
    }

    fn scope(n: u8) -> Target {
        Target::Body {
            world: "com.example.notes".into(),
            body: [n; 16],
        }
    }

    #[tokio::test]
    async fn local_declarations_wake_sessions_without_waiting_for_the_driver_poll() {
        let handle = LiveHandle::new(None);
        let mut updates = handle.subscribe_local_generation();
        handle.declare_local(vec![scope(1)]);
        tokio::time::timeout(Duration::from_millis(10), updates.changed())
            .await
            .expect("declaration wake")
            .expect("publisher remains open");
        assert_eq!(*updates.borrow_and_update(), handle.local_generation());
    }

    #[tokio::test]
    async fn table_changes_wake_local_live_subscriptions() {
        let handle = LiveHandle::new(None);
        let mut updates = handle.subscribe_generation();
        handle.note_dropped(Instant::now());
        tokio::time::timeout(Duration::from_millis(10), updates.changed())
            .await
            .expect("table wake")
            .expect("publisher remains open");
        assert_eq!(*updates.borrow_and_update(), handle.generation());
    }

    #[test]
    fn a_peer_hears_only_about_what_it_subscribed_to() {
        // A subscription is a snapshot of what a client is looking at. A store
        // lookup that ignored it would keep delivering to a peer that stopped
        // watching, which is both a leak and a waste.
        let mut session = Connection::new(station());
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
        let mut session = Connection::new(station());
        let epoch = [7u8; 16];
        let now = Instant::now();

        let mut stray = TransientItem {
            connection_epoch: [8u8; 16],
            seq: 1,
            scope: scope(1),
            payload: TransientPayload::Presence,
        };
        session.admit(&stray, &epoch, now);
        assert_eq!(session.counters().wrong_epoch, 1);

        stray.connection_epoch = epoch;
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
            mechanics::actor::device_from_seed(&[1u8; 32])
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

    #[tokio::test]
    async fn a_subscription_that_could_not_be_sent_says_so() {
        // The caller must not record a subscription it could not send. Presence
        // published into a subscription the peer never received is dropped on
        // arrival, silently, for as long as the declaration does not change —
        // which on a document somebody leaves open is the whole session.
        //
        // This returned `()` before, so the caller had nothing to check and
        // advanced its published set regardless.
        let scopes = std::collections::BTreeSet::from([scope(1)]);
        assert!(
            !subscribe_remotely(&Narrow(Some(1_162)), &scopes).await,
            "a connection that cannot open a flow has not subscribed"
        );
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
            connection_epoch: [1u8; 16],
            seq: 1,
            scope: Target::Content { content: [4u8; 32] },
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
            connection_epoch: [1u8; 16],
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
    Refused(crate::plane::Refusal),
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
    local: &Key,
    peer: &Key,
    connection_id: [u8; 16],
) -> Result<LivePeer, DialRefusal> {
    // Asked before the transport is touched. Dialling a peer we would refuse on
    // arrival is a round trip spent to be told what we already knew.
    let resolution = authority.admit_peer(peer).ok_or(DialRefusal::NotAdmitted)?;

    let mut space_bytes = [0u8; crate::plane::SPACE_ID_LEN];
    let raw = space.as_str().as_bytes();
    if raw.len() != crate::plane::SPACE_ID_LEN {
        return Err(DialRefusal::Unreachable);
    }
    space_bytes.copy_from_slice(raw);
    let mut epoch = [0u8; 16];
    getrandom::fill(&mut epoch).map_err(|_| DialRefusal::Unreachable)?;

    let connection = tokio::time::timeout(
        deadline::LIVE_DIAL,
        transport.connect_session(peer.as_device(), crate::plane::LIVE_ALPN),
    )
    .await
    .map_err(|_| DialRefusal::Unreachable)?
    .map_err(|_| DialRefusal::Unreachable)?;

    let open = crate::plane::Open {
        plane: crate::plane::Plane::Live,
        protocol_version: crate::plane::Plane::Live.protocol_version(),
        features: crate::plane::feature::LOCAL_SUPPORTED,
        space: space_bytes,
        initiator_station: local.key_bytes(),
        responder_station: peer.key_bytes(),
        connection_id,
        connection_epoch: epoch,
        authority_frontier: Vec::new(),
        requested_lanes: vec![
            stream_kind::CONTROL,
            stream_kind::RELIABLE_SIGNAL,
            stream_kind::MEDIA_GROUP,
            stream_kind::MEDIA_CONTROL,
        ],
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

    match crate::plane::Accept::decode_canonical(&answer) {
        Ok(accept) => Ok(LivePeer {
            connection: std::sync::Arc::from(connection),
            peer: AdmittedPeer {
                station: peer.clone(),
                actor: resolution.actor,
                authority_frontier: resolution.authority_frontier,
                // The one thing taken from the accept, because it is the one
                // thing only the responder knows: what it is willing to serve.
                granted_lanes: accept.granted_lanes,
                connection_id,
                connection_epoch: epoch,
                // Intersected locally, never taken on the peer's word. The
                // accept is the peer telling us what *it* agreed to, and a peer
                // is free to claim a bit this build does not implement — at
                // which point this Station would honour residency hints it has
                // no oracle behind. `judge` does this intersection on the
                // inbound path; the outbound path has to do it too, or the same
                // field means two different things depending on who dialled.
                features: accept.capability.features & crate::plane::feature::LOCAL_SUPPORTED,
            },
        }),
        Err(_) => Err(match crate::plane::Refusal::decode_canonical(&answer) {
            Ok(refusal) => DialRefusal::Refused(refusal),
            Err(_) => DialRefusal::Unintelligible,
        }),
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
    cooldown: std::collections::BTreeMap<Key, (Instant, u32)>,
    in_flight: std::collections::BTreeSet<Key>,
    sessions: std::collections::BTreeMap<Key, usize>,
}

impl DialLedger {
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether to dial this peer now.
    pub fn may_dial(&self, peer: &Key, now: Instant) -> bool {
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

    pub fn begin(&mut self, peer: &Key) {
        self.in_flight.insert(peer.clone());
    }

    /// A dial that became a session. The cooldown is cleared: what it was
    /// protecting against is a peer that will not talk to us, and this one just
    /// did.
    pub fn established(&mut self, peer: &Key) {
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
    pub fn failed(&mut self, peer: &Key, now: Instant) {
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
    pub fn ended(&mut self, peer: &Key) {
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
    pub local_station: Key,
    pub transport: std::sync::Arc<dyn comms::Transport>,
    /// Who might be worth dialling, asked afresh every round.
    ///
    /// A closure rather than a snapshot taken once: a Neighbor learned a minute
    /// after activation should be dialled a minute after activation, not at the
    /// next restart.
    pub candidates: std::sync::Arc<dyn Fn() -> Vec<Key> + Send + Sync>,
    pub handle: std::sync::Arc<LiveHandle>,
    pub authority: std::sync::Arc<dyn crate::world::AuthorityView>,
    pub worlds: crate::registry::Catalog,
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
                let mut connection_id = [0u8; 16];
                if getrandom::fill(&mut connection_id).is_err() {
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
                    connection_id,
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
    worlds: crate::registry::Catalog,
    handle: std::sync::Arc<LiveHandle>,
    cancel: crate::lifecycle::CancelToken,
    space: mechanics::ids::SpaceId,
    local_station: Key,
    ledger: Rc<RefCell<DialLedger>>,
}

async fn dial_and_serve(task: DialTask, peer: Key, connection_id: [u8; 16]) {
    let dialled = dial(
        task.transport.as_ref(),
        task.authority.as_ref(),
        &task.space,
        &task.local_station,
        &peer,
        connection_id,
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
        Context {
            handle: Some(task.handle.clone()),
            signals: Some(signals),
            worlds: Some(task.worlds.clone()),
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
/// **No production caller.** [`publish`] has one now; this does not. Presence is
/// republished on a fixed beat rather than coalesced, because a presence flag has
/// no intermediate values to collapse — the coalescer is for carets, and nothing
/// mints one yet. Stated so a reader does not assume a path exists and go looking
/// for it.
///
/// A caret moves as fast as a person types and is superseded by its own next
/// position, so sending each one spends a packet to deliver a number that is
/// already wrong. Holding for a coalescing window and sending the last one is
/// not a loss — the intermediate positions were never the answer to anything.
///
/// Keyed by scope and kind, because two scopes coalescing into one another
/// would be a cursor in one document overwriting a cursor in another.
pub struct Coalescer {
    pending: std::collections::BTreeMap<(Target, u8), (TransientItem, Instant)>,
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

    fn scope(n: u8) -> Target {
        Target::Body {
            world: "com.example.notes".into(),
            body: [n; 16],
        }
    }

    fn item(scope: Target, seq: u64) -> TransientItem {
        TransientItem {
            connection_epoch: [1u8; 16],
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
                connection_epoch: [1u8; 16],
                seq: 1,
                scope: Target::Typing {
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
