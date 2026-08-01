//! Local control routing and Station placement.
//!
//! A [`ControlRouter`] is the sole host-plane entrance from a catalog-wide
//! client to an Orbit. It resolves the Orbit through [`OrbitDirectory`], places
//! or reuses exactly one Station host for that Orbit, and dispatches Space
//! control plus product-neutral World calls. Its transport factory shares one
//! concrete endpoint per device identity across the owned placements. A vacant
//! Orbit is hosted in-process; a compatible pre-existing per-home daemon is
//! attached as an external placement. Per-home IPC remains an internal
//! compatibility adapter behind the identity-scoped Lait daemon endpoint.

use std::collections::HashMap;
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, Mutex, RwLock};

use crate::control::{self, ControlRoute, Doorbell, Request, Response};
use crate::orbital::space_bridge::{StationHostRunner, StationHostStop};
use crate::orbital::{CallFailureCode, WorldCall, WorldPackages, WorldReply};
use crate::transport::{DefaultFactory, TransportFactory};

use super::transport_hub::TransportHubFactory;
use super::{LocalOrbitId, OrbitAddress, OrbitDirectory, ResolvedOrbit};

/// A Station placement's current hosting strategy.
///
/// This enum makes process hosting an adapter choice rather than the definition
/// of a Station. An in-process or isolated plugin worker can be added without
/// changing Orbit addressing or the client protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlacementHost {
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

/// The live host-plane record for a Station occupying one Orbit.
///
/// The placement may own an in-process runner or merely attach to a compatible
/// process that was already serving the Orbit. Only the owned mode participates
/// in router shutdown.
pub struct StationPlacement {
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
pub enum ContentPlacement {
    InProcess {
        bridge: Arc<crate::orbital::space_bridge::StationHost>,
        address: OrbitAddress,
    },
    Attached {
        home: std::path::PathBuf,
    },
}

enum PlacementMode {
    Owned {
        bridge: std::sync::Weak<crate::orbital::space_bridge::StationHost>,
        stop: StationHostStop,
        completion: StdMutex<Option<tokio::task::JoinHandle<Result<()>>>>,
    },
    Attached,
}

impl StationPlacement {
    pub fn orbit(&self) -> &LocalOrbitId {
        &self.orbit
    }

    pub fn host(&self) -> PlacementHost {
        match self.mode {
            PlacementMode::Owned { .. } => PlacementHost::InProcess,
            PlacementMode::Attached => PlacementHost::CompatibilityProcess,
        }
    }

    fn is_live(&self) -> bool {
        if !self.alive.load(Ordering::Acquire)
            || !self
                .doorbell_pump
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .as_ref()
                .is_some_and(|task| !task.is_finished())
        {
            return false;
        }
        match &self.mode {
            PlacementMode::Owned { completion, .. } => completion
                .lock()
                .unwrap_or_else(|error| error.into_inner())
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
    ) -> Result<Self> {
        let mode = match control::probe(&resolved.home).await {
            control::Probe::Healthy => PlacementMode::Attached,
            control::Probe::Foreign { why, replaceable } => {
                return Err(crate::cli::ForeignDaemon {
                    home: resolved.home.clone(),
                    why,
                    replaceable,
                }
                .into())
            }
            control::Probe::Absent => {
                match Self::start_owned(resolved, factory.as_ref(), packages).await {
                    Ok(mode) => mode,
                    Err(start_error) => {
                        // A cwd-bound CLI can win the daemon lock after our
                        // probe. If its process becomes healthy, attach to that
                        // winner; otherwise preserve our own startup diagnosis.
                        for _ in 0..20 {
                            match control::probe(&resolved.home).await {
                                control::Probe::Healthy => {
                                    return Self::observe(
                                        resolved,
                                        PlacementMode::Attached,
                                        doorbells,
                                    )
                                }
                                control::Probe::Foreign { why, replaceable } => {
                                    return Err(crate::cli::ForeignDaemon {
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
    ) -> Result<PlacementMode> {
        if !crate::orbital::space_store_present(&resolved.home) {
            return Err(anyhow!(
                "no space at {} — found one with `lait init`, or join one with `lait join <link>`",
                resolved.home.display()
            ));
        }
        let seed = crate::config::load_or_create_identity(&resolved.identity_dir)?;
        let runner =
            StationHostRunner::start(resolved.home.clone(), seed, factory, packages).await?;
        let bridge = runner.bridge_handle();
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
            bridge,
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
        let pump_alive = alive.clone();

        let doorbell_pump = tokio::spawn(async move {
            match control::subscribe_routed(&pump_home, 0, route).await {
                Ok(mut subscription) => loop {
                    match subscription.next().await {
                        Ok(Some(doorbell)) => {
                            let _ = doorbells.send(OrbitDoorbell {
                                orbit: orbit_for_pump.clone(),
                                doorbell,
                            });
                        }
                        Ok(None) => break,
                        Err(error) => {
                            tracing::warn!(
                                orbit = %orbit_for_pump,
                                %error,
                                "Orbit doorbell stream ended"
                            );
                            break;
                        }
                    }
                },
                Err(error) => tracing::warn!(
                    orbit = %orbit_for_pump,
                    %error,
                    "Orbit doorbell subscription failed"
                ),
            }

            pump_alive.store(false, Ordering::Release);
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
        let pump = self
            .doorbell_pump
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take();
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
        let task = completion
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take();
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

impl Drop for StationPlacement {
    fn drop(&mut self) {
        if let PlacementMode::Owned { stop, .. } = &self.mode {
            // Drop cannot await. Signal the cooperative path and let the
            // detached join handle finish; normal host shutdown calls
            // `shutdown` first and observes completion.
            stop.stop();
        }
        if let Some(pump) = self
            .doorbell_pump
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
        {
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
            let mut slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
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

    async fn placements(&self) -> Vec<Arc<T>> {
        let slots: Vec<_> = self
            .slots
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .values()
            .cloned()
            .collect();
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
pub struct ControlRouter {
    directory: OrbitDirectory,
    occupancy: OrbitOccupancy<StationPlacement>,
    doorbells: broadcast::Sender<OrbitDoorbell>,
    factory: Arc<dyn TransportFactory>,
    packages: WorldPackages,
    lifecycle: RwLock<()>,
    shutting_down: AtomicBool,
}

impl ControlRouter {
    pub fn new(directory: OrbitDirectory, packages: WorldPackages) -> Self {
        Self::with_factory(directory, Arc::new(DefaultFactory), packages)
    }

    pub fn with_factory(
        directory: OrbitDirectory,
        factory: Arc<dyn TransportFactory>,
        packages: WorldPackages,
    ) -> Self {
        // Doorbells are invalidations, not state. Lagging receivers rebaseline,
        // so a bounded fan-in is both sufficient and necessary.
        let (doorbells, _) = broadcast::channel(256);
        Self {
            directory,
            occupancy: OrbitOccupancy::default(),
            doorbells,
            factory: Arc::new(TransportHubFactory::new(factory)),
            packages,
            lifecycle: RwLock::new(()),
            shutting_down: AtomicBool::new(false),
        }
    }

    pub fn directory(&self) -> &OrbitDirectory {
        &self.directory
    }

    pub fn resolve(&self, id: &str) -> Result<ResolvedOrbit> {
        self.directory.resolve(id)
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
    ) -> Result<(ResolvedOrbit, Arc<StationPlacement>)> {
        let orbit = resolved.address.orbit.clone();
        let doorbells = self.doorbells.clone();
        let factory = self.factory.clone();
        let packages = self.packages.clone();
        let placement = self
            .occupancy
            .get_or_try_place(orbit, StationPlacement::is_live, || {
                StationPlacement::establish(&resolved, doorbells, factory, packages)
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
    ) -> Result<(ResolvedOrbit, Arc<StationPlacement>)> {
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
    pub async fn content_placement(
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
            PlacementMode::Owned { bridge, .. } => {
                let bridge = bridge
                    .upgrade()
                    .ok_or_else(|| anyhow!("owned StationHost is draining"))?;
                Ok(ContentPlacement::InProcess {
                    bridge,
                    address: resolved.address.clone(),
                })
            }
            PlacementMode::Attached => Ok(ContentPlacement::Attached {
                home: resolved.home,
            }),
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
        call: &WorldCall,
        act_as: Option<&str>,
    ) -> Result<WorldReply> {
        call.validate()?;
        let ControlRoute::World { address, world } = &route else {
            return Err(anyhow!("World call requires an explicit World route"));
        };
        let Some(route_world) = replica::ids::WorldId::parse(world) else {
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
                "World '{}' is not bundled by this Lait daemon",
                call.world()
            ));
        }
        if let Err(error) = self.packages.call_access(call) {
            return Ok(WorldReply::error(call, error.code, error.message));
        }

        let (resolved, placement) = self.place_address_with_host(address).await?;
        match &placement.mode {
            PlacementMode::Owned { bridge, .. } => {
                let Some(bridge) = bridge.upgrade() else {
                    return Ok(WorldReply::error(
                        call,
                        CallFailureCode::Unavailable,
                        "owned StationHost is draining",
                    ));
                };
                Ok(bridge.call_world(address, call, act_as))
            }
            PlacementMode::Attached => {
                control::call_world(&resolved.home, route, call.clone(), act_as).await
            }
        }
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

    /// Stop and join every in-process placement. Externally attached
    /// compatibility daemons are left running.
    pub async fn shutdown(&self) -> Result<()> {
        let _lifecycle = self.lifecycle.write().await;
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
    use crate::net::Network;
    use crate::orbits::{Entry, Origin};
    use crate::transport::mem::MemNet;
    use crate::transport::Transport;

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
                self.0.peer(crate::crypto::device_from_seed(identity_seed)),
            ))
        }
    }

    fn orbit(name: &str) -> LocalOrbitId {
        LocalOrbitId::for_store(std::path::Path::new(name))
    }

    fn formed_directory(
        tag: &str,
        seed: &[u8; 32],
    ) -> (std::path::PathBuf, OrbitDirectory, String) {
        let n = HOME_COUNTER.fetch_add(1, Ordering::SeqCst);
        let home =
            std::env::temp_dir().join(format!("lait-router-{tag}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        let (mechanics, _) = crate::orbital::form_space(&home, seed, "Router Test").unwrap();
        std::fs::write(
            home.join("secret.key"),
            data_encoding::HEXLOWER.encode(seed),
        )
        .unwrap();
        let id = LocalOrbitId::for_store(&home).as_str().to_string();
        let directory = OrbitDirectory::with_entries(
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
                projects: Vec::new(),
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
        let router = ControlRouter::with_factory(
            directory,
            Arc::new(MemFactory(MemNet::new())),
            crate::world::packages(),
        );
        let resolved = router.resolve(&id).unwrap();
        let call = WorldCall::new(
            crate::world::contract::world_id(),
            issues_app::IssuesCallHandler::OPERATION,
            issues_app::IssuesCallHandler::VERSION + 1,
            serde_json::to_vec(&issues_app::IssuesRequest::ProjectList).unwrap(),
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
        assert_eq!(
            error.code,
            crate::orbital::CallFailureCode::UnsupportedVersion
        );
        assert!(router.occupancy.placements().await.is_empty());
        drop(
            crate::config::acquire_daemon_lock(&home)
                .expect("invalid product calls must not wake a vacant Orbit"),
        );
        let _ = std::fs::remove_dir_all(home);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn vacant_orbit_is_owned_in_process_and_shutdown_returns_it_to_vacancy() {
        let seed = [201; 32];
        let (home, directory, id) = formed_directory("owned", &seed);
        let router = ControlRouter::with_factory(
            directory,
            Arc::new(MemFactory(MemNet::new())),
            crate::world::packages(),
        );

        assert!(matches!(
            router.request(&id, &Request::Status).await.unwrap(),
            Response::Status(_)
        ));
        let resolved = router.resolve(&id).unwrap();
        let call = issues_app::encode_call(&issues_app::IssuesRequest::ProjectList).unwrap();
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
        assert_eq!(placements[0].host(), PlacementHost::InProcess);
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
        let runner = StationHostRunner::start(
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

        let router = ControlRouter::with_factory(
            directory,
            Arc::new(MemFactory(net)),
            crate::world::packages(),
        );
        assert!(matches!(
            router.request(&id, &Request::Status).await.unwrap(),
            Response::Status(_)
        ));
        let call = issues_app::encode_call(&issues_app::IssuesRequest::ProjectList).unwrap();
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
        assert_eq!(placements[0].host(), PlacementHost::CompatibilityProcess);

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
