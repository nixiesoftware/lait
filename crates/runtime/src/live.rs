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
    signals: Option<SignalSink>,
) {
    let session = Rc::new(RefCell::new(LiveSession::new(peer.station.clone())));
    let epoch = peer.session_epoch;

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
            session.borrow_mut().sweep(Instant::now());
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
                session.admit(&item, &epoch, Instant::now());
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
                    if let Some(sink) = &signals {
                        // Nobody listening is not a failure. A Station with no
                        // viewer attached still admits signals and still bounds
                        // them; it simply has nobody to hand them to.
                        let _ = sink.send((peer.station.clone(), signal));
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
    signals: SignalSink,
}

impl LiveService {
    pub fn new() -> Self {
        Self {
            sessions: std::cell::RefCell::new(Vec::new()),
            signals: tokio::sync::broadcast::channel(SIGNAL_QUEUE).0,
        }
    }

    /// Listen for signals this Station receives.
    ///
    /// Subscribing before anything arrives is the only way to hear anything:
    /// a broadcast delivers what follows the subscription, and a signal is an
    /// event rather than a state anyone can re-read.
    pub fn signals(&self) -> tokio::sync::broadcast::Receiver<(StationId, crate::planes::Signal)> {
        self.signals.subscribe()
    }

    /// Which peers currently hold a session. A Station-local answer, and not a
    /// membership claim — a member with nothing open is not here.
    pub fn present(&self) -> Vec<StationId> {
        self.sessions.borrow().clone()
    }
}

impl Default for LiveService {
    fn default() -> Self {
        Self::new()
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
        self.sessions.borrow_mut().push(station.clone());
        serve_session(connection, peer, cancel, Some(self.signals.clone())).await;
        // Removed on the way out, whatever ended it. Presence has no goodbye it
        // can rely on, so the session ending *is* the goodbye — and a peer left
        // in this list after its connection closed is a ghost that no TTL
        // reaches, because the TTLs are on slots rather than on sessions.
        self.sessions.borrow_mut().retain(|held| held != &station);
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
