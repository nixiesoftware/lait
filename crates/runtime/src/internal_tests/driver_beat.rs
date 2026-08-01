//! The real driver loop, run under a paused clock.
//!
//! `paused_clock` asserts the beat *arithmetic* against the real constants.
//! This runs `plane_driver::drive` itself — the actual `tokio::select!`, the
//! actual `sleep(DRIVER_POLL)`, the actual `last_maintained.elapsed()` check —
//! and counts what the service is asked to do.
//!
//! It could not be written before the clock seam. `MAINTENANCE_INTERVAL` is
//! thirty seconds, so proving "maintenance rides a slow beat, not the poll"
//! meant waiting ninety seconds of wall clock per assertion; a test that
//! expensive does not get written, which is why the claim in the driver's own
//! comment went unchecked. Under `start_paused` the same ninety seconds costs
//! about a millisecond, and the count is exact rather than approximate.
//!
//! The public entry point `run_driver` builds its own current-thread runtime,
//! which a `#[tokio::test]` cannot nest inside — hence `drive` being
//! `pub(crate)`. What runs here is the loop, not a re-description of it.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use mechanics::ids::SpaceId;
use mechanics::station::Key;
use replica::frontier::AuthorityFrontier;

use crate::admission::{AdmittedPeer, PlanePolicy};
use crate::lifecycle::CancelToken;
use crate::plane::Plane;
use crate::plane_driver::{drive, PlaneContext, PlaneService};
use crate::world::{AuthorityView, PrincipalResolution};

fn space() -> SpaceId {
    SpaceId::from_digest([31u8; 16])
}

struct Everyone;
impl AuthorityView for Everyone {
    fn resolve(&self, _device: &mechanics::ids::DeviceId) -> Option<PrincipalResolution> {
        Some(PrincipalResolution {
            actor: mechanics::ids::ActorId::parse(&format!("act_{}", "ef".repeat(32)))
                .expect("actor"),
            authority_frontier: AuthorityFrontier::from_canonical_bytes(vec![9]),
        })
    }
}

/// Counts what the driver asks of it. `Cell` rather than an atomic because the
/// driver is a `LocalSet`-shaped single-threaded loop and the test is too —
/// using an atomic here would imply a concurrency that does not exist.
#[derive(Default)]
struct CountingService {
    maintained: Rc<Cell<u32>>,
}

impl PlaneService for CountingService {
    async fn maintain(&self) {
        self.maintained.set(self.maintained.get() + 1);
    }

    async fn serve(
        &self,
        _connection: Arc<dyn comms::Connection>,
        _peer: AdmittedPeer,
        _cancel: CancelToken,
    ) {
        unreachable!("this driver is never offered a connection");
    }
}

fn context(cancel: CancelToken) -> PlaneContext {
    PlaneContext {
        plane: Plane::Freight,
        space: space(),
        local_station: Key::from_key_bytes([7u8; 32]),
        authority: Arc::new(Everyone),
        policy: PlanePolicy::default(),
        cancel,
        drain_deadline: crate::lifecycle::DEFAULT_DRAIN_DEADLINE,
        authority_tick: None,
    }
}

/// Ninety simulated seconds of an idle driver: the poll fires ~3600 times and
/// maintenance fires three.
///
/// The driver's comment says sweeping on every 25 ms tick "would be a directory
/// walk forty times a second". This is that claim, checked against the loop
/// itself: 3 rather than 3600.
///
/// Time passes by SLEEPING, not by `advance`. Under `start_paused` the runtime
/// auto-advances to the next pending timer whenever it has nothing to run, so
/// the driver's own `sleep(DRIVER_POLL)` fires 3600 times in sequence — the
/// schedule it would really see, at no wall-clock cost. Jumping the clock with
/// one `advance` would instead deliver a single enormous tick, which is a
/// different scenario (and the subject of the next test).
#[tokio::test(start_paused = true)]
async fn an_idle_driver_maintains_on_the_slow_beat() {
    let cancel = CancelToken::new();
    let maintained = Rc::new(Cell::new(0u32));
    let service = CountingService {
        maintained: maintained.clone(),
    };
    // A queue nobody sends on: the driver has no connections and nothing to do
    // but poll, which is exactly the state maintenance exists for.
    let (_tx, queue) = tokio::sync::mpsc::channel(1);

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let driving = tokio::task::spawn_local(drive(context(cancel.clone()), queue, service));

            tokio::time::sleep(Duration::from_secs(90)).await;

            cancel.cancel();
            tokio::time::sleep(Duration::from_millis(50)).await;
            let _ = driving.await;
        })
        .await;

    let count = maintained.get();
    assert_eq!(
        count, 3,
        "ninety seconds at a thirty-second beat is three maintenances, not {count}"
    );
}

/// A clock that jumps does NOT produce a burst of catch-up maintenance.
///
/// This was found by writing the test above with `advance` and expecting three:
/// the driver got one. That is the loop's actual behaviour — `elapsed() >=
/// MAINTENANCE_INTERVAL` is a level check, not a counter — and it is the right
/// behaviour, because the alternative is a laptop waking from ten minutes of
/// sleep and immediately queueing twenty directory walks.
///
/// It is worth pinning precisely because it is not obvious from reading the
/// loop, and because a future change to a `tokio::time::interval` would silently
/// acquire the opposite behaviour: intervals default to `MissedTickBehavior::Burst`.
#[tokio::test(start_paused = true)]
async fn a_suspended_process_does_not_wake_up_owing_maintenance() {
    let cancel = CancelToken::new();
    let maintained = Rc::new(Cell::new(0u32));
    let service = CountingService {
        maintained: maintained.clone(),
    };
    let (_tx, queue) = tokio::sync::mpsc::channel(1);

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let driving = tokio::task::spawn_local(drive(context(cancel.clone()), queue, service));
            tokio::task::yield_now().await;

            // Ten minutes gone in one step, as if the host had been suspended.
            tokio::time::advance(Duration::from_secs(600)).await;
            tokio::task::yield_now().await;

            cancel.cancel();
            tokio::time::advance(Duration::from_millis(50)).await;
            let _ = driving.await;
        })
        .await;

    let count = maintained.get();
    assert_eq!(
        count, 1,
        "a ten-minute jump should settle the beat once, not queue twenty; got {count}"
    );
}

/// The driver stops when cancelled, and stops *promptly* — within a poll
/// interval, not at the next maintenance beat.
///
/// This is the property the poll exists for, and the driver says so: "a driver
/// that only notices it should stop when a peer happens to connect is a driver
/// that does not stop." Before the clock seam, checking it meant either a real
/// sleep or trusting the comment.
#[tokio::test(start_paused = true)]
async fn a_cancelled_driver_stops_within_one_poll() {
    let cancel = CancelToken::new();
    let maintained = Rc::new(Cell::new(0u32));
    let service = CountingService {
        maintained: maintained.clone(),
    };
    let (_tx, queue) = tokio::sync::mpsc::channel(1);

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let driving = tokio::task::spawn_local(drive(context(cancel.clone()), queue, service));
            tokio::task::yield_now().await;

            cancel.cancel();
            // One poll interval of simulated time is all a prompt stop may take.
            tokio::time::advance(crate::budget::deadline::DRIVER_POLL).await;
            tokio::task::yield_now().await;

            // If the driver were parked until the next maintenance beat, this
            // would hang until the test harness gave up rather than returning.
            let stopped = tokio::time::timeout(Duration::from_secs(1), driving).await;
            assert!(
                stopped.is_ok(),
                "a cancelled driver must stop within a poll, not at the next beat"
            );
        })
        .await;

    assert_eq!(
        maintained.get(),
        0,
        "a driver cancelled immediately should never have maintained"
    );
}
