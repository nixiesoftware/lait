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
use crate::budget::deadline;
use crate::lifecycle::CancelToken;
use crate::planes::{Plane, SessionOpen, SessionRefusal};
use crate::world::AuthorityView;

/// Everything a driver needs that is not about which plane it is.
pub struct PlaneContext {
    pub plane: Plane,
    pub space: SpaceId,
    pub local_station: StationId,
    pub authority: Arc<dyn AuthorityView>,
    pub transport: Arc<dyn comms::Transport>,
    pub policy: PlanePolicy,
    pub cancel: CancelToken,
}

/// What a plane does with an admitted connection.
///
/// The driver owns everything up to and including the accept; from there the
/// plane owns the conversation. Splitting it here is what lets the admission
/// path, the budgets, and the shutdown ladder be written once.
pub trait PlaneService {
    /// Serve one admitted connection until it ends or the driver stops.
    fn serve(
        &self,
        connection: Box<dyn comms::Connection>,
        peer: AdmittedPeer,
        cancel: CancelToken,
    ) -> impl std::future::Future<Output = ()>;
}

/// Run a plane driver until cancelled. Blocking; call it on its own thread.
pub fn run_driver<S>(context: PlaneContext, service: S)
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
    local.block_on(&runtime, drive(context, service));
}

async fn drive<S>(context: PlaneContext, service: S)
where
    S: PlaneService + 'static,
{
    let context = Rc::new(context);
    let service = Rc::new(service);
    let replays = Rc::new(std::cell::RefCell::new(AcceptedOpenings::default()));
    let mut connections = tokio::task::JoinSet::new();

    loop {
        if context.cancel.is_cancelled() {
            break;
        }
        // Polled rather than parked indefinitely, so cancellation is observed
        // even when nothing is arriving. A driver that only notices it should
        // stop when a peer happens to connect is a driver that does not stop.
        let accepted = tokio::select! {
            incoming = context.transport.accept_connection() => incoming,
            _ = tokio::time::sleep(deadline::DRIVER_POLL) => continue,
        };
        let Some(incoming) = accepted else {
            break;
        };
        if context.cancel.is_cancelled() {
            incoming.connection.close(REFUSED, b"");
            break;
        }

        let context = context.clone();
        let service = service.clone();
        let replays = replays.clone();
        connections.spawn_local(async move {
            serve_connection(context, service, replays, incoming).await;
        });

        // Reap finished connections without waiting for them, so the set does
        // not grow with the number of connections this driver has ever seen.
        while connections.try_join_next().is_some() {}
    }

    shut_down(connections).await;
}

/// The coarse close code. One for every reason a connection is not served.
const REFUSED: u32 = 1;

async fn serve_connection<S>(
    context: Rc<PlaneContext>,
    service: Rc<S>,
    replays: Rc<std::cell::RefCell<AcceptedOpenings>>,
    incoming: comms::IncomingConnection,
) where
    S: PlaneService,
{
    let connection = incoming.connection;
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
    if let Replay::Repeat(previous) = replays.borrow_mut().lookup(&open, now) {
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

    service
        .serve(connection, *peer, context.cancel.clone())
        .await;
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
    let bytes = postcard::to_stdvec(&refusal).unwrap_or_default();
    answer(connection, &bytes).await;
    close_after_flush(connection).await;
}

/// Close, having given the peer a bounded chance to read what we just wrote.
///
/// Dropping a connection resets its streams. Without this wait a refusal that
/// was correctly written arrives as an ambiguous transport failure.
async fn close_after_flush(connection: &dyn comms::Connection) {
    connection.close(REFUSED, b"");
    let _ = tokio::time::timeout(deadline::FLUSH_BEFORE_DROP, connection.closed()).await;
}

/// The shutdown ladder.
///
/// Order matters and each rung has a reason: stop taking work, let what is
/// in flight finish inside a budget strictly smaller than the Station's drain
/// deadline, and return from the driver last so the runtime drops after its
/// tasks rather than aborting them mid-await.
async fn shut_down(mut connections: tokio::task::JoinSet<()>) {
    // Strictly less than the Station's drain deadline: `drain_tasks` leaks an
    // unfinished handle rather than blocking, so a driver that takes the whole
    // budget is a driver that can outlive the Station that owns it.
    let budget = crate::lifecycle::DEFAULT_DRAIN_DEADLINE.saturating_sub(deadline::ACCEPT_WRITE);
    let joined = tokio::time::timeout(budget, async {
        while connections.join_next().await.is_some() {}
    })
    .await;
    if joined.is_err() {
        // Aborting is the last resort: a task that will not finish inside the
        // budget is a bug, and the alternative to aborting it is a Station that
        // never stops.
        connections.abort_all();
        while connections.join_next().await.is_some() {}
    }
}
