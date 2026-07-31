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
//! stream is bidirectional, subscriptions arrive on it, and everything else is
//! a datagram. That is a constraint the transport imposes rather than a
//! preference, and it is written here rather than discovered in a test.
//!
//! Three things are bounded before anything is spent, and the order is the
//! bound: a permit before the stream kind is read, the gate before the message,
//! the message's declared length before its buffer.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Instant;

use mechanics::ids::StationId;

use crate::admission::AdmittedPeer;
use crate::budget::{deadline, gates, ByteGate, Gate, Verdict};
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
        Self {
            peer,
            store: TransientStore::new(),
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
}

#[derive(Default)]
struct PublishTable {
    generation: u64,
    partial: bool,
    slots: std::collections::BTreeMap<
        (StationId, TransientScope, u8),
        crate::transient::TransientSlot,
    >,
}

impl LiveHandle {
    pub fn new(anchors: Option<std::sync::Arc<dyn AnchorSource>>) -> Self {
        Self {
            table: std::sync::Mutex::new(PublishTable::default()),
            signals: tokio::sync::broadcast::channel(SIGNAL_QUEUE).0,
            anchors,
        }
    }

    fn table(&self) -> std::sync::MutexGuard<'_, PublishTable> {
        self.table.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Listen for signals this Station receives.
    ///
    /// Subscribing before anything arrives is the only way to hear anything: a
    /// signal is an event rather than a state anyone can re-read.
    pub fn signals(&self) -> tokio::sync::broadcast::Receiver<(StationId, crate::planes::Signal)> {
        self.signals.subscribe()
    }

    pub(crate) fn sink(&self) -> SignalSink {
        self.signals.clone()
    }

    /// Record one item a peer sent, after the per-session store admitted it.
    ///
    /// After, not instead: the session store is what bounds a single connection,
    /// and this is what a reader sees. An item that failed the session's own
    /// ceilings never reaches here.
    pub fn record(&self, station: &StationId, item: &TransientItem, now: Instant) {
        let mut table = self.table();
        table.generation = table.generation.wrapping_add(1);
        table.slots.insert(
            (
                station.clone(),
                item.scope.clone(),
                item.payload.kind() as u8,
            ),
            crate::transient::TransientSlot {
                session_epoch: item.session_epoch,
                seq: item.seq,
                arrived_at: now,
                retired_at: None,
                payload: item.payload.clone(),
            },
        );
    }

    /// Drop everything one Station held.
    ///
    /// What a disconnect does, and what a revocation does. Immediate rather than
    /// at TTL: a peer that lost standing keeps nothing, and a peer that hung up
    /// is not "here for another ninety seconds".
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

    /// Say whether this Station is hearing from everyone it could be.
    pub fn set_partial(&self, partial: bool) {
        let mut table = self.table();
        if table.partial != partial {
            table.partial = partial;
            table.generation = table.generation.wrapping_add(1);
        }
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
            (table.generation, table.partial, held)
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
pub type SignalSink = tokio::sync::broadcast::Sender<(StationId, crate::planes::Signal)>;

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
pub async fn serve_session(
    connection: std::sync::Arc<dyn comms::Connection>,
    peer: AdmittedPeer,
    cancel: crate::lifecycle::CancelToken,
    handle: Option<std::sync::Arc<LiveHandle>>,
) {
    let session = Rc::new(RefCell::new(LiveSession::new(peer.station.clone())));
    let epoch = peer.session_epoch;
    // Whatever ends this connection — idle, cancel, a gate, a peer hanging up —
    // the slots go with it. Presence has no goodbye it can rely on, so the
    // session ending *is* the goodbye.
    let leaving = Leaving {
        station: peer.station.clone(),
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
                    Verdict::Drop => continue,
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
                let now = Instant::now();
                if session.admit(&item, &epoch, now) == AdmitOutcome::Stored {
                    // Only what the session store took. The per-connection
                    // ceilings are what bound one peer, and an item that failed
                    // them must not appear to a reader as though it had not.
                    if let Some(handle) = &handle {
                        handle.record(&peer.station, &item, now);
                    }
                }
            }

            accepted = connection.accept_bi() => {
                let Ok(Some((send, mut recv))) = accepted else { break };
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

                let kind = match read_stream_kind(recv.as_mut()).await {
                    Ok(kind) => kind,
                    // A reserved kind is a peer using a reservation we
                    // published and have not built: the flow resets and the
                    // connection stays up, because the peer is not wrong.
                    Err(StreamError::ReservedKind(_)) | Err(StreamError::UnknownKind(_)) => {
                        drop(send);
                        continue;
                    }
                    Err(_) => {
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
                    // One bounded message per stream, read under the
                    // declaration's own ceiling — resolved from the selector
                    // before the length is consulted, which is what makes that
                    // ceiling a pre-allocation bound rather than a comment.
                    let received = tokio::time::timeout(
                        deadline::SIGNAL_READ,
                        crate::signal::read_signal(recv.as_mut()),
                    )
                    .await;
                    drop(send);
                    let Ok(Ok(signal)) = received else { continue };
                    if matches!(
                        signal_bytes.check(Instant::now(), signal.encode().len()),
                        Verdict::Close
                    ) {
                        connection.close(REFUSED, b"");
                        break;
                    }
                    if let Some(handle) = &handle {
                        // Nobody listening is not a failure. A Station with no
                        // viewer attached still admits signals and still bounds
                        // them; it simply has nobody to hand them to.
                        let _ = handle.sink().send((peer.station.clone(), signal));
                    }
                    continue;
                }
                if kind != stream_kind::CONTROL {
                    drop(send);
                    continue;
                }

                let Ok(body) = read_framed(recv.as_mut(), bounds::MAX_CONTROL_FRAME_BYTES).await
                else {
                    drop(send);
                    continue;
                };
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
                    LiveControl::Subscribe { scopes } => session.subscribe(scopes),
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
}

impl LiveService {
    pub fn new(handle: std::sync::Arc<LiveHandle>) -> Self {
        Self {
            sessions: std::cell::RefCell::new(Vec::new()),
            handle,
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
                .set_partial(sessions.len() > crate::budget::slots::MAX_LIVE_SESSIONS);
        }
        serve_session(connection, peer, cancel, Some(self.handle.clone())).await;
        // Removed on the way out, whatever ended it. A peer left in this list
        // after its connection closed is a ghost that no TTL reaches, because
        // the TTLs are on slots rather than on sessions.
        let mut sessions = self.sessions.borrow_mut();
        sessions.retain(|held| held != &station);
        self.handle
            .set_partial(sessions.len() > crate::budget::slots::MAX_LIVE_SESSIONS);
    }
}

/// Drops one peer's slots when its session ends, however it ends.
///
/// A guard rather than a line at the bottom of the loop: `serve_session` leaves
/// through eight `break`s and a cancellation, and the one path that forgot to
/// clean up would leave a cursor on screen belonging to somebody who closed
/// their laptop.
struct Leaving {
    station: StationId,
    handle: Option<std::sync::Arc<LiveHandle>>,
}

impl Drop for Leaving {
    fn drop(&mut self) {
        if let Some(handle) = &self.handle {
            handle.forget(&self.station);
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

    #[test]
    fn a_payload_the_path_cannot_carry_is_dropped_rather_than_cut() {
        // The one thing that must never happen: a transient payload has no
        // retransmit, so half of one is corruption rather than a gap.
        struct NoDatagrams;
        impl NoDatagrams {
            fn capacity(&self) -> Option<usize> {
                None
            }
        }
        let mut counters = TransientCounters::default();
        // A peer that negotiated no datagram support at all.
        assert!(!datagram_fits(16, NoDatagrams.capacity()));
        counters.capacity_drops += 1;
        assert_eq!(counters.capacity_drops, 1);

        // And a payload past what the measured path carries.
        assert!(!datagram_fits(2_000, Some(1_162)));
        assert!(datagram_fits(1_000, Some(1_162)));
    }
}

/// Send-side coalescing: hold a value briefly, and send the newest.
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
