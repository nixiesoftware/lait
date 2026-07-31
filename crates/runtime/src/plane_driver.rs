//! The scaffolding both delivery planes run on: accept, judge, serve, stop.
//!
//! Parameterised rather than duplicated, because the parts that are easy to get
//! wrong are the parts that have nothing to do with which plane it is — taking
//! a permit *before* spawning rather than inside the spawned work, observing
//! cancellation while parked, and closing in an order that lets a refusal
//! actually reach the peer before the connection is dropped.
//!
//! **One driver per plane, one thread per driver.** Freight and Live are
//! separate ALPNs, separate queues, and separate threads, so a saturated
//! transfer cannot delay a cursor and a cursor flood cannot delay a transfer.
//! Sharing one driver between them would put both on one runtime's ready queue
//! and make that isolation a hope.
//!
//! Nothing here is `Send`: each driver owns a current-thread runtime and a
//! `LocalSet`, the same shape the Contact driver uses. Per-connection state is
//! therefore plain `Rc`/`RefCell` with no locking on the hot path, which is the
//! point of the gates living per connection-owning task.

use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

use mechanics::ids::{SpaceId, StationId};

use crate::admission::{
    judge, AcceptedOpenings, Admission, AdmittedPeer, OpeningContext, PlanePolicy, Replay,
};
use crate::budget::{deadline, slots};
use crate::lifecycle::CancelToken;
use crate::planes::{Plane, SessionOpen, SessionRefusal};
use crate::world::AuthorityView;

/// Everything a driver needs that is not about which plane it is.
pub struct PlaneContext {
    pub plane: Plane,
    pub space: SpaceId,
    pub local_station: StationId,
    pub authority: Arc<dyn AuthorityView>,
    pub policy: PlanePolicy,
    pub cancel: CancelToken,
    /// The Station's own drain deadline.
    ///
    /// Taken rather than assumed, because `Station::drain_tasks` leaks an
    /// unfinished handle instead of blocking: a driver budgeted from a constant
    /// outlives any Station configured with a shorter one, and then holds a
    /// cache whose store lock has already been released.
    pub drain_deadline: std::time::Duration,
    /// Bumped when Space authority advances.
    ///
    /// A session pins the view it was admitted at, which is what makes every
    /// later question on it answerable consistently — and also what makes a
    /// revocation invisible to it. This is the wake-up: on a bump, every live
    /// session re-asks whether its peer still has standing, and one that does
    /// not is closed. Without it a revoked peer keeps what it was holding until
    /// it happens to disconnect, which is not a bound.
    pub authority_tick: Option<tokio::sync::watch::Receiver<u64>>,
}

/// What a plane does with an admitted connection.
///
/// The driver owns everything up to and including the accept; from there the
/// plane owns the conversation. Splitting it here is what lets the admission
/// path, the budgets, and the shutdown ladder be written once.
pub trait PlaneService {
    /// Housekeeping, on a slow beat.
    ///
    /// Separate from serving because it must happen whether or not anyone is
    /// connected — reclaiming what a dead transfer left is exactly the work a
    /// quiet Station needs done. The default does nothing, so a plane with no
    /// housekeeping says so by omission.
    fn maintain(&self) -> impl std::future::Future<Output = ()> {
        std::future::ready(())
    }

    /// Serve one admitted connection until it ends or the driver stops.
    /// Shared rather than owned, because the driver keeps its own handle: a
    /// revocation has to be able to close a connection out from under whatever
    /// the plane is doing with it.
    fn serve(
        &self,
        connection: Arc<dyn comms::Connection>,
        peer: AdmittedPeer,
        cancel: CancelToken,
    ) -> impl std::future::Future<Output = ()>;
}

/// Run a plane driver until cancelled. Blocking; call it on its own thread.
///
/// The queue is a parameter rather than a context field for two reasons. `drive`
/// shares its context by `Rc` with every per-connection task and
/// `mpsc::Receiver::recv` takes `&mut self`; and it is the honest shape — a
/// driver owns one plane's inbound connections, not an endpoint. Everything a
/// plane dials *out* on, it gets from elsewhere.
pub fn run_driver<S>(context: PlaneContext, queue: comms::SessionQueue, service: S)
where
    S: PlaneService + 'static,
{
    let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        return;
    };
    let local = tokio::task::LocalSet::new();
    local.block_on(&runtime, drive(context, queue, service));
}

async fn drive<S>(context: PlaneContext, mut queue: comms::SessionQueue, service: S)
where
    S: PlaneService + 'static,
{
    let context = Rc::new(context);
    let service = Rc::new(service);
    let replays = Rc::new(std::cell::RefCell::new(AcceptedOpenings::default()));
    let mut connections = tokio::task::JoinSet::new();
    let mut last_maintained = Instant::now();
    // Two ceilings, because they answer different questions: how much this
    // Space will hold at once, and how much of that any one member may take.
    // Without the second, one peer's reconnect storm is indistinguishable from
    // the Space being busy.
    let held = Rc::new(std::cell::RefCell::new(std::collections::BTreeMap::<
        StationId,
        usize,
    >::new()));

    loop {
        if context.cancel.is_cancelled() {
            break;
        }
        // Polled rather than parked indefinitely, so cancellation is observed
        // even when nothing is arriving. A driver that only notices it should
        // stop when a peer happens to connect is a driver that does not stop.
        let accepted = tokio::select! {
            // `recv` is cancel-safe, so the poll below still drops this future
            // between connections without losing one.
            incoming = queue.recv() => incoming,
            _ = tokio::time::sleep(deadline::DRIVER_POLL) => {
                // The poll exists so cancellation is never missed. Maintenance
                // rides it on a much slower beat: sweeping on every 25 ms tick
                // would be a directory walk forty times a second.
                if last_maintained.elapsed() >= MAINTENANCE_INTERVAL {
                    last_maintained = Instant::now();
                    service.maintain().await;
                }
                continue;
            }
        };
        let Some(incoming) = accepted else {
            break;
        };
        if context.cancel.is_cancelled() {
            incoming.connection.close(REFUSED, b"");
            break;
        }

        let Some(peer) = StationId::from_device(&incoming.from) else {
            incoming.connection.close(REFUSED, b"");
            continue;
        };
        {
            let mut held = held.borrow_mut();
            let total: usize = held.values().sum();
            let mine = held.get(&peer).copied().unwrap_or(0);
            if total >= slots::MAX_SPACE_CONNECTIONS
                || mine >= slots::MAX_CONNECTIONS_PER_PEER_PLANE
            {
                drop(held);
                // Coarse, like every other refusal: a peer learns it was not
                // served, not whether the Space is full or it is greedy.
                incoming.connection.close(REFUSED, b"");
                continue;
            }
            *held.entry(peer.clone()).or_insert(0) += 1;
        }

        let context = context.clone();
        let service = service.clone();
        let replays = replays.clone();
        let held_for_task = held.clone();
        connections.spawn_local(async move {
            serve_connection(context, service, replays, incoming).await;
            let mut held = held_for_task.borrow_mut();
            if let Some(count) = held.get_mut(&peer) {
                *count = count.saturating_sub(1);
                if *count == 0 {
                    held.remove(&peer);
                }
            }
        });

        // Reap finished connections without waiting for them, so the set does
        // not grow with the number of connections this driver has ever seen.
        while connections.try_join_next().is_some() {}
    }

    // Dropped before the drain. A dispatcher parked on a full lane learns
    // immediately that nobody is reading and gives its peer a coarse close,
    // rather than holding a pending-opener permit until it times out.
    drop(queue);
    shut_down(&context, connections).await;
}

/// The coarse close code. One for every reason a connection is not served.
const REFUSED: u32 = 1;

/// How often a driver does housekeeping.
///
/// Slow, because the work it does — walking a staging directory, deciding what
/// a quota can still evict — costs the same whether it runs every second or
/// every minute, and only the second one is free.
const MAINTENANCE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(30);

async fn serve_connection<S>(
    context: Rc<PlaneContext>,
    service: Rc<S>,
    replays: Rc<std::cell::RefCell<AcceptedOpenings>>,
    incoming: comms::IncomingConnection,
) where
    S: PlaneService,
{
    let connection: Arc<dyn comms::Connection> = Arc::from(incoming.connection);
    if incoming.alpn != context.plane.alpn() {
        // The ALPN is the version gate and it also fixes the plane, so a
        // connection routed here under another one is a routing bug on our side
        // or an attempt on theirs. Either way this driver is not its owner.
        refuse(connection.as_ref(), Some(SessionRefusal::Malformed)).await;
        return;
    }
    let Some(peer_station) = StationId::from_device(&incoming.from) else {
        refuse(connection.as_ref(), None).await;
        return;
    };

    // A router that demultiplexed by Space had to read the opening to do it,
    // and reading a flow consumes it — so it hands the bytes over. When nobody
    // did, the driver reads them itself: the opening is the first thing on the
    // connection either way, and a plane that only worked behind one particular
    // router would be a plane that cannot be tested without one.
    let raw = if incoming.opening.is_empty() {
        match tokio::time::timeout(deadline::ACCEPT_WRITE, read_opening(connection.as_ref())).await
        {
            Ok(Some(bytes)) => bytes,
            _ => {
                refuse(connection.as_ref(), Some(SessionRefusal::Malformed)).await;
                return;
            }
        }
    } else {
        incoming.opening
    };
    let Ok(open) = SessionOpen::decode_canonical(&raw) else {
        // When a router did parse this to route it, a failure here means the
        // two of us disagree — worth refusing rather than papering over,
        // because everything downstream trusts this parse.
        refuse(connection.as_ref(), Some(SessionRefusal::Malformed)).await;
        return;
    };

    // Idempotent acceptance. 0.5-RTT data is replayable, so a replay must get
    // the answer the first one got and mint nothing: no second session, no
    // second charge against any session-scoped budget. The connection itself is
    // real and was charged as one, which is why it is still closed afterwards.
    let now = Instant::now();
    // Bound before the branch. `if let` extends its scrutinee's temporaries to
    // the end of the block, so borrowing inside the condition would hold the
    // `RefCell` across both awaits below — and a second opening arriving on
    // this driver would panic rather than be served.
    let replay = replays.borrow_mut().lookup(&open, now);
    if let Replay::Repeat(previous) = replay {
        answer(connection.as_ref(), &previous.encode()).await;
        close_after_flush(connection.as_ref()).await;
        return;
    }

    let verdict = judge(
        &open,
        &OpeningContext {
            space: &context.space,
            local_station: context.local_station.clone(),
            peer: peer_station,
            plane: context.plane,
        },
        context.authority.as_ref(),
        &context.policy,
    );
    let (accept, peer) = match verdict {
        Admission::Accept(accept, peer) => (accept, peer),
        Admission::Refuse(refusal) => {
            refuse(connection.as_ref(), Some(refusal)).await;
            return;
        }
    };

    replays.borrow_mut().remember(&open, &accept, now);
    if !answer(connection.as_ref(), &accept.encode()).await {
        return;
    }

    // Serving races a revocation watch. Whichever finishes first ends the
    // session — and if it is the watch, the connection is closed under the
    // service rather than politely asked to stop, because a peer that has lost
    // standing does not get to finish what it was doing.
    let station = peer.station.clone();
    let serving = service.serve(connection.clone(), *peer, context.cancel.clone());
    let revoked = watch_for_revocation(&context, station);
    tokio::select! {
        _ = serving => {}
        _ = revoked => {}
    }
    // Both endings go through the same close. Dropping a connection resets its
    // streams, so a served chunk whose last bytes are still in flight would be
    // truncated by our own teardown — a transfer failing for a reason that is
    // entirely ours.
    close_after_flush(connection.as_ref()).await;
}

/// Resolve once this peer no longer has standing.
///
/// Parks forever when nothing publishes an authority tick, which is the honest
/// answer for a driver with no authority source: it never spuriously closes a
/// session it has no reason to doubt.
async fn watch_for_revocation(context: &PlaneContext, station: StationId) {
    let Some(mut tick) = context.authority_tick.clone() else {
        std::future::pending::<()>().await;
        return;
    };
    loop {
        if tick.changed().await.is_err() {
            std::future::pending::<()>().await;
            return;
        }
        // Re-asked, not remembered. The pinned view is exactly what has gone
        // stale, so the answer has to come from the authority again.
        if context.authority.admit_peer(&station).is_none() {
            return;
        }
    }
}

/// Read the opening off the connection's first flow, bounded before anything
/// is allocated for it.
async fn read_opening(connection: &dyn comms::Connection) -> Option<Vec<u8>> {
    let mut flow = connection.accept_uni().await.ok()??;
    flow.read_to_end(crate::planes::bounds::MAX_OPENING_BYTES)
        .await
        .ok()
}

/// Write one bounded answer on a fresh flow. `false` when the peer is gone.
async fn answer(connection: &dyn comms::Connection, bytes: &[u8]) -> bool {
    let write = async {
        let mut flow = connection.open_uni().await.ok()?;
        flow.write_all(bytes).await.ok()?;
        flow.finish().ok()?;
        Some(())
    };
    matches!(
        tokio::time::timeout(deadline::ACCEPT_WRITE, write).await,
        Ok(Some(()))
    )
}

/// Tell a peer no, then close.
///
/// The refusal is written before the close because a close alone reaches the
/// peer as a transport error it will retry, which is the opposite of a refusal.
/// Every reason shares one answer, except an unsupported generation — the one a
/// peer can actually act on.
async fn refuse(connection: &dyn comms::Connection, refusal: Option<SessionRefusal>) {
    let refusal = refusal.unwrap_or(SessionRefusal::Refused);
    // `unwrap_or_default` here would have written an empty vector on an encode
    // failure, which the peer reads as a closed stream — a silence where a
    // refusal was meant. The encode cannot fail for this shape, and saying so
    // is better than a fallback that quietly means the opposite.
    answer(connection, &refusal.encode()).await;
    close_after_flush(connection).await;
}

/// Close, having given the peer a bounded chance to read what we just wrote.
///
/// Dropping a connection resets its streams. Without this wait a refusal that
/// was correctly written arrives as an ambiguous transport failure.
async fn close_after_flush(connection: &dyn comms::Connection) {
    // Wait *first*, then close. Closing and then awaiting `closed()` is a zero
    // width window — the future resolves immediately because we are the ones
    // who closed it — and the refusal we just wrote reaches the peer as an
    // ambiguous transport error it will retry. Waiting for the peer to hang up
    // gives it the read; the deadline is what stops a peer that never does from
    // holding the slot.
    let _ = tokio::time::timeout(deadline::FLUSH_BEFORE_DROP, connection.closed()).await;
    connection.close(REFUSED, b"");
}

/// The shutdown ladder.
///
/// Order matters and each rung has a reason: stop taking work, let what is
/// in flight finish inside a budget strictly smaller than the Station's drain
/// deadline, and return from the driver last so the runtime drops after its
/// tasks rather than aborting them mid-await.
async fn shut_down(context: &PlaneContext, mut connections: tokio::task::JoinSet<()>) {
    // Strictly less than the Station's, and every rung below is strictly less
    // than this: a ladder where each step has margin over the one under it is
    // what makes the ordinary path a clean join rather than an abort.
    let budget = context
        .drain_deadline
        .saturating_sub(deadline::ACCEPT_WRITE)
        .max(deadline::DRIVER_POLL * 2);
    let joined = tokio::time::timeout(budget, async {
        while connections.join_next().await.is_some() {}
    })
    .await;
    if joined.is_err() {
        // Aborting is the last resort: a task that will not finish inside the
        // budget is a bug, and the alternative to aborting it is a Station that
        // never stops.
        connections.abort_all();
        // Bounded even here. An aborted task still runs its drops, and a drop
        // that blocks would turn "we gave up waiting" into "we hung anyway".
        let _ = tokio::time::timeout(deadline::ACCEPT_WRITE, async {
            while connections.join_next().await.is_some() {}
        })
        .await;
    }
}
