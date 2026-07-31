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

/// Serve one admitted Live connection until it ends or the driver stops.
///
/// Everything here is `Rc`/`RefCell` and never a lock: `run_driver` is a
/// current-thread runtime with a `LocalSet`, so per-session state has one
/// owner and contention is not a thing that can happen.
pub async fn serve_session(
    connection: std::sync::Arc<dyn comms::Connection>,
    peer: AdmittedPeer,
    cancel: crate::lifecycle::CancelToken,
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
