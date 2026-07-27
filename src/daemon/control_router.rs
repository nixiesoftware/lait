//! Local control routing and Station placement.
//!
//! A [`ControlRouter`] is the sole host-plane entrance from a catalog-wide
//! client to an Orbit. It resolves the Orbit through [`OrbitDirectory`], places
//! or reuses exactly one Station host for that Orbit, and dispatches the existing
//! control protocol. The compatibility host remains a process and the transport
//! remains per-home IPC; those are adapters behind the placement boundary.

use std::collections::HashMap;
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};

use anyhow::Result;
use serde::Serialize;
use tokio::sync::{broadcast, Mutex};

use crate::control::{self, ControlRoute, Doorbell, Request, RequestOwner};

use super::{LocalOrbitId, OrbitDirectory, ResolvedOrbit, StationIdentity};

/// A Station placement's current hosting strategy.
///
/// This enum makes process hosting an adapter choice rather than the definition
/// of a Station. An in-process or isolated plugin worker can be added without
/// changing Orbit addressing or the client protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlacementHost {
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
/// Today this observes a process-backed SpaceBridge over IPC. It deliberately
/// does not expose process details upward: the router needs only liveness and
/// the stable Orbit address.
pub struct StationPlacement {
    orbit: LocalOrbitId,
    host: PlacementHost,
    alive: Arc<AtomicBool>,
    doorbell_pump: tokio::task::JoinHandle<()>,
}

impl StationPlacement {
    pub fn orbit(&self) -> &LocalOrbitId {
        &self.orbit
    }

    pub fn host(&self) -> PlacementHost {
        self.host
    }

    fn is_live(&self) -> bool {
        self.alive.load(Ordering::Acquire) && !self.doorbell_pump.is_finished()
    }

    async fn process_backed(
        resolved: &ResolvedOrbit,
        doorbells: broadcast::Sender<OrbitDoorbell>,
    ) -> Result<Self> {
        let pin = match &resolved.identity {
            StationIdentity::Agent { .. } => Some(resolved.home.as_path()),
            StationIdentity::Own => None,
        };

        // A self-contained agent home is also its identity directory. Pinning
        // it prevents a catalog-wide human router from opening the agent's Orbit
        // under the human key; ordinary Orbits intentionally share Own.
        // The CLI memo is process-lifetime safe, while this router is
        // daemon-lifetime state and can outlive the process it observed.
        crate::cli::forget_verified(&resolved.home);
        crate::cli::ensure_daemon_as(&resolved.home, pin).await?;

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
            host: PlacementHost::CompatibilityProcess,
            alive,
            doorbell_pump,
        })
    }
}

impl Drop for StationPlacement {
    fn drop(&mut self) {
        self.doorbell_pump.abort();
    }
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
}

impl ControlRouter {
    pub fn new(directory: OrbitDirectory) -> Self {
        // Doorbells are invalidations, not state. Lagging receivers rebaseline,
        // so a bounded fan-in is both sufficient and necessary.
        let (doorbells, _) = broadcast::channel(256);
        Self {
            directory,
            occupancy: OrbitOccupancy::default(),
            doorbells,
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
        let resolved = self.resolve(id)?;
        let orbit = resolved.address.orbit.clone();
        let doorbells = self.doorbells.clone();
        self.occupancy
            .get_or_try_place(orbit, StationPlacement::is_live, || {
                StationPlacement::process_backed(&resolved, doorbells)
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
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;

    use super::*;

    fn orbit(name: &str) -> LocalOrbitId {
        LocalOrbitId::for_store(std::path::Path::new(name))
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
}
