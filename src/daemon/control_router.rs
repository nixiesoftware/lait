//! Local control routing and Station placement.
//!
//! A [`ControlRouter`] is the sole host-plane entrance from a catalog-wide
//! client to an Orbit. It resolves the Orbit through [`OrbitDirectory`], places
//! or reuses exactly one Station host for that Orbit, and dispatches the existing
//! control protocol. A vacant Orbit is hosted in-process; a compatible
//! pre-existing per-home daemon is attached as an external placement. Per-home
//! IPC remains an adapter so cwd-bound CLI and MCP clients keep the same route.

use std::collections::HashMap;
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use anyhow::{anyhow, Result};
use serde::Serialize;
use tokio::sync::{broadcast, Mutex, RwLock};

use crate::control::{self, ControlRoute, Doorbell, Request, RequestOwner, Response};
use crate::orbital::space_bridge::{SpaceBridgeRunner, SpaceBridgeStop};
use crate::transport::{DefaultFactory, TransportFactory};

use super::{LocalOrbitId, OrbitDirectory, ResolvedOrbit};

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
#[derive(Debug, Clone, Serialize)]
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

enum PlacementMode {
    Owned {
        stop: SpaceBridgeStop,
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
    ) -> Result<Self> {
        crate::cli::forget_verified(&resolved.home);
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
                match Self::start_owned(resolved, factory.as_ref()).await {
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
    ) -> Result<PlacementMode> {
        if !crate::orbital::space_store_present(&resolved.home) {
            return Err(anyhow!(
                "no space at {} — found one with `lait init`, or join one with `lait join <link>`",
                resolved.home.display()
            ));
        }
        let seed = crate::config::load_or_create_identity(&resolved.identity_dir)?;
        let runner = SpaceBridgeRunner::start(resolved.home.clone(), seed, factory).await?;
        let stop = runner.stop_handle();
        let mut completion = tokio::spawn(runner.run());

        if let Err(readiness_error) = wait_until_control_ready(resolved, &mut completion).await {
            stop.stop();
            return match tokio::time::timeout(Duration::from_secs(15), completion).await {
                Ok(Ok(Err(run_error))) => Err(run_error),
                Ok(Err(join_error)) => {
                    Err(anyhow!("in-process SpaceBridge task failed: {join_error}"))
                }
                _ => Err(readiness_error),
            };
        }

        Ok(PlacementMode::Owned {
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
        let route = Some(ControlRoute::Space {
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

        let PlacementMode::Owned { stop, completion } = &self.mode else {
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
            Ok(Err(error)) => Err(anyhow!("in-process SpaceBridge task failed: {error}")),
            Err(_) => Err(anyhow!(
                "in-process SpaceBridge did not finish dormancy within 10s; it remains draining"
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
    let route = ControlRoute::Space {
        address: resolved.address.clone(),
    };
    for _ in 0..100 {
        if completion.is_finished() {
            return Err(anyhow!(
                "in-process SpaceBridge exited before its control channel became ready"
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
        "in-process SpaceBridge did not open its control channel within 15s"
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
/// dormancy; SpaceBridge owns product adaptation; this router owns only the
/// decision to reuse or establish the host through which an Orbit is reached.
pub struct ControlRouter {
    directory: OrbitDirectory,
    occupancy: OrbitOccupancy<StationPlacement>,
    doorbells: broadcast::Sender<OrbitDoorbell>,
    factory: Arc<dyn TransportFactory>,
    lifecycle: RwLock<()>,
    shutting_down: AtomicBool,
}

impl ControlRouter {
    pub fn new(directory: OrbitDirectory) -> Self {
        Self::with_factory(directory, Arc::new(DefaultFactory))
    }

    pub fn with_factory(directory: OrbitDirectory, factory: Arc<dyn TransportFactory>) -> Self {
        // Doorbells are invalidations, not state. Lagging receivers rebaseline,
        // so a bounded fan-in is both sufficient and necessary.
        let (doorbells, _) = broadcast::channel(256);
        Self {
            directory,
            occupancy: OrbitOccupancy::default(),
            doorbells,
            factory,
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
        let orbit = resolved.address.orbit.clone();
        let doorbells = self.doorbells.clone();
        let factory = self.factory.clone();
        self.occupancy
            .get_or_try_place(orbit, StationPlacement::is_live, || {
                StationPlacement::establish(&resolved, doorbells, factory)
            })
            .await?;
        Ok(resolved)
    }

    /// Dispatch a request after ensuring its Orbit has one live placement.
    pub async fn request(&self, id: &str, request: &Request) -> Result<control::Response> {
        let resolved = self.place(id).await?;
        let route = match control::classify(request) {
            RequestOwner::World => ControlRoute::World {
                address: resolved.address,
                world: crate::world::contract::world_id().as_str().to_string(),
            },
            _ => ControlRoute::Space {
                address: resolved.address,
            },
        };
        control::request_routed(&resolved.home, request, route).await
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

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;

    use async_trait::async_trait;

    use super::*;
    use crate::net::Network;
    use crate::spaces::{Origin, SpaceEntry};
    use crate::transport::mem::MemNet;
    use crate::transport::{Alpn, Transport};

    static HOME_COUNTER: AtomicUsize = AtomicUsize::new(0);

    struct MemFactory(MemNet);

    #[async_trait]
    impl TransportFactory for MemFactory {
        async fn build(
            &self,
            identity_seed: &[u8; 32],
            _network: &Network,
            _alpns: &[Alpn],
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
            vec![SpaceEntry {
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

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn vacant_orbit_is_owned_in_process_and_shutdown_returns_it_to_vacancy() {
        let seed = [201; 32];
        let (home, directory, id) = formed_directory("owned", &seed);
        let router = ControlRouter::with_factory(directory, Arc::new(MemFactory(MemNet::new())));

        assert!(matches!(
            router.request(&id, &Request::Status).await.unwrap(),
            Response::Status(_)
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
        let runner = SpaceBridgeRunner::start(home.clone(), seed, &MemFactory(net.clone()))
            .await
            .unwrap();
        let stop = runner.stop_handle();
        let mut completion = tokio::spawn(runner.run());
        let resolved = directory.resolve(&id).unwrap();
        wait_until_control_ready(&resolved, &mut completion)
            .await
            .unwrap();

        let router = ControlRouter::with_factory(directory, Arc::new(MemFactory(net)));
        assert!(matches!(
            router.request(&id, &Request::Status).await.unwrap(),
            Response::Status(_)
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
