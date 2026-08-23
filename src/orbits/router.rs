//! Local control routing and Station placement.
//!
//! A [`Router`] is the sole host-plane entrance from a catalog-wide
//! client to an Orbit. It resolves the Orbit through [`Catalog`], places
//! or reuses exactly one Station host for that Orbit, and dispatches Space
//! control plus product-neutral World calls. Its transport factory shares one
//! concrete endpoint per device identity across the owned placements. A vacant
//! Orbit is hosted in-process; a compatible pre-existing per-home daemon is
//! attached as an external placement. Per-home IPC remains an internal
//! compatibility adapter behind the identity-scoped Lait daemon endpoint.

use runtime::poison::LockRecovering;
use std::collections::HashMap;
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, Mutex, RwLock, Semaphore};

use crate::control::{self, ControlRoute, Doorbell, Request, Response};
use crate::orbital::hosting::{StationRunner, StationStop};
use crate::orbital::WorldPackages;
use crate::orbital::WorldUpgradeStep;
use comms::{DefaultFactory, TransportFactory};
use runtime::world::call::{Call, Code, Reply};

use crate::daemon::transport_hub::TransportHubFactory;
use crate::daemon::{LocalOrbitId, OrbitAddress};
use crate::orbits::{Catalog, ResolvedOrbit};
use replica::body::WorldId;

/// A Station placement's current hosting strategy.
///
/// This enum makes process hosting an adapter choice rather than the definition
/// of a Station. An in-process or isolated plugin worker can be added without
/// changing Orbit addressing or the client protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hosting {
    InProcess,
    CompatibilityProcess,
}

/// A doorbell tagged with the durable local Orbit that produced it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrbitDoorbell {
    /// Kept as `space` on the existing web wire until that client contract gets
    /// its own versioned rename. The value is a local Orbit id, not a Space id.
    #[serde(rename = "space")]
    pub orbit: LocalOrbitId,
    #[serde(flatten)]
    pub doorbell: Doorbell,
}

/// How long the doorbell pump waits between re-subscribe attempts, and the
/// ceiling it backs off to.
///
/// The same policy the viewer's daemon pump runs (`crate::serve`), and
/// deliberately the same numbers: both re-subscribe to a doorbell stream, and
/// every re-subscribe costs a subscriber downstream a full rebaseline. Half a
/// second makes a host restart look instant; thirty seconds is where a Station
/// that is not answering stops costing anything.
const PUMP_BACKOFF_FLOOR: Duration = Duration::from_millis(500);
const PUMP_BACKOFF_CEILING: Duration = Duration::from_secs(30);

/// How long one subscribe attempt may take before it counts as unreachable.
///
/// Matches `control::probe`'s bound, for the reason given there: connecting to
/// a Windows named pipe with no free instance parks rather than erroring, so
/// without this the pump can wait forever on a wedged host and never conclude
/// anything — least of all that the host is gone.
const SUBSCRIBE_TIMEOUT: Duration = Duration::from_secs(5);

/// How long shutdown waits to exclude placements before it proceeds regardless.
///
/// The read side of `lifecycle` is held for the whole of a lazy placement —
/// transport build included, which has its own multi-second waits — so a stop
/// that arrives mid-placement can queue behind it for far longer than any
/// shutdown should take. This bounds that wait; exceeding it is reported, not
/// obeyed.
const LIFECYCLE_DEADLINE: Duration = Duration::from_secs(5);
const HOST_BLOCKING_CAPACITY: usize = 4;

#[derive(Debug)]
pub(crate) enum BlockingFailure {
    Capacity,
    Join(String),
    Work(anyhow::Error),
}

impl std::fmt::Display for BlockingFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Capacity => formatter.write_str("the bounded host blocking lane is at capacity"),
            Self::Join(error) => {
                write!(formatter, "the blocking host task did not finish: {error}")
            }
            Self::Work(error) => std::fmt::Display::fmt(error, formatter),
        }
    }
}

impl std::error::Error for BlockingFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Work(error) => Some(error.as_ref()),
            Self::Capacity | Self::Join(_) => None,
        }
    }
}

/// Clears the placement's reachability flag however the pump ends — return,
/// panic, or abort.
struct ReachabilityGuard(Arc<AtomicBool>);

impl Drop for ReachabilityGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

/// The live host-plane record for a Station occupying one Orbit.
///
/// The placement may own an in-process runner or merely attach to a compatible
/// process that was already serving the Orbit. Only the owned mode participates
/// in router shutdown.
pub struct Placement {
    orbit: LocalOrbitId,
    mode: PlacementMode,
    alive: Arc<AtomicBool>,
    doorbell_pump: StdMutex<Option<tokio::task::JoinHandle<()>>>,
}

/// Which process will serve a content call, resolved before a byte moves.
///
/// Split out rather than answered inline because the two branches move bytes
/// differently — one hands a reader to a host in this address space, the other
/// forwards a declared number of bytes down a second socket — and a function
/// returning one answer for both would have to materialise the body.
pub(crate) enum ContentPlacement {
    InProcess {
        host: Arc<crate::orbital::hosting::StationHost>,
        address: OrbitAddress,
    },
    Attached {
        home: std::path::PathBuf,
    },
}

enum PlacementMode {
    Owned {
        host: std::sync::Weak<crate::orbital::hosting::StationHost>,
        stop: StationStop,
        completion: StdMutex<Option<tokio::task::JoinHandle<Result<()>>>>,
    },
    Attached,
}

impl Placement {
    pub fn orbit(&self) -> &LocalOrbitId {
        &self.orbit
    }

    pub fn hosting(&self) -> Hosting {
        match self.mode {
            PlacementMode::Owned { .. } => Hosting::InProcess,
            PlacementMode::Attached => Hosting::CompatibilityProcess,
        }
    }

    /// Whether this placement can still be reached, so a caller may reuse it
    /// instead of establishing the Station again.
    ///
    /// The doorbell subscription is deliberately not consulted. It is an event
    /// stream, not a health probe, and it ends for reasons that say nothing
    /// about the host: a Station with no standing yet closes it immediately by
    /// design (`StationHost::stream_subscribe`), which is the state every
    /// joiner is in until admission. Reading that as death rebuilt the whole
    /// Station on every request of a join — tearing down its transport
    /// registration mid-Contact — for as long as it took to be admitted.
    ///
    /// What the pump does report is *reachability*, through `alive`: it clears
    /// that flag when it cannot connect to the host at all. For an attached
    /// placement, whose host is in another process, that is the only liveness
    /// signal there is.
    fn handle_snapshot(&self) -> Option<crate::daemon::address_book::HandleSnapshot> {
        let host = match &self.mode {
            PlacementMode::Owned { host, .. } => host.upgrade()?,
            PlacementMode::Attached => return None,
        };
        Some(host.handle_snapshot())
    }

    fn is_live(&self) -> bool {
        if !self.alive.load(Ordering::Acquire) {
            return false;
        }
        match &self.mode {
            PlacementMode::Owned { completion, .. } => completion
                .lock_recovering()
                .as_ref()
                .is_some_and(|task| !task.is_finished()),
            PlacementMode::Attached => true,
        }
    }

    async fn establish(
        resolved: &ResolvedOrbit,
        doorbells: broadcast::Sender<OrbitDoorbell>,
        factory: Arc<dyn TransportFactory>,
        packages: WorldPackages,
        blocking: Arc<Semaphore>,
    ) -> Result<Self> {
        let mode = match control::probe(&resolved.home).await {
            control::Probe::Healthy { .. } => PlacementMode::Attached,
            control::Probe::Foreign { why, replaceable } => {
                return Err(crate::control::ForeignDaemon {
                    home: resolved.home.clone(),
                    why,
                    replaceable,
                }
                .into())
            }
            control::Probe::Absent => {
                match Self::start_owned(resolved, factory.as_ref(), packages, blocking).await {
                    Ok(mode) => mode,
                    Err(start_error) => {
                        // A cwd-bound CLI can win the daemon lock after our
                        // probe. If its process becomes healthy, attach to that
                        // winner; otherwise preserve our own startup diagnosis.
                        for _ in 0..20 {
                            match control::probe(&resolved.home).await {
                                control::Probe::Healthy { .. } => {
                                    return Self::observe(
                                        resolved,
                                        PlacementMode::Attached,
                                        doorbells,
                                    )
                                }
                                control::Probe::Foreign { why, replaceable } => {
                                    return Err(crate::control::ForeignDaemon {
                                        home: resolved.home.clone(),
                                        why,
                                        replaceable,
                                    }
                                    .into())
                                }
                                control::Probe::Absent => {}
                            }
                            tokio::time::sleep(Duration::from_millis(50)).await;
                        }
                        return Err(start_error);
                    }
                }
            }
        };

        Self::observe(resolved, mode, doorbells)
    }

    async fn start_owned(
        resolved: &ResolvedOrbit,
        factory: &dyn TransportFactory,
        packages: WorldPackages,
        blocking: Arc<Semaphore>,
    ) -> Result<PlacementMode> {
        if !crate::orbital::space_store_present(&resolved.home) {
            return Err(anyhow!(
                "no space at {} — found one from the local app, or enter one from an invite",
                resolved.home.display()
            ));
        }
        let seed = crate::config::load_or_create_identity(&resolved.identity_dir)?;
        let runner =
            StationRunner::start_admitted(resolved.home.clone(), seed, factory, packages, blocking)
                .await?;
        let host = runner.host();
        let stop = runner.stop_handle();
        let mut completion = tokio::spawn(runner.run());

        if let Err(readiness_error) = wait_until_control_ready(resolved, &mut completion).await {
            stop.stop();
            return match tokio::time::timeout(Duration::from_secs(15), completion).await {
                Ok(Ok(Err(run_error))) => Err(run_error),
                Ok(Err(join_error)) => {
                    Err(anyhow!("in-process StationHost task failed: {join_error}"))
                }
                _ => Err(readiness_error),
            };
        }

        Ok(PlacementMode::Owned {
            host,
            stop,
            completion: StdMutex::new(Some(completion)),
        })
    }

    fn observe(
        resolved: &ResolvedOrbit,
        mode: PlacementMode,
        doorbells: broadcast::Sender<OrbitDoorbell>,
    ) -> Result<Self> {
        let orbit = resolved.address.orbit.clone();
        let orbit_for_pump = orbit.clone();
        let pump_home = resolved.home.clone();
        let route = Some(ControlRoute::Orbit {
            address: resolved.address.clone(),
        });
        let alive = Arc::new(AtomicBool::new(true));
        let alive_for_pump = alive.clone();

        let doorbell_pump = tokio::spawn(async move {
            // The flag is cleared by dropping this guard rather than by a store
            // on the way out, so a panic inside the loop reads as a dead
            // placement too. It used to be `JoinHandle::is_finished` that
            // covered a panicking pump, by accident; `is_live` no longer asks.
            let _unreachable = ReachabilityGuard(alive_for_pump);
            let mut backoff = PUMP_BACKOFF_FLOOR;
            loop {
                // An established subscription ending is NOT a dead host. It is
                // what a Station with no standing does by design — see
                // `StationHost::stream_subscribe`, which emits one reset and
                // closes when the Station cannot dock yet. Every joiner is in
                // exactly that state until admission, so ending the pump there
                // would condemn the placement it was created for. A Station
                // going dormant and a lagging fan-out close the stream too.
                //
                // Only being unable to REACH the host at all is death, and for
                // an attached placement — whose host is in another process —
                // this loop is the sole thing that ever notices.
                let attempt = tokio::time::timeout(
                    SUBSCRIBE_TIMEOUT,
                    control::subscribe_routed(&pump_home, 0, route.clone()),
                )
                .await;
                match attempt {
                    Ok(Ok(mut subscription)) => {
                        // Every subscription opens with a reset frame. One that
                        // carries nothing else is a host saying "not yet" —
                        // which is what a Station with no standing answers, on
                        // repeat, until it is admitted. Re-subscribing to that
                        // at the floor forever would forward a reset twice a
                        // second, and a reset costs every subscriber downstream
                        // a full rebaseline. So the floor is earned by a stream
                        // that carried real news; a stream that did not backs
                        // off toward the ceiling.
                        let mut carried = 0u32;
                        loop {
                            match subscription.next().await {
                                Ok(Some(doorbell)) => {
                                    carried = carried.saturating_add(1);
                                    let _ = doorbells.send(OrbitDoorbell {
                                        orbit: orbit_for_pump.clone(),
                                        doorbell,
                                    });
                                }
                                Ok(None) => break,
                                // A frame that will not decode is a peer on
                                // another generation, not a host that has died.
                                // Re-subscribing is the authoritative check, and
                                // the backoff rate-limits it.
                                Err(error) => {
                                    tracing::debug!(
                                        orbit = %orbit_for_pump,
                                        %error,
                                        "Orbit doorbell stream ended"
                                    );
                                    break;
                                }
                            }
                        }
                        backoff = if carried > 1 {
                            PUMP_BACKOFF_FLOOR
                        } else {
                            backoff.saturating_mul(2).min(PUMP_BACKOFF_CEILING)
                        };
                    }
                    // Nothing is accepting, or the connect parked past its
                    // bound. A Windows named pipe with no free instance parks
                    // rather than erroring, so the timeout is what makes this
                    // reachable at all.
                    Ok(Err(_)) | Err(_) => {
                        tracing::warn!(
                            orbit = %orbit_for_pump,
                            "Orbit doorbell subscription cannot reach its Station host"
                        );
                        break;
                    }
                }
                tracing::debug!(
                    orbit = %orbit_for_pump,
                    ?backoff,
                    "Orbit doorbell stream closed; re-subscribing"
                );
                tokio::time::sleep(backoff).await;
            }

            let _ = doorbells.send(OrbitDoorbell {
                orbit: orbit_for_pump,
                doorbell: Doorbell {
                    reset: true,
                    ..Default::default()
                },
            });
        });

        Ok(Self {
            orbit,
            mode,
            alive,
            doorbell_pump: StdMutex::new(Some(doorbell_pump)),
        })
    }

    async fn shutdown(&self) -> Result<()> {
        self.alive.store(false, Ordering::Release);
        let pump = self.doorbell_pump.lock_recovering().take();
        if let Some(pump) = pump {
            pump.abort();
            let _ = pump.await;
        }

        let PlacementMode::Owned {
            stop, completion, ..
        } = &self.mode
        else {
            return Ok(());
        };
        stop.stop();
        let task = completion.lock_recovering().take();
        let Some(task) = task else {
            return Ok(());
        };
        match tokio::time::timeout(Duration::from_secs(10), task).await {
            Ok(Ok(result)) => result,
            Ok(Err(error)) => Err(anyhow!("in-process StationHost task failed: {error}")),
            Err(_) => Err(anyhow!(
                "in-process StationHost did not finish dormancy within 10s; it remains draining"
            )),
        }
    }
}

impl Drop for Placement {
    fn drop(&mut self) {
        if let PlacementMode::Owned { stop, .. } = &self.mode {
            // Drop cannot await. Signal the cooperative path and let the
            // detached join handle finish; normal host shutdown calls
            // `shutdown` first and observes completion.
            stop.stop();
        }
        if let Some(pump) = self.doorbell_pump.lock_recovering().take() {
            pump.abort();
        }
    }
}

async fn wait_until_control_ready(
    resolved: &ResolvedOrbit,
    completion: &mut tokio::task::JoinHandle<Result<()>>,
) -> Result<()> {
    let route = ControlRoute::Orbit {
        address: resolved.address.clone(),
    };
    for _ in 0..100 {
        if completion.is_finished() {
            return Err(anyhow!(
                "in-process StationHost exited before its control channel became ready"
            ));
        }
        let response = tokio::time::timeout(
            Duration::from_millis(100),
            control::request_routed(&resolved.home, &Request::Status, route.clone()),
        )
        .await;
        if matches!(response, Ok(Ok(Response::Status(_)))) {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    Err(anyhow!(
        "in-process StationHost did not open its control channel within 15s"
    ))
}

/// Per-Orbit occupancy with keyed single-flight placement.
///
/// The map lock is held only long enough to obtain the selected Orbit's slot.
/// The slot lock is held across placement, so two requests for one Orbit share
/// one activation while requests for different Orbits proceed concurrently.
type OrbitSlot<T> = Arc<Mutex<Option<Arc<T>>>>;

/// An Orbit held out of service: vacant, and unable to be filled while this
/// lives. Dropping it lets the next routed request place the Orbit again.
pub struct SlotVacancy<T> {
    _slot: tokio::sync::OwnedMutexGuard<Option<Arc<T>>>,
}

/// The vacancy an exclusive store operation runs inside.
pub type OrbitVacancy = SlotVacancy<Placement>;

struct OrbitOccupancy<T> {
    slots: StdMutex<HashMap<LocalOrbitId, OrbitSlot<T>>>,
}

impl<T> Default for OrbitOccupancy<T> {
    fn default() -> Self {
        Self {
            slots: StdMutex::new(HashMap::new()),
        }
    }
}

impl<T> OrbitOccupancy<T> {
    /// Observe occupancy without creating a slot or placing.
    async fn peek(&self, orbit: &LocalOrbitId) -> Option<Arc<T>> {
        let slot = {
            let slots = self.slots.lock_recovering();
            slots.get(orbit)?.clone()
        };
        let placement = slot.lock().await.clone();
        placement
    }

    async fn get_or_try_place<F, Fut, E>(
        &self,
        orbit: LocalOrbitId,
        is_live: impl Fn(&T) -> bool,
        place: F,
    ) -> Result<Arc<T>, E>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<T, E>>,
    {
        let slot = {
            let mut slots = self.slots.lock_recovering();
            slots
                .entry(orbit)
                .or_insert_with(|| Arc::new(Mutex::new(None)))
                .clone()
        };

        let mut occupied = slot.lock().await;
        if let Some(existing) = occupied.as_ref() {
            if is_live(existing) {
                return Ok(existing.clone());
            }
            *occupied = None;
        }

        let placement = Arc::new(place().await?);
        *occupied = Some(placement.clone());
        Ok(placement)
    }

    /// Empty one Orbit's slot, hand back whatever occupied it, and keep the slot
    /// empty until the returned guard is dropped.
    ///
    /// The lock is held by the guard rather than released at the handoff,
    /// because a placement that has left the slot is not yet gone: it is
    /// draining, and until it finishes it still holds the store lock the next
    /// placement would need. `get_or_try_place` waits on this same lock, so
    /// nothing can occupy the Orbit in the meantime — neither during the drain
    /// nor during whatever exclusive work the caller does next.
    ///
    /// The slot is created if absent for the same reason: an Orbit nobody has
    /// placed yet must be held vacant too, or the first request to arrive
    /// places one behind the caller's back.
    async fn vacate(&self, orbit: &LocalOrbitId) -> (SlotVacancy<T>, Option<Arc<T>>) {
        let slot = {
            let mut slots = self.slots.lock_recovering();
            slots
                .entry(orbit.clone())
                .or_insert_with(|| Arc::new(Mutex::new(None)))
                .clone()
        };
        let mut occupied = slot.lock_owned().await;
        let previous = occupied.take();
        (SlotVacancy { _slot: occupied }, previous)
    }

    async fn placements(&self) -> Vec<Arc<T>> {
        let slots: Vec<_> = self.slots.lock_recovering().values().cloned().collect();
        let mut placements = Vec::with_capacity(slots.len());
        for slot in slots {
            if let Some(placement) = slot.lock().await.clone() {
                placements.push(placement);
            }
        }
        placements
    }
}

/// The daemon's control-plane router.
///
/// It owns placement policy, not Station internals. Runtime owns activation and
/// dormancy; StationHost owns product adaptation; this router owns only the
/// decision to reuse or establish the host through which an Orbit is reached.
pub struct Router {
    catalog: Catalog,
    occupancy: OrbitOccupancy<Placement>,
    doorbells: broadcast::Sender<OrbitDoorbell>,
    factory: Arc<dyn TransportFactory>,
    packages: WorldPackages,
    /// One process-owned admission lane for blocking host/lifecycle work.
    /// Acquiring is a try operation: a control request gets bounded feedback
    /// instead of waiting on Tokio while all workers are occupied.
    blocking: Arc<Semaphore>,
    lifecycle: RwLock<()>,
    shutting_down: AtomicBool,
    book: Result<Arc<crate::daemon::address_book::AddressBookService>, String>,
    correspondence: Arc<crate::daemon::correspondence::CorrespondenceService>,
    asks: crate::daemon::sponsorship::SponsorshipAsks,
}

impl Router {
    pub fn new(catalog: Catalog, packages: WorldPackages) -> Self {
        Self::with_factory(catalog, Arc::new(DefaultFactory), packages)
    }

    pub fn with_factory(
        catalog: Catalog,
        factory: Arc<dyn TransportFactory>,
        packages: WorldPackages,
    ) -> Self {
        // Doorbells are invalidations, not state. Lagging receivers rebaseline,
        // so a bounded fan-in is both sufficient and necessary.
        let (doorbells, _) = broadcast::channel(256);
        let identity = catalog.identity().to_path_buf();
        let book = crate::daemon::address_book::AddressBookService::open(&identity)
            .map(Arc::new)
            .map_err(|error| error.to_string());
        let correspondence = Arc::new(crate::daemon::correspondence::CorrespondenceService::open(
            &identity,
        ));
        // Carried over a hosted Post when one is named. Absent, the plane stands
        // but carries nothing, and every operation says so — which is a
        // different fact from an empty mailbox and the only one worth acting on.
        if let Some(base) = crate::daemon::correspondence::configured_carrier() {
            if let Err(error) =
                correspondence.carry_over(base, crate::daemon::correspondence::now_secs())
            {
                tracing::warn!(%error, "correspondence could not be carried");
            }
        }
        let asks = crate::daemon::sponsorship::SponsorshipAsks::open(&identity);
        Self {
            catalog,
            occupancy: OrbitOccupancy::default(),
            doorbells,
            factory: Arc::new(TransportHubFactory::new(factory)),
            packages,
            blocking: Arc::new(Semaphore::new(HOST_BLOCKING_CAPACITY)),
            lifecycle: RwLock::new(()),
            shutting_down: AtomicBool::new(false),
            book,
            correspondence,
            asks,
        }
    }

    pub fn catalog(&self) -> &Catalog {
        &self.catalog
    }

    /// The immutable World generation snapshot future placements receive.
    pub fn packages(&self) -> WorldPackages {
        self.packages.clone()
    }

    pub(crate) fn book(&self) -> Result<&crate::daemon::address_book::AddressBookService, String> {
        self.book.as_ref().map(Arc::as_ref).map_err(Clone::clone)
    }

    pub(crate) fn correspondence(&self) -> &crate::daemon::correspondence::CorrespondenceService {
        &self.correspondence
    }

    pub(crate) fn asks(&self) -> &crate::daemon::sponsorship::SponsorshipAsks {
        &self.asks
    }

    /// Authored-handle snapshot of an *already placed* Orbit. Vacant, attached,
    /// or draining placements return `None` and never call [`Self::place`].
    pub(crate) async fn active_handle_snapshot(
        &self,
        orbit: &str,
    ) -> Option<crate::daemon::address_book::HandleSnapshot> {
        let resolved = self.resolve(orbit).ok()?;
        // A named agent's home is a distinct identity: its Orbit must never
        // decorate from — or leak existence bits into — this identity's book.
        if resolved.identity != crate::orbits::StationIdentity::Own {
            return None;
        }
        let placement = self.occupancy.peek(&resolved.address.orbit).await?;
        if !placement.is_live() {
            return None;
        }
        placement.handle_snapshot()
    }

    pub fn resolve(&self, id: &str) -> Result<ResolvedOrbit> {
        self.catalog.resolve(id)
    }

    /// The reviewed implementation this daemon can execute for one World.
    ///
    /// Display assignments pin this value. Exposing the already-injected
    /// package fact here lets the coordinator reject implementation drift
    /// before it places an Orbit or asks product code to render anything.
    pub fn reviewed_world_implementation(&self, world: &WorldId) -> Option<[u8; 32]> {
        self.packages.reviewed_implementation(world)
    }

    /// Immutable World distribution release running in this daemon
    /// generation. Staging may select a newer release on disk; it cannot
    /// change this fact until the daemon deliberately crosses a generation.
    pub(crate) fn world_release_version(&self, world: &WorldId) -> Option<&str> {
        self.packages.release_version(world)
    }

    pub(crate) fn lifecycle_world_ids(&self) -> impl Iterator<Item = &WorldId> {
        self.packages.lifecycle_world_ids()
    }

    fn visible_orbit_ids(&self) -> Vec<String> {
        let mut orbits: Vec<_> = self
            .catalog
            .bindings()
            .into_iter()
            .map(|binding| {
                LocalOrbitId::for_store(std::path::Path::new(&binding.entry.path)).to_string()
            })
            .collect();
        orbits.sort();
        orbits.dedup();
        orbits
    }

    pub(crate) async fn visible_orbit_ids_blocking(
        self: &Arc<Self>,
    ) -> Result<Vec<String>, BlockingFailure> {
        let lane = self.clone();
        let source = self.clone();
        lane.run_blocking(move || Ok(source.visible_orbit_ids()))
            .await
    }

    /// Run one admitted blocking operation without occupying a Tokio worker.
    /// Capacity refusal is immediate and retryable.
    pub(crate) async fn run_blocking<T, F>(&self, work: F) -> Result<T, BlockingFailure>
    where
        T: Send + 'static,
        F: FnOnce() -> Result<T> + Send + 'static,
    {
        let permit = self
            .blocking
            .clone()
            .try_acquire_owned()
            .map_err(|_| BlockingFailure::Capacity)?;
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            work()
        })
        .await
        .map_err(|error| BlockingFailure::Join(error.to_string()))?
        .map_err(BlockingFailure::Work)
    }

    /// Advance one consented lifecycle turn in an exact local Orbit.
    pub(crate) async fn advance_world_upgrade(
        &self,
        orbit: &str,
        world: WorldId,
        operation: [u8; 16],
    ) -> Result<WorldUpgradeStep> {
        let resolved = self.resolve(orbit)?;
        let (_resolved, placement) = self.place_resolved(resolved).await?;
        let host = match &placement.mode {
            PlacementMode::Owned { host, .. } => host
                .upgrade()
                .ok_or_else(|| anyhow!("owned StationHost is draining"))?,
            PlacementMode::Attached => {
                return Err(anyhow!(
                    "the Orbit is hosted by a compatibility process; stop it before resuming the native World upgrade"
                ))
            }
        };
        self.run_blocking(move || host.advance_world_upgrade(&world, operation))
            .await
            .map_err(anyhow::Error::from)
    }

    pub fn subscribe(&self) -> broadcast::Receiver<OrbitDoorbell> {
        self.doorbells.subscribe()
    }

    /// Place or reuse the Station host for one Orbit.
    pub async fn place(&self, id: &str) -> Result<ResolvedOrbit> {
        // Read permits unrelated Orbit placements to proceed concurrently.
        // Shutdown takes the write side, waits for in-flight placements, and
        // prevents any new Station from appearing behind its snapshot.
        let _lifecycle = self.lifecycle.read().await;
        if self.shutting_down.load(Ordering::Acquire) {
            return Err(anyhow!("the control router is shutting down"));
        }
        let resolved = self.resolve(id)?;
        self.place_resolved(resolved)
            .await
            .map(|(resolved, _)| resolved)
    }

    async fn place_resolved(
        &self,
        resolved: ResolvedOrbit,
    ) -> Result<(ResolvedOrbit, Arc<Placement>)> {
        let orbit = resolved.address.orbit.clone();
        let doorbells = self.doorbells.clone();
        let factory = self.factory.clone();
        let packages = self.packages.clone();
        let blocking = self.blocking.clone();
        let placement = self
            .occupancy
            .get_or_try_place(orbit, Placement::is_live, || {
                Placement::establish(&resolved, doorbells, factory, packages, blocking)
            })
            .await?;
        Ok((resolved, placement))
    }

    /// Place an explicitly addressed Orbit and reject a stale Space
    /// expectation before the request reaches its StationHost.
    pub async fn place_address(&self, address: &OrbitAddress) -> Result<ResolvedOrbit> {
        self.place_address_with_host(address)
            .await
            .map(|(resolved, _)| resolved)
    }

    async fn place_address_with_host(
        &self,
        address: &OrbitAddress,
    ) -> Result<(ResolvedOrbit, Arc<Placement>)> {
        // Address validation precedes placement: a stale or confused route must
        // not wake the Station it is subsequently refused from addressing.
        let _lifecycle = self.lifecycle.read().await;
        if self.shutting_down.load(Ordering::Acquire) {
            return Err(anyhow!("the control router is shutting down"));
        }
        let resolved = self.resolve_address(address)?;
        self.place_resolved(resolved).await
    }

    /// Dispatch a request after ensuring its Orbit has one live placement.
    pub async fn request(&self, id: &str, request: &Request) -> Result<control::Response> {
        let resolved = self.resolve(id)?;
        let route = control::station_route(resolved.address);
        self.request_routed(route, request, None).await
    }

    /// Dispatch one explicitly routed host request.
    ///
    /// The caller-facing host socket selects no Orbit out of band, so both the
    /// complete local address and the terminal Space/World boundary are checked
    /// here before the compatibility adapter is opened.
    pub async fn request_routed(
        &self,
        route: ControlRoute,
        request: &Request,
        act_as: Option<&str>,
    ) -> Result<control::Response> {
        let address = routed_address(&route)?;
        let resolved = self.place_address(address).await?;
        control::request_as_routed(&resolved.home, request, Some(route), act_as).await
    }

    /// Where a content call has to go, and how.
    ///
    /// Modelled on [`Self::call_world`] rather than on `request_routed`,
    /// because `request_routed` re-opens the per-Orbit socket even for a
    /// Station this process already owns — which for content would mean copying
    /// every byte through a second socket to reach a `ContentHost` in this
    /// address space.
    pub(crate) async fn content_placement(
        &self,
        route: &control::ControlRoute,
    ) -> Result<ContentPlacement> {
        let address = match route {
            control::ControlRoute::Orbit { address }
            | control::ControlRoute::World { address, .. } => address,
            control::ControlRoute::Daemon => {
                return Err(anyhow!("a content call requires an explicit Space route"))
            }
        };
        let (resolved, placement) = self.place_address_with_host(address).await?;
        match &placement.mode {
            PlacementMode::Owned { host, .. } => {
                let host = host
                    .upgrade()
                    .ok_or_else(|| anyhow!("owned StationHost is draining"))?;
                Ok(ContentPlacement::InProcess {
                    host,
                    address: resolved.address.clone(),
                })
            }
            PlacementMode::Attached => Ok(ContentPlacement::Attached {
                home: resolved.home,
            }),
        }
    }

    /// Subscribe to the native media sessions for one explicitly resolved
    /// Orbit. Media is transport-live state and is therefore available only
    /// from an in-process StationHost; a compatibility process cannot be
    /// silently treated as an equivalent byte source.
    pub(crate) async fn live_media(
        &self,
        address: &OrbitAddress,
    ) -> Result<(
        Vec<runtime::plane::live::media::Session>,
        tokio::sync::broadcast::Receiver<runtime::plane::live::media::Event>,
    )> {
        let (_resolved, placement) = self.place_address_with_host(address).await?;
        match &placement.mode {
            PlacementMode::Owned { host, .. } => host
                .upgrade()
                .map(|host| host.live_media())
                .ok_or_else(|| anyhow!("owned StationHost is draining")),
            PlacementMode::Attached => Err(anyhow!(
                "native live media is owned by an attached StationHost process"
            )),
        }
    }

    /// Dispatch one product-neutral call to its explicitly addressed World.
    ///
    /// Owned placements are invoked directly in-process. An attached
    /// StationHost receives the same opaque envelope over its per-Orbit socket;
    /// the router never decodes or translates product payloads.
    pub async fn call_world(
        &self,
        route: ControlRoute,
        call: &Call,
        act_as: Option<&str>,
    ) -> Result<Reply> {
        call.validate()?;
        let ControlRoute::World { address, world } = &route else {
            return Err(anyhow!("World call requires an explicit World route"));
        };
        let Some(route_world) = replica::body::WorldId::parse(world) else {
            return Err(anyhow!("invalid World id '{world}'"));
        };
        if &route_world != call.world() {
            return Err(anyhow!(
                "World route addresses {route_world}, but the call addresses {}",
                call.world()
            ));
        }
        if !self.packages.contains(call.world()) {
            return Err(anyhow!(
                "World '{}' has no selected installation in this Lait daemon",
                call.world()
            ));
        }
        if let Err(error) = self.packages.call_access(call) {
            return Ok(Reply::error(call, error.code, error.message()));
        }

        let (resolved, placement) = self.place_address_with_host(address).await?;
        match &placement.mode {
            PlacementMode::Owned { host, .. } => {
                let Some(host) = host.upgrade() else {
                    return Ok(Reply::error(
                        call,
                        Code::Unavailable,
                        "owned StationHost is draining",
                    ));
                };
                Ok(host.call_world(address, call, act_as))
            }
            PlacementMode::Attached => {
                control::call_world(&resolved.home, route, call.clone(), act_as).await
            }
        }
    }

    /// Execute a World call only when the daemon's trusted package handler
    /// classifies it as the required access.
    ///
    /// Display coordination uses this with `Access::Query`. The outer client
    /// package declaration is useful for early refusal, but it is not an
    /// authorization fact; this check runs against the host-side handler before
    /// placement and before any product code executes.
    pub async fn call_world_requiring(
        &self,
        route: ControlRoute,
        call: &Call,
        required: runtime::world::call::Access,
    ) -> Result<Reply> {
        call.validate()?;
        let actual = match self.packages.call_access(call) {
            Ok(access) => access,
            Err(error) => return Ok(Reply::error(call, error.code, error.message())),
        };
        if actual != required {
            return Ok(Reply::error(
                call,
                Code::Denied,
                "World call does not satisfy the required access",
            ));
        }
        // No acting-identity selector exists on this operation. The
        // coordinator always executes as its configured local identity.
        self.call_world(route, call, None).await
    }

    /// Dispatch through an existing per-Orbit adapter without placing a vacant
    /// Orbit. This keeps passive catalog reads behind the daemon boundary while
    /// preserving their no-wake contract.
    pub async fn request_running(
        &self,
        route: ControlRoute,
        request: &Request,
    ) -> Result<control::Response> {
        let address = routed_address(&route)?;
        let _lifecycle = self.lifecycle.read().await;
        if self.shutting_down.load(Ordering::Acquire) {
            return Err(anyhow!("the control router is shutting down"));
        }
        let resolved = self.resolve_address(address)?;
        control::request_routed(&resolved.home, request, route).await
    }

    fn resolve_address(&self, address: &OrbitAddress) -> Result<ResolvedOrbit> {
        let resolved = self.resolve(address.orbit.as_str())?;
        if resolved.address != *address {
            return Err(anyhow!(
                "local Orbit {} is registered for Space {}, not {}",
                address.orbit,
                resolved.address.space,
                address.space
            ));
        }
        Ok(resolved)
    }

    /// Stop one Orbit's placement and hold the Orbit vacant, so an exclusive
    /// store operation can have the lock this process was holding.
    ///
    /// Exclusion lasts as long as the returned guard, not as long as this call:
    /// the operation that follows needs the Orbit to stay empty while it runs,
    /// and a routed request arriving mid-operation is the ordinary case, not the
    /// unlucky one. Dropping the guard re-opens the Orbit and the next request
    /// places it lazily, exactly as if it had never been placed.
    ///
    /// An attached placement is another process's to stop, so this only forgets
    /// it — the operation that follows will say so in its own words rather than
    /// have this one guess on its behalf.
    pub async fn vacate(&self, orbit: &LocalOrbitId) -> Result<OrbitVacancy> {
        let (vacancy, placement) = self.occupancy.vacate(orbit).await;
        if let Some(placement) = placement {
            placement.shutdown().await?;
        }
        Ok(vacancy)
    }

    /// Stop and join every in-process placement. Externally attached
    /// compatibility daemons are left running.
    pub async fn shutdown(&self) -> Result<()> {
        // The lifecycle guard excludes placements while we tear down — but the
        // read side is held across a whole lazy placement, which includes a
        // transport build that can sit for tens of seconds. Waiting for it
        // without a bound is how a stop that races a first board load becomes a
        // daemon that never exits. Past the deadline, shut down anyway: a
        // placement still arriving is a placement whose Station will be torn
        // down by the process leaving, and an unbounded wait here is worse than
        // an unsynchronised one.
        let lifecycle = tokio::time::timeout(LIFECYCLE_DEADLINE, self.lifecycle.write()).await;
        if lifecycle.is_err() {
            tracing::warn!(
                seconds = LIFECYCLE_DEADLINE.as_secs(),
                "a placement was still in flight at shutdown — draining without the lifecycle guard"
            );
        }
        if self.shutting_down.swap(true, Ordering::AcqRel) {
            return Ok(());
        }

        let mut tasks = tokio::task::JoinSet::new();
        for placement in self.occupancy.placements().await {
            tasks.spawn(async move { placement.shutdown().await });
        }
        let mut failures = Vec::new();
        while let Some(result) = tasks.join_next().await {
            match result {
                Ok(Ok(())) => {}
                Ok(Err(error)) => failures.push(error.to_string()),
                Err(error) => failures.push(format!("shutdown task failed: {error}")),
            }
        }
        // Stations unregister their Space-scoped views during dormancy. Only
        // after every placement has joined may the identity endpoints close.
        self.factory.shutdown().await;
        if failures.is_empty() {
            Ok(())
        } else {
            Err(anyhow!(
                "one or more Station placements failed to shut down: {}",
                failures.join("; ")
            ))
        }
    }
}

fn routed_address(route: &ControlRoute) -> Result<&OrbitAddress> {
    match route {
        ControlRoute::Daemon => Err(anyhow!(
            "daemon-scoped request cannot be dispatched to a Station"
        )),
        ControlRoute::Orbit { address } => Ok(address),
        ControlRoute::World { .. } => Err(anyhow!(
            "typed product requests were retired in control protocol v5; \
             send a versioned World call"
        )),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;

    use async_trait::async_trait;

    use super::*;
    use crate::orbits::{Entry, Origin};
    use comms::mem::MemNet;
    use comms::policy::Network;
    use comms::Transport;

    static HOME_COUNTER: AtomicUsize = AtomicUsize::new(0);

    struct MemFactory(MemNet);

    #[async_trait]
    impl TransportFactory for MemFactory {
        async fn build(
            &self,
            identity_seed: &[u8; 32],
            _network: &Network,
            _protocols: comms::Protocols<'_>,
        ) -> Result<Arc<dyn Transport>> {
            Ok(Arc::new(
                self.0
                    .peer(mechanics::actor::device_from_seed(identity_seed)),
            ))
        }
    }

    fn orbit(name: &str) -> LocalOrbitId {
        LocalOrbitId::for_store(std::path::Path::new(name))
    }

    fn formed_directory(tag: &str, seed: &[u8; 32]) -> (std::path::PathBuf, Catalog, String) {
        let n = HOME_COUNTER.fetch_add(1, Ordering::SeqCst);
        let home =
            std::env::temp_dir().join(format!("lait-router-{tag}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        let (mechanics, _) =
            crate::orbital::form_space(&crate::world::packages(), &home, seed, "Router Test")
                .unwrap();
        std::fs::write(
            home.join("secret.key"),
            data_encoding::HEXLOWER.encode(seed),
        )
        .unwrap();
        let id = LocalOrbitId::for_store(&home).as_str().to_string();
        let directory = Catalog::with_entries(
            home.clone(),
            home.join("agents"),
            false,
            vec![Entry {
                space: mechanics.space().as_str().to_string(),
                name: "Router Test".into(),
                path: home.to_string_lossy().to_string(),
                origin: Origin::Founded,
                host_nick: String::new(),
                last_opened: 1,
            }],
        );
        (home, directory, id)
    }

    #[tokio::test]
    async fn one_orbit_places_once_for_concurrent_callers() {
        let occupancy = Arc::new(OrbitOccupancy::<usize>::default());
        let starts = Arc::new(AtomicUsize::new(0));
        let mut calls = tokio::task::JoinSet::new();

        for _ in 0..12 {
            let occupancy = occupancy.clone();
            let starts = starts.clone();
            calls.spawn(async move {
                occupancy
                    .get_or_try_place(
                        orbit("/one"),
                        |_| true,
                        || async move {
                            let start = starts.fetch_add(1, Ordering::SeqCst) + 1;
                            tokio::task::yield_now().await;
                            Ok::<_, ()>(start)
                        },
                    )
                    .await
                    .unwrap()
            });
        }

        let placements = calls.join_all().await;
        assert_eq!(starts.load(Ordering::SeqCst), 1);
        assert!(placements
            .windows(2)
            .all(|pair| Arc::ptr_eq(&pair[0], &pair[1])));
    }

    /// The exclusion an exclusive store operation actually needs.
    ///
    /// Emptying the slot and unlocking it in the same breath narrows the race
    /// without closing it: the drained placement is still letting go of the
    /// store lock, and the operation that asked for the vacancy has not even
    /// started. The next routed request would re-place the Orbit underneath
    /// both.
    #[tokio::test]
    async fn a_vacancy_keeps_an_orbit_empty_for_as_long_as_it_is_held() {
        let occupancy = Arc::new(OrbitOccupancy::<usize>::default());
        let id = orbit("/vacated");
        occupancy
            .get_or_try_place(id.clone(), |_| true, || async { Ok::<_, ()>(1usize) })
            .await
            .unwrap();

        let (vacancy, drained) = occupancy.vacate(&id).await;
        assert_eq!(drained.as_deref(), Some(&1));

        let placed = Arc::new(AtomicUsize::new(0));
        let waiting = tokio::spawn({
            let occupancy = occupancy.clone();
            let id = id.clone();
            let placed = placed.clone();
            async move {
                occupancy
                    .get_or_try_place(
                        id,
                        |_| true,
                        || async move {
                            placed.fetch_add(1, Ordering::SeqCst);
                            Ok::<_, ()>(2usize)
                        },
                    )
                    .await
                    .unwrap()
            }
        });

        // Every chance to win the race the guard exists to lose.
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
        assert_eq!(
            placed.load(Ordering::SeqCst),
            0,
            "nothing may occupy the Orbit while the vacancy is held"
        );

        drop(vacancy);
        assert_eq!(*waiting.await.unwrap(), 2, "and it re-places once released");
    }

    /// An attached placement whose doorbell pump has ended, the way a
    /// pre-admission joiner's does within milliseconds of being placed.
    /// Callers yield before asserting, so the handle really is finished.
    fn placement_with_a_finished_pump() -> Placement {
        Placement {
            orbit: orbit("/finished-pump"),
            mode: PlacementMode::Attached,
            alive: Arc::new(AtomicBool::new(true)),
            doorbell_pump: StdMutex::new(Some(tokio::spawn(async {}))),
        }
    }

    /// A Station with no standing closes its doorbell stream at once and by
    /// design, so the pump ending says nothing about the host.
    #[tokio::test]
    async fn a_finished_doorbell_pump_does_not_mean_a_dead_placement() {
        let placement = placement_with_a_finished_pump();
        // Let the replacement handle finish too, so the old `is_finished`
        // clause would fire if it were still there.
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            placement.is_live(),
            "an ended doorbell subscription is not a dead Station"
        );

        placement.alive.store(false, Ordering::Release);
        assert!(
            !placement.is_live(),
            "an unreachable host — what the pump actually reports — is dead"
        );
    }

    /// The consequence the predicate exists for. This is the regression: a
    /// joiner rebuilt its whole Station, transport registration and all, on
    /// every request of its join loop.
    #[tokio::test]
    async fn a_placement_whose_doorbell_stream_ended_is_reused_not_rebuilt() {
        let occupancy = OrbitOccupancy::<Placement>::default();
        let id = orbit("/reused");
        let builds = Arc::new(AtomicUsize::new(0));

        let mut placed = Vec::new();
        for _ in 0..2 {
            let builds = builds.clone();
            placed.push(
                occupancy
                    .get_or_try_place(id.clone(), Placement::is_live, || async move {
                        builds.fetch_add(1, Ordering::SeqCst);
                        Ok::<_, ()>(placement_with_a_finished_pump())
                    })
                    .await
                    .unwrap(),
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }

        assert_eq!(
            builds.load(Ordering::SeqCst),
            1,
            "the second request must reuse the placement, not establish a second Station"
        );
        assert!(Arc::ptr_eq(&placed[0], &placed[1]));
    }

    /// The risk the change above takes on: with the pump task no longer
    /// consulted, an attached placement is only ever declared dead by the
    /// pump failing to reach it. If that stops working, nothing else notices.
    #[tokio::test]
    async fn a_host_that_cannot_be_reached_is_declared_dead() {
        let (home, catalog, id) = formed_directory("unreachable", &[41u8; 32]);
        let resolved = catalog.resolve(&id).expect("resolve");

        // The store is real, but no Station host was ever placed here, so
        // nothing is listening on its control channel and nothing will be.
        let placement =
            Placement::observe(&resolved, PlacementMode::Attached, broadcast::channel(8).0)
                .expect("observe");

        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        while placement.is_live() && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(
            !placement.is_live(),
            "a placement whose host cannot be reached must not stay live forever"
        );

        let _ = std::fs::remove_dir_all(&home);
    }

    #[tokio::test]
    async fn different_orbits_place_concurrently() {
        let occupancy = Arc::new(OrbitOccupancy::<usize>::default());
        let barrier = Arc::new(tokio::sync::Barrier::new(2));
        let place = |id: &'static str, value| {
            let occupancy = occupancy.clone();
            let barrier = barrier.clone();
            tokio::spawn(async move {
                occupancy
                    .get_or_try_place(
                        orbit(id),
                        |_| true,
                        || async move {
                            barrier.wait().await;
                            Ok::<_, ()>(value)
                        },
                    )
                    .await
            })
        };

        let (a, b) = tokio::time::timeout(std::time::Duration::from_secs(1), async {
            tokio::join!(place("/a", 1), place("/b", 2))
        })
        .await
        .expect("different Orbit slots must not block each other");
        assert_eq!(*a.unwrap().unwrap(), 1);
        assert_eq!(*b.unwrap().unwrap(), 2);
    }

    #[tokio::test]
    async fn failed_placement_leaves_orbit_vacant_for_retry() {
        let occupancy = OrbitOccupancy::<usize>::default();
        let id = orbit("/retry");
        let failed = occupancy
            .get_or_try_place(id.clone(), |_| true, || async { Err::<usize, _>("failed") })
            .await;
        assert_eq!(failed.unwrap_err(), "failed");

        let placed = occupancy
            .get_or_try_place(id, |_| true, || async { Ok::<_, &str>(7) })
            .await
            .unwrap();
        assert_eq!(*placed, 7);
    }

    #[tokio::test]
    async fn invalid_world_call_is_rejected_before_it_places_a_station() {
        let seed = [200; 32];
        let (home, directory, id) = formed_directory("invalid-call", &seed);
        let router = Router::with_factory(
            directory,
            Arc::new(MemFactory(MemNet::new())),
            crate::world::packages(),
        );
        let resolved = router.resolve(&id).unwrap();
        let call = Call::new(
            crate::world::contract::world_id(),
            issues_app::IssuesCallHandler::OPERATION,
            issues_app::IssuesCallHandler::VERSION + 1,
            serde_json::to_vec(&issues_app::IssuesRequest::ProjectList {
                page: issues::contract::PageRequest::default(),
            })
            .unwrap(),
        )
        .unwrap();
        let reply = router
            .call_world(
                ControlRoute::World {
                    address: resolved.address,
                    world: call.world().as_str().to_string(),
                },
                &call,
                None,
            )
            .await
            .unwrap();
        let error = reply.into_result().unwrap_err();
        assert_eq!(error.code, runtime::world::call::Code::UnsupportedVersion);
        assert!(router.occupancy.placements().await.is_empty());
        drop(
            crate::config::acquire_daemon_lock(&home)
                .expect("invalid product calls must not wake a vacant Orbit"),
        );
        let _ = std::fs::remove_dir_all(home);
    }

    #[tokio::test]
    async fn required_query_refuses_a_command_before_it_places_a_station() {
        let seed = [199; 32];
        let (home, directory, id) = formed_directory("required-query", &seed);
        let router = Router::with_factory(
            directory,
            Arc::new(MemFactory(MemNet::new())),
            crate::world::packages(),
        );
        let resolved = router.resolve(&id).unwrap();
        let call = issues_app::encode_call(&issues_app::IssuesRequest::IssueNew {
            title: "must not execute".into(),
            project: None,
            project_hint: None,
            assignees: Vec::new(),
            priority: None,
            labels: Vec::new(),
            body: None,
            due: None,
            estimate: None,
        })
        .unwrap();
        let reply = router
            .call_world_requiring(
                ControlRoute::World {
                    address: resolved.address,
                    world: call.world().as_str().to_string(),
                },
                &call,
                runtime::world::call::Access::Query,
            )
            .await
            .unwrap();
        assert_eq!(
            reply.into_result().unwrap_err().code,
            runtime::world::call::Code::Denied
        );
        assert!(router.occupancy.placements().await.is_empty());
        drop(
            crate::config::acquire_daemon_lock(&home)
                .expect("a refused display query must not wake a vacant Orbit"),
        );
        let _ = std::fs::remove_dir_all(home);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn saturated_open_lane_refuses_without_stalling_the_reactor() {
        let seed = [198; 32];
        let (home, directory, id) = formed_directory("open-capacity", &seed);
        let router = Arc::new(Router::with_factory(
            directory,
            Arc::new(MemFactory(MemNet::new())),
            crate::world::packages(),
        ));
        let capacity = u32::try_from(HOST_BLOCKING_CAPACITY).expect("small blocking capacity");
        let permits = router
            .blocking
            .clone()
            .acquire_many_owned(capacity)
            .await
            .unwrap();
        let ticks = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let ticks_for_heartbeat = ticks.clone();
        let heartbeat = tokio::spawn(async move {
            for _ in 0..8 {
                tokio::time::sleep(Duration::from_millis(5)).await;
                ticks_for_heartbeat.fetch_add(1, Ordering::Relaxed);
            }
        });

        let refused = tokio::time::timeout(Duration::from_secs(2), router.place(&id))
            .await
            .expect("bounded open admission returns promptly")
            .unwrap_err();
        heartbeat.await.unwrap();
        assert!(
            format!("{refused:#}").contains("blocking lane is saturated"),
            "unexpected refusal: {refused:#}"
        );
        assert_eq!(ticks.load(Ordering::Relaxed), 8);

        drop(permits);
        router.shutdown().await.unwrap();
        let _ = std::fs::remove_dir_all(home);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn vacant_orbit_is_owned_in_process_and_shutdown_returns_it_to_vacancy() {
        let seed = [201; 32];
        let (home, directory, id) = formed_directory("owned", &seed);
        let router = Router::with_factory(
            directory,
            Arc::new(MemFactory(MemNet::new())),
            crate::world::packages(),
        );

        assert!(matches!(
            router.request(&id, &Request::Status).await.unwrap(),
            Response::Status(_)
        ));
        let resolved = router.resolve(&id).unwrap();
        let call = issues_app::encode_call(&issues_app::IssuesRequest::ProjectList {
            page: issues::contract::PageRequest::default(),
        })
        .unwrap();
        let reply = router
            .call_world(
                ControlRoute::World {
                    address: resolved.address,
                    world: call.world().as_str().to_string(),
                },
                &call,
                None,
            )
            .await
            .unwrap();
        let value = issues_app::decode_reply(&call, reply).unwrap();
        assert!(matches!(
            serde_json::from_value::<issues_app::IssuesResponse>(value).unwrap(),
            issues_app::IssuesResponse::Projects { .. }
        ));
        let placements = router.occupancy.placements().await;
        assert_eq!(placements.len(), 1);
        assert_eq!(placements[0].hosting(), Hosting::InProcess);
        assert!(
            crate::config::acquire_daemon_lock(&home).is_err(),
            "the owned runner must retain the Orbit lease"
        );

        router.shutdown().await.unwrap();
        assert!(router.place(&id).await.is_err(), "shutdown is terminal");
        let lease = crate::config::acquire_daemon_lock(&home)
            .expect("dormancy releases the Orbit's process lease");
        drop(lease);
        let _ = std::fs::remove_dir_all(home);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn attached_compatibility_process_is_not_stopped_by_router_shutdown() {
        let seed = [202; 32];
        let (home, directory, id) = formed_directory("attached", &seed);
        let net = MemNet::new();
        let runner = StationRunner::start(
            home.clone(),
            seed,
            &MemFactory(net.clone()),
            crate::world::packages(),
        )
        .await
        .unwrap();
        let stop = runner.stop_handle();
        let mut completion = tokio::spawn(runner.run());
        let resolved = directory.resolve(&id).unwrap();
        wait_until_control_ready(&resolved, &mut completion)
            .await
            .unwrap();

        let router = Router::with_factory(
            directory,
            Arc::new(MemFactory(net)),
            crate::world::packages(),
        );
        assert!(matches!(
            router.request(&id, &Request::Status).await.unwrap(),
            Response::Status(_)
        ));
        let call = issues_app::encode_call(&issues_app::IssuesRequest::ProjectList {
            page: issues::contract::PageRequest::default(),
        })
        .unwrap();
        let reply = router
            .call_world(
                ControlRoute::World {
                    address: resolved.address.clone(),
                    world: call.world().as_str().to_string(),
                },
                &call,
                None,
            )
            .await
            .unwrap();
        let value = issues_app::decode_reply(&call, reply).unwrap();
        assert!(matches!(
            serde_json::from_value::<issues_app::IssuesResponse>(value).unwrap(),
            issues_app::IssuesResponse::Projects { .. }
        ));
        let placements = router.occupancy.placements().await;
        assert_eq!(placements.len(), 1);
        assert_eq!(placements[0].hosting(), Hosting::CompatibilityProcess);

        router.shutdown().await.unwrap();
        assert!(
            matches!(
                control::request(&home, &Request::Status).await,
                Ok(Response::Status(_))
            ),
            "an attached daemon remains owned by its original host"
        );

        stop.stop();
        completion.await.unwrap().unwrap();
        let _ = std::fs::remove_dir_all(home);
    }
}
