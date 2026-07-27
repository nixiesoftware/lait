//! The Space bridge — the product/control entrance to one active Space.
//!
//! It composes [`OrbitalMechanics`] (authority/keys/membership over signed
//! material), a [`Runtime`] hosting the build's registered Worlds, and a
//! [`Station`] with the comms Contact plane. [`WorldBridgeRegistry`] owns one
//! bridge per hosted World. The current process adapter serves the **same** newline-delimited
//! `control::Request`/`Response` IPC the CLI/serve/MCP speak, so those clients
//! are unchanged. The current application requests route through
//! [`IssueRouter`]; peer exchange is Contact/Convergence over `comms`;
//! invitation is Coordinates v1; `Subscribe` streams the Station's
//! `ObservationStream` as `Doorbell` frames.
//!
//! Every control request has an explicit terminal owner (see
//! `tests/control_classification.rs`): product intents/queries route to the
//! World Session; membership, admission, device, key and the FROST
//! recovery/elevation/custody ceremonies are served by [`OrbitalMechanics`]
//! over the mechanics primitives; seeds, diagnose, inbox and log are node-local
//! lifecycle concerns. There is no catch-all refusal.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use interprocess::local_socket::{
    tokio::{prelude::*, Stream as LocalStream},
    ListenerOptions,
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use mechanics::ids::{SpaceId, StationId};
use replica::{AuthorityIncorporator, WorldId};
use runtime::{
    ActivationOptions, CommsOptions, ContactMechanics, ContactOptions, GossipOptions,
    LocalIdentity, Runtime, Session, Station,
};

use crate::config::{acquire_daemon_lock, load_or_create_identity, DaemonLock};
use crate::control::{
    control_name, CatalogScope, ControlRoute, DirtyProject, Doorbell, ProjectRef, Request,
    RequestOwner, Response, StatusInfo,
};
use crate::daemon::OrbitAddress;
use crate::ids::SystemUlidSource;
use crate::orbital::{
    orbital_store_root, unsupported_store_at, OrbitalMechanics, WorldBridgeRegistry,
};
use crate::transport::{Transport, TransportFactory};
use crate::world::{IssueRouter, RouterFacts};

/// Discover the single Space id under a home's orbital store root.
fn discover_space(home: &Path) -> Result<SpaceId> {
    let root = orbital_store_root(home);
    let mut found = None;
    for entry in std::fs::read_dir(&root)
        .with_context(|| format!("no orbital store at {} — run `lait init`", root.display()))?
        .flatten()
    {
        if let Some(name) = entry.file_name().to_str() {
            if name.starts_with("ws_") {
                if let Some(space) = SpaceId::parse(name) {
                    if found.replace(space).is_some() {
                        return Err(anyhow!("more than one Space under {}", root.display()));
                    }
                }
            }
        }
    }
    found.ok_or_else(|| anyhow!("no Space under {} — run `lait init`", root.display()))
}

/// The sole product-side entrance to one active Space.
///
/// This is a logical boundary, not an OS-process claim. The current
/// [`run_space_bridge`] adapter gives it a per-home control listener; a general
/// Lait daemon can instead hold several instances and route by Space id.
pub struct SpaceBridge {
    /// The durable local Orbit occupied by this bridge's Station.
    address: OrbitAddress,
    mechanics: OrbitalMechanics,
    station: Station,
    /// The canonical [`ApproachRoute`]s this Station advertises, resolved from
    /// the retained transport handle at activation (an Isolated endpoint's own
    /// bound direct addresses) — the composition root's route source, kept
    /// beside the Station (which never exposes its own transport). Invite
    /// creation signs exactly these into Coordinates.
    advertised_routes: Vec<runtime::coordinates::ApproachRoute>,
    /// A retained transport handle (the Station never exposes its own): lets
    /// the manual `connect` nudge teach routes from a pasted Coordinates link
    /// before dialing.
    transport: Arc<dyn Transport>,
    /// Every hosted World plus its lazily docked primary/agent Sessions.
    ///
    /// An un-admitted joiner still serves control and drives Contact before it
    /// can dock. Once standing lands, each World docks independently under the
    /// correct local identity.
    worlds: WorldBridgeRegistry,
    identity: LocalIdentity,
    device_seed: [u8; 32],
    home: PathBuf,
    /// Signalled by a `Stop` request so `serve` returns (the injectable
    /// contract: return, don't `exit`).
    shutdown: Arc<tokio::sync::Notify>,
    /// Latched when teardown begins (Stop or idle-shutdown). Subscription
    /// worker threads check it between bounded waits; the async side watches
    /// [`Self::stop_tx`] for the prompt wakeup.
    stopping: std::sync::atomic::AtomicBool,
    /// The teardown broadcast: `true` once Stop/idle-shutdown latched. Every
    /// live `Subscribe` connection selects on this, so teardown is prompt and
    /// bounded instead of waiting out a poll interval per subscriber.
    stop_tx: tokio::sync::watch::Sender<bool>,
    /// The previous ring's per-plane digests — the baseline a catalog change is
    /// diffed against to recover *which* plane moved.
    ///
    /// This is a diff baseline, not a cache: nothing reads through it, and it is
    /// only ever compared against state read fresh at the current root. It is
    /// advanced exactly once per Observation, which is why the fan-out below
    /// exists — two subscribers each advancing it would leave the second seeing
    /// no change at all.
    ring_planes: Mutex<Option<std::collections::BTreeMap<CatalogScope, String>>>,
    /// The one enriched-doorbell fan-out.
    ///
    /// Subscribers read this rather than each opening their own Observation
    /// stream. Translating Body scopes into a dirty-set is per-*Observation*
    /// work, not per-subscriber work — and once that translation holds a diff
    /// baseline it MUST be, because two subscribers each advancing the same
    /// baseline would leave the second one seeing no change at all.
    doorbells: tokio::sync::broadcast::Sender<Doorbell>,
    /// The live task feeding [`Self::doorbells`], when one exists.
    ///
    /// The mutex is deliberately **held across startup**. A second subscriber
    /// waits until the winner has opened the Observation stream, so "ready" is
    /// the only state it can observe. Retaining the JoinHandle also lets
    /// shutdown signal and join this owner before Station dormancy.
    pump: tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// Control connections currently being served (idle-shutdown suppressor).
    active_conns: std::sync::atomic::AtomicU64,
    /// When the last control connection was accepted or completed — the idle
    /// clock's reference point.
    last_activity: Mutex<std::time::Instant>,
}

/// An owned in-process SpaceBridge lifecycle.
///
/// The runner holds the per-home daemon lock from before activation until after
/// the Station has gone dormant. It is joinable by its host; dropping or
/// aborting the task is not a successful shutdown path.
pub(crate) struct SpaceBridgeRunner {
    home: PathBuf,
    bridge: Arc<SpaceBridge>,
    _lock: DaemonLock,
}

/// A non-owning signal for an in-process SpaceBridge runner.
///
/// Weak by design: retaining a stop handle must not keep the bridge alive while
/// the runner waits for every control/session owner to drain.
#[derive(Clone)]
pub(crate) struct SpaceBridgeStop {
    bridge: std::sync::Weak<SpaceBridge>,
}

impl SpaceBridgeStop {
    pub(crate) fn stop(&self) {
        if let Some(bridge) = self.bridge.upgrade() {
            bridge.begin_stop();
        }
    }
}

impl SpaceBridgeRunner {
    /// Acquire the Orbit's process-wide lease and activate its Station.
    pub(crate) async fn start(
        home: PathBuf,
        device_seed: [u8; 32],
        factory: &dyn TransportFactory,
    ) -> Result<Self> {
        let lock = acquire_daemon_lock(&home)?;
        let bridge = Arc::new(SpaceBridge::open(&home, device_seed, factory).await?);
        Ok(Self {
            home,
            bridge,
            _lock: lock,
        })
    }

    pub(crate) fn stop_handle(&self) -> SpaceBridgeStop {
        SpaceBridgeStop {
            bridge: Arc::downgrade(&self.bridge),
        }
    }

    /// Serve until stopped, drain every bridge owner, return the Station to its
    /// Orbit, and only then release the per-home lease.
    pub(crate) async fn run(self) -> Result<()> {
        let serve_result = self.bridge.clone().serve().await;
        self.bridge.begin_stop();

        tokio::time::timeout(Duration::from_secs(5), async {
            while Arc::strong_count(&self.bridge) != 1 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .map_err(|_| {
            anyhow!(
                "SpaceBridge clients did not drain during shutdown ({} owners remain)",
                Arc::strong_count(&self.bridge).saturating_sub(1)
            )
        })?;

        let Self {
            home,
            bridge,
            _lock,
        } = self;
        let bridge = Arc::try_unwrap(bridge)
            .map_err(|_| anyhow!("SpaceBridge still shared after client drain"))?;
        let dormancy_result = bridge.go_dormant();

        serve_result?;
        dormancy_result?;
        #[cfg(unix)]
        let _ = std::fs::remove_file(crate::config::socket_path(&home));
        #[cfg(not(unix))]
        let _ = home;
        drop(_lock);
        Ok(())
    }
}

impl SpaceBridge {
    /// Open and activate the orbital stack for a home, then dock the routing
    /// Session. Refuses a pre-orbital home.
    pub async fn open(
        home: &Path,
        device_seed: [u8; 32],
        factory: &dyn TransportFactory,
    ) -> Result<Self> {
        if let Some(err) = unsupported_store_at(home) {
            return Err(anyhow!("{err}"));
        }
        let space = discover_space(home)?;
        let mechanics = OrbitalMechanics::open(&orbital_store_root(home), &space, &device_seed)?;

        let (registry, worlds) = crate::orbital::issues_worlds()
            .build()
            .map_err(|e| anyhow!("world registry: {e:?}"))?;
        let rt = Runtime::open(
            orbital_store_root(home),
            registry,
            Arc::new(mechanics.clone()),
            Arc::new(mechanics.clone()),
        );

        let network = crate::net::Network::from_env()?;
        let transport = factory
            .build(
                &device_seed,
                &network,
                &[runtime::contact::CONTACT_ALPN, runtime::PRESENCE_ALPN],
            )
            .await?;
        // Retain a transport clone for invite route advertisement (and the
        // manual `connect` nudge) before the Station consumes one into its
        // Comms.
        let retained_transport = transport.clone();
        // Resolve the routes this Station will advertise — in invites AND in
        // its Beacons: the transport's currently-dialable direct addresses
        // (bounded wait for a fresh iroh endpoint), canonicalized. A
        // relay/discovery transport returns none — its invites are
        // address-free (bare ids resolve).
        let advertised_addrs = retained_transport
            .advertised_routes(Duration::from_secs(3))
            .await
            .unwrap_or_default();
        let advertised_routes = runtime::coordinates::canonical_routes(&advertised_addrs);
        // W0-S1: the gossip bootstrap union — pinned seeds, the verified
        // invite ticket's approach Station, and persisted Neighbor registry
        // entries holding an unexpired route lease. Identities only; the
        // eclipse fence governs everything learned after this.
        let my_id = retained_transport.my_id();
        let mut bootstrap: Vec<crate::ids::DeviceId> =
            load_seeds(home).into_iter().map(|s| s.id).collect();
        // The ticket's approach Station: teach the transport its signed direct
        // routes so the first dial resolves (Coordinates-only, no shared
        // registry), and bootstrap the swarm from it.
        if let Some(coords) = mechanics.pending_coordinates() {
            if let Ok(verified) = coords.verify() {
                if !verified.approach_routes.is_empty() {
                    // PeerId is a DeviceId — the approach Station's key is
                    // its dialable peer id.
                    retained_transport
                        .learn(verified.approach_station.clone(), &verified.approach_routes);
                }
                bootstrap.push(verified.approach_station.clone());
            }
        }
        // Persisted Neighbors with live route leases (S1(c)): dead-hub
        // recovery — surviving peers keep finding each other without the
        // approach Station.
        if let Ok(registry) =
            runtime::NeighborRegistry::load(&orbital_store_root(home).join(space.as_str()), &space)
        {
            for (station, routes) in registry.bootstrap_candidates(now_secs() * 1_000) {
                let device = station.as_device();
                let addrs: Vec<std::net::SocketAddr> = routes
                    .iter()
                    .filter(|h| h.scheme == 1)
                    .filter_map(|h| {
                        std::str::from_utf8(&h.bytes)
                            .ok()
                            .and_then(|t| t.parse().ok())
                    })
                    .collect();
                if !addrs.is_empty() {
                    retained_transport.learn(device.clone(), &addrs);
                }
                bootstrap.push(device);
            }
        }
        bootstrap.sort();
        bootstrap.dedup();
        bootstrap.retain(|p| p != &my_id);
        // The Beacon advertisement (scheme 1: UTF-8 socket address) — the same
        // routes invites carry, in route-hint form, canonically sorted.
        let mut advertise: Vec<runtime::beacon::RouteHint> = advertised_routes
            .iter()
            .map(|r| runtime::beacon::RouteHint {
                scheme: 1,
                bytes: r.to_socket().to_string().into_bytes(),
            })
            .collect();
        advertise.sort();
        advertise.dedup();
        advertise.truncate(runtime::beacon::MAX_ROUTE_HINTS);
        let station = rt
            .orbit(&space)
            .map_err(|e| anyhow!("acquire orbit: {e:?}"))?
            .activate(ActivationOptions {
                drain_deadline: Duration::from_secs(5),
                comms: Some(comms_options(
                    transport,
                    device_seed,
                    &mechanics,
                    bootstrap,
                    advertise,
                )),
                observation_capacity: 0,
            })
            .map_err(|e| anyhow!("activate: {e:?}"))?;
        let identity = Runtime::identity_from_seed(&device_seed);
        // Dock now if we already hold standing (founder / re-opened member);
        // otherwise defer until admission lands (an un-admitted joiner cannot
        // dock, but must still serve control to drive its own Contact).
        let _ = worlds.ensure_primary(&station, &crate::world::contract::world_id(), &identity);

        // The implementation self-check. Receipts pin whichever implementation
        // id is ACTIVE in the ledger — not this build's — so a build whose
        // descriptor has moved on would silently attest an implementation it
        // is not. Say so at open; `lait world-upgrade` (admin) activates this
        // build's id.
        {
            use runtime::AuthorityView;
            let device = crate::crypto::device_from_seed(&device_seed);
            if let Some(principal) = mechanics.resolve(&device) {
                for (world, ours) in worlds.reviewed_implementations() {
                    let active =
                        mechanics.active_implementation(world, &principal.authority_frontier);
                    if active != Some(*ours) {
                        tracing::warn!(
                            "this build's {} World implementation ({}) is not the space's \
                             active one ({}) — writes will attest the active implementation; \
                             an admin should activate this build's reviewed implementation",
                            world,
                            data_encoding::HEXLOWER.encode(&ours[..8]),
                            active
                                .map(|a| data_encoding::HEXLOWER.encode(&a[..8]))
                                .unwrap_or_else(|| "none".into()),
                        );
                    }
                }
            }
        }

        Ok(Self {
            address: OrbitAddress::for_store(home, space),
            mechanics,
            station,
            advertised_routes,
            transport: retained_transport,
            worlds,
            identity,
            device_seed,
            home: home.to_path_buf(),
            shutdown: Arc::new(tokio::sync::Notify::new()),
            stopping: std::sync::atomic::AtomicBool::new(false),
            stop_tx: tokio::sync::watch::channel(false).0,
            ring_planes: Mutex::new(None),
            doorbells: tokio::sync::broadcast::channel(256).0,
            pump: tokio::sync::Mutex::new(None),
            active_conns: std::sync::atomic::AtomicU64::new(0),
            last_activity: Mutex::new(std::time::Instant::now()),
        })
    }

    /// Ensure the primary identity has a Session for `world`, docking lazily
    /// once standing is held.
    fn ensure_world_session(&self, world: &WorldId) -> bool {
        self.worlds
            .ensure_primary(&self.station, world, &self.identity)
            .is_ok()
    }

    /// Route an issue-family request through the docked Session, or refuse with a
    /// typed "not admitted yet" when this device holds no standing.
    fn route_issue(&self, req: Request, act_as: Option<&str>) -> Response {
        let world = crate::world::contract::world_id();
        // Resolve which local identity signs this request: the daemon's primary
        // (human) when no selector, else a sponsored local agent provisioned
        // under this home. One store, N signing identities (Architecture B).
        let seed = match self.acting_seed(act_as) {
            Ok(s) => s,
            Err(resp) => return resp,
        };
        let device = crate::crypto::device_from_seed(&seed);

        // Hard partial-view guard (docs/plans/09 §10 finding 3): a *delegated*
        // identity — a sponsored agent — must not AUTHOR against a view it knows
        // is incomplete. "Close what's done" against a missing-epoch partial
        // view could act on issues it cannot see. A human acting for themselves
        // gets the loud `whoami`/`sync` signal and judges; an agent is stopped
        // by construction. Reads are always allowed (that is how it re-syncs).
        if !crate::serve::policy::is_read(&req) {
            use runtime::AuthorityView;
            if let Some(actor) = self.mechanics.resolve(&device).map(|r| r.actor) {
                if self.mechanics.is_agent(&actor) {
                    let divergence = self.mechanics.view_divergence();
                    if !divergence.is_empty() {
                        return Response::denied(format!(
                            "refusing to author against a partial view — {}. Run `sync` \
                             until whole first; a delegated agent must not act on issues \
                             it cannot see. Nothing was changed.",
                            divergence.join("; ")
                        ));
                    }
                }
            }
        }

        // The primary (human) uses the World-keyed primary Session; a sponsored
        // agent docks lazily under the same World, sharing the one Replica.
        if act_as.is_none() {
            if !self.ensure_world_session(&world) {
                return Response::err(
                    "not admitted to this space yet — run `lait connect` to reach an \
                     admin and complete admission before filing issues",
                );
            }
            self.worlds
                .with_primary(&world, |session| {
                    self.route_with(session, &self.identity, &seed, req)
                })
                .expect("World Session present after ensure")
        } else {
            let identity = Runtime::identity_from_seed(&seed);
            match self
                .worlds
                .with_agent(&self.station, &world, &identity, |session| {
                    self.route_with(session, &identity, &seed, req)
                }) {
                Ok(response) => response,
                Err(_) => Response::denied(
                    "this agent identity holds no standing in the space yet — a \
                     human member must sponsor it (`lait members agent <key>`) \
                     before it can author. Nothing was changed.",
                ),
            }
        }
    }

    /// Route one issue request through a docked Session for a specific identity,
    /// absorbing the transient "membership changed — retry" a background Contact
    /// can cause between a submit's resolve and its commit.
    fn route_with(
        &self,
        session: &Session,
        identity: &LocalIdentity,
        seed: &[u8; 32],
        req: Request,
    ) -> Response {
        let router = IssueRouter::new(session, identity, CLOCK.get_or_init(|| SystemUlidSource));
        let facts = self.facts_for(seed);
        let mut resp = router.route(req.clone(), &facts).0;
        for _ in 0..3 {
            match &resp {
                Response::Error { message, .. } if message == "membership changed — retry" => {
                    std::thread::sleep(std::time::Duration::from_millis(15));
                    resp = router.route(req.clone(), &facts).0;
                }
                _ => break,
            }
        }
        resp
    }

    /// Resolve the acting identity's seed. `None` → the daemon's primary (human)
    /// identity. `Some(name)` → a local agent identity provisioned under this
    /// home; a missing one is a typed denial the agent surface can act on.
    fn acting_seed(&self, act_as: Option<&str>) -> Result<[u8; 32], Response> {
        match act_as {
            None => Ok(self.device_seed),
            Some(name) => load_agent_seed(&self.home, name).map_err(|e| {
                Response::denied(format!(
                    "no local agent identity '{name}' on this node — provision one with \
                     `lait members agent --new {name}` (it self-incepts + is sponsored in \
                     one step), then act as it: {e}"
                ))
            }),
        }
    }

    fn facts(&self) -> RouterFacts {
        self.facts_for(&self.device_seed)
    }

    /// Router facts (actor/device) for a specific identity seed.
    fn facts_for(&self, seed: &[u8; 32]) -> RouterFacts {
        use runtime::AuthorityView;
        let device = crate::crypto::device_from_seed(seed);
        let actor = self
            .mechanics
            .resolve(&device)
            .map(|r| r.actor.as_str().to_string())
            .unwrap_or_default();
        RouterFacts {
            device: device.as_str().to_string(),
            actor,
            project_hint: std::env::var("LAIT_PROJECT_HINT").ok(),
            default_project: None,
            now: now_secs(),
        }
    }

    /// Route one control request to its terminal owner — the value the
    /// PRODUCTION classifier returns. Tests and the generated routing table
    /// consume the same `control::classify`; there is no second table and no
    /// wildcard terminal owner.
    fn dispatch(
        &self,
        route: Option<&ControlRoute>,
        req: Request,
        act_as: Option<&str>,
    ) -> Response {
        let owner = crate::control::classify(&req);
        if let Err(response) = self.validate_route(route, owner) {
            return response;
        }
        match owner {
            // The acting-identity selector matters where the answer is
            // identity-relative: issue authoring (who signs) and whoami (who am
            // I). Membership/station/lifecycle ops stay the daemon's — an agent
            // holds no membership authority, so routing them "as the agent"
            // would only ever be denied.
            RequestOwner::World => self.route_issue(req, act_as),
            // Authority never passes through the Session, so a local membership,
            // role, device or key change publishes nothing on its own — the
            // remote half of this plane is published by the Contact driver.
            // Compared by frontier rather than matched per request: a new
            // mechanics verb inherits the ring instead of quietly not having one.
            RequestOwner::Mechanics => {
                let before = self.mechanics.current_frontier();
                let response = self.dispatch_mechanics(req, act_as);
                if self.mechanics.current_frontier() != before {
                    self.publish_authority_advanced();
                }
                response
            }
            RequestOwner::Station => self.dispatch_station(req),
            RequestOwner::Observation => self.dispatch_observation(req),
            RequestOwner::Lifecycle => self.dispatch_lifecycle(req),
        }
    }

    /// Validate the explicit broker path before any terminal owner sees the
    /// request. A missing route is the legacy per-home protocol and remains
    /// accepted while clients migrate to the general Lait daemon.
    fn validate_route(
        &self,
        route: Option<&ControlRoute>,
        owner: RequestOwner,
    ) -> std::result::Result<(), Response> {
        let Some(route) = route else {
            return Ok(());
        };
        let actual_space = self.station.space_id();
        let wrong_space = |space: &mechanics::ids::SpaceId| {
            Response::err(format!(
                "request targets Space {space}, but this bridge owns {actual_space}"
            ))
        };
        let wrong_orbit = |address: &OrbitAddress| {
            Response::err(format!(
                "request targets local Orbit {}, but this bridge occupies {}",
                address.orbit, self.address.orbit
            ))
        };
        match route {
            ControlRoute::Daemon => {
                Err(Response::err("daemon-scoped request reached a SpaceBridge"))
            }
            ControlRoute::Space { address } => {
                if address.orbit != self.address.orbit {
                    return Err(wrong_orbit(address));
                }
                if &address.space != actual_space {
                    return Err(wrong_space(&address.space));
                }
                if owner == RequestOwner::World {
                    return Err(Response::err("World-owned request requires a world route"));
                }
                Ok(())
            }
            ControlRoute::World { address, world } => {
                if address.orbit != self.address.orbit {
                    return Err(wrong_orbit(address));
                }
                if &address.space != actual_space {
                    return Err(wrong_space(&address.space));
                }
                if owner != RequestOwner::World {
                    return Err(Response::err(
                        "Space-owned request cannot be sent through a WorldBridge",
                    ));
                }
                let Some(world_id) = WorldId::parse(world) else {
                    return Err(Response::err(format!("invalid World id '{world}'")));
                };
                if !self.worlds.contains(&world_id) {
                    return Err(Response::err(format!(
                        "World '{world}' is not enabled in Space {actual_space}"
                    )));
                }
                // The public Request enum is still the issue tracker's adapter.
                // A second application will add its own typed World call; until
                // then, do not let an issue verb borrow another World's Session.
                if world_id != crate::world::contract::world_id() {
                    return Err(Response::err(format!(
                        "request belongs to {}, not World '{world}'",
                        crate::world::contract::world_id()
                    )));
                }
                Ok(())
            }
        }
    }

    /// Ring the authority plane, if there is a docked Session to ring through.
    /// Before admission there is no Session and nothing is subscribed anyway.
    fn publish_authority_advanced(&self) {
        let world = crate::world::contract::world_id();
        if !self.ensure_world_session(&world) {
            return;
        }
        self.worlds.with_any_primary(|session| {
            session.publish_authority_advanced();
        });
    }

    /// Membership, admission, device, key, ceremony and custody requests —
    /// served by [`OrbitalMechanics`] over the mechanics primitives.
    fn dispatch_mechanics(&self, req: Request, act_as: Option<&str>) -> Response {
        match req {
            Request::Members => self.members(),
            Request::MemberAdd { who, admin, .. } => match self.mechanics.member_add(&who, admin) {
                Ok(()) => Response::Ok {
                    message: Some(format!("added {who}")),
                },
                Err(e) => Response::err(format!("{e}")),
            },
            Request::MemberRemove { who } => match self.mechanics.member_remove(&who) {
                Ok(()) => Response::Ok {
                    message: Some(format!("removed {who}")),
                },
                Err(e) => Response::err(format!("{e}")),
            },
            Request::MemberSetRole { who, admin } => {
                match self.mechanics.member_set_role(&who, admin) {
                    Ok(actor) => Response::Ok {
                        message: Some(if admin {
                            format!("promoted {} to admin", actor.short())
                        } else {
                            format!("{} is now a plain member", actor.short())
                        }),
                    },
                    Err(e) => Response::err(format!("{e}")),
                }
            }
            Request::MemberLog => Response::MemberLog {
                entries: self.mechanics.member_log(),
            },
            Request::DeviceInvite => match self.mechanics.device_invite() {
                Ok((actor, space)) => Response::Text {
                    text: format!("{actor} {space}"),
                },
                Err(e) => Response::err(format!("{e}")),
            },
            Request::DeviceAdd { consent } => self.device_add(&consent),
            Request::DeviceRevoke { device } => match self.mechanics.device_revoke(&device) {
                Ok(true) => Response::Ok {
                    message: Some(format!("revoked device {device} and rotated the key")),
                },
                Ok(false) => Response::Ok {
                    message: Some(format!(
                        "revoked device {device} from your identity — ask an admin to \
                         rotate the space key to fence its access to existing content"
                    )),
                },
                Err(e) => Response::err(format!("{e}")),
            },
            Request::DeviceList => Response::Text {
                text: self.device_list_text(),
            },
            Request::Recover => match self.mechanics.recover() {
                Ok(actor) => Response::Ok {
                    message: Some(format!(
                        "recovered actor {} — device set reset to this device; content \
                         access re-seals once a peer syncs",
                        actor.short()
                    )),
                },
                Err(e) => Response::err(format!("{e}")),
            },
            Request::KeyRotate => match self.mechanics.key_rotate() {
                Ok(gen) => Response::Ok {
                    message: Some(format!("rotated the space key to generation {gen}")),
                },
                Err(e) => Response::err(format!("{e}")),
            },
            Request::InviteRevoke { invite } => match self.mechanics.invite_revoke(&invite) {
                Ok(already_spent) => Response::Ok {
                    message: Some(if already_spent {
                        "revoked the invite — note it had already admitted at least one member; \
                         revocation stops further admissions but does not remove them"
                            .to_string()
                    } else {
                        "revoked the invite — it can no longer admit anyone".to_string()
                    }),
                },
                Err(e) => Response::err(format!("{e}")),
            },
            Request::AgentAdd { key } => match self.mechanics.agent_add(&key) {
                Ok(actor) => Response::Ok {
                    message: Some(format!("sponsored agent {}", actor.short())),
                },
                Err(e) => Response::err(format!("{e}")),
            },
            Request::AgentProvision { name } => self.agent_provision(&name),
            Request::WorldUpgrade => {
                let ours = crate::orbital::issues_implementation_id();
                match self
                    .mechanics
                    .activate_implementation(crate::world::contract::PRODUCT_WORLD, ours)
                {
                    Ok(()) => Response::Ok {
                        message: Some(format!(
                            "implementation {} is active for {} (no-op if it already was)",
                            data_encoding::HEXLOWER.encode(&ours[..8]),
                            crate::world::contract::PRODUCT_WORLD,
                        )),
                    },
                    Err(e) => Response::err(format!("{e}")),
                }
            }
            Request::Id => {
                // First line: the device id (the stable, parseable form).
                // Second line, when the actor plane resolves this device (a
                // pending joiner's inception counts): the actor id — the
                // handle admission and role verbs take (GOV-11).
                let device = crate::crypto::device_from_seed(&self.device_seed).to_string();
                Response::Ok {
                    message: Some(match self.mechanics.my_actor() {
                        Some(actor) => format!("{device}\nactor {}", actor.as_str()),
                        None => device,
                    }),
                }
            }
            Request::Whoami => self.whoami(act_as),
            Request::Invite {
                role,
                reusable,
                ttl_hours,
            } => self.invite(role.as_deref(), reusable, ttl_hours),
            Request::Join { ticket } => self.connect(&ticket),
            Request::SpaceRecover => self.space_recover(),
            Request::SpaceRecoverApprove { session, expect } => {
                self.space_recover_approve(session, expect)
            }
            Request::SpaceElevate { cofounders, k } => self.space_elevate(cofounders, k),
            Request::SpaceElevateApprove { session, proposal } => {
                self.space_elevate_approve(session, proposal)
            }
            Request::SpaceReshare { participants, k } => self.space_reshare(participants, k),
            Request::SpaceCustodyExport { path, passphrase } => {
                self.space_custody_export(path, passphrase)
            }
            Request::SpaceCustodyImport {
                path,
                passphrase,
                force,
            } => self.space_custody_import(path, passphrase, force),
            Request::AccessList { actor } => {
                let subject = match actor.as_deref() {
                    None => None,
                    Some(who) => match self.mechanics.resolve_actor_ref(who) {
                        Some(a) => Some(a),
                        None => return Response::not_found(format!("no actor matches '{who}'")),
                    },
                };
                Response::Assignments {
                    rows: self.mechanics.assignment_rows(subject.as_ref()),
                }
            }
            Request::AccessGrant {
                actor,
                role,
                project,
            } => self.access_grant(&actor, &role, project.as_deref()),
            Request::AccessRevoke { grant_id } => {
                let raw = match data_encoding::HEXLOWER_PERMISSIVE
                    .decode(grant_id.trim().as_bytes())
                    .ok()
                    .and_then(|b| <[u8; 32]>::try_from(b.as_slice()).ok())
                {
                    Some(id) => id,
                    None => return Response::err("expected a 64-hex grant id"),
                };
                match self.mechanics.revoke_assignment(raw) {
                    Ok(()) => Response::Ok {
                        message: Some("revoked the assignment".into()),
                    },
                    Err(e) => Response::err(format!("{e}")),
                }
            }
            // The production classifier routed this here; any other variant
            // reaching this arm is a routing invariant violation, not a
            // servable request.
            other => unreachable!("misclassified mechanics request: {other:?}"),
        }
    }

    /// Connect/neighbor/Contact requests — served by the Station.
    fn dispatch_station(&self, req: Request) -> Response {
        match req {
            Request::Connect { ticket } => self.connect(&ticket),
            Request::Who => Response::Who { peers: self.who() },
            Request::Sync => self.sync(),
            other => unreachable!("misclassified station request: {other:?}"),
        }
    }

    /// Converge the keyring against the authority ledger (adopting any
    /// just-arrived sealed epoch envelopes) and report completeness **loudly**.
    /// The ambient Contact/Beacon plane exchanges peer material continuously;
    /// `sync` names the state so a missing epoch key is never inferred from a
    /// short issue count (`docs/plans/09` §3.4). It supersedes `connect
    /// <device-id>` as the "am I caught up?" verb.
    fn sync(&self) -> Response {
        let divergence = self.mechanics.view_divergence();
        let whole = divergence.is_empty();
        let message = if whole {
            "converged — this view is complete (every authorized epoch key is held)".to_string()
        } else {
            format!(
                "view is PARTIAL — {} authorized epoch key(s) not yet sealed to this \
                 device; content under them is invisible until they sync",
                divergence.len()
            )
        };
        Response::Sync {
            whole,
            divergence,
            message,
        }
    }

    /// The one-shot identity + standing + view-completeness projection
    /// (`lait whoami`, the MCP `whoami` tool). Every fact resolved once: actor,
    /// device, `did:key`, space, role, capabilities, sponsor, and the loud
    /// partial-view signal — so "who am I / what may I do / is my view complete"
    /// is a glance, never a deduction (`docs/plans/09` §3.4).
    fn whoami(&self, act_as: Option<&str>) -> Response {
        let seed = match self.acting_seed(act_as) {
            Ok(s) => s,
            Err(resp) => return resp,
        };
        let device = crate::crypto::device_from_seed(&seed);
        let did = crate::crypto::did_key_from_device(&device);
        // The acting device's actor in the plane (independent of standing, so a
        // provisioned-but-unsponsored agent still sees "who it is / not a member
        // yet"). For the primary this is `my_actor`.
        let actor = self.mechanics.actor_of_device(&device);
        let space = Some(self.mechanics.space().as_str().to_string());
        let name = match act_as {
            None => Some(crate::config::Settings::load(Some(&self.home)).nick()),
            Some(n) => Some(n.to_string()),
        };
        let divergence = self.mechanics.view_divergence();
        let partial_view = !divergence.is_empty();
        let (actor_str, role, member, can_write, capabilities, policy_admin, sponsor) = match &actor
        {
            Some(a) => {
                let (capabilities, policy_admin) = self.mechanics.effective_capabilities(a);
                (
                    Some(a.as_str().to_string()),
                    self.mechanics.role_of(a),
                    self.mechanics.is_member(a),
                    self.mechanics.can_write(a),
                    capabilities,
                    policy_admin,
                    self.mechanics.sponsor_of(a).map(|s| s.as_str().to_string()),
                )
            }
            None => (None, "none".to_string(), false, false, vec![], false, None),
        };
        Response::Whoami(crate::dto::WhoamiDto {
            actor: actor_str,
            device: device.as_str().to_string(),
            did,
            space,
            role,
            member,
            can_write,
            capabilities,
            policy_admin,
            sponsor,
            name,
            partial_view,
            divergence,
        })
    }

    /// Provision a co-located agent identity by name (the seamless "sponsor
    /// once" flow): mint/reuse its seed under this home, self-incept it into the
    /// shared store, and sponsor it with content authority — all local, one
    /// step. Afterwards a client acts as it via `--as <name>` / MCP.
    fn agent_provision(&self, name: &str) -> Response {
        // The name becomes a directory under the home; keep it a plain segment.
        if name.is_empty()
            || name.contains(['/', '\\'])
            || name.contains("..")
            || name.contains(':')
        {
            return Response::err(
                "an agent name must be a plain identifier (no path separators or '..')",
            );
        }
        // Only a human member may sponsor; surface it before minting a seed.
        if !self.mechanics.am_i_member() {
            return Response::denied(
                "you are not yet a member of this space, so you cannot sponsor an agent — \
                 complete your own admission first",
            );
        }
        let seed = match load_or_create_agent_seed(&self.home, name) {
            Ok(s) => s,
            Err(e) => return Response::err(format!("provision agent '{name}': {e:#}")),
        };
        match self.mechanics.provision_agent(&seed) {
            Ok(actor) => {
                let device = crate::crypto::device_from_seed(&seed);
                let did = crate::crypto::did_key_from_device(&device).unwrap_or_default();
                Response::Ok {
                    message: Some(format!(
                        "provisioned + sponsored agent '{name}'\nactor {}\n{did}\n\
                         it holds write access; act as it with `--as {name}` (or point an \
                         MCP client at this home with LAIT_AGENT={name})",
                        actor.as_str()
                    )),
                }
            }
            Err(e) => Response::err(format!("sponsor agent '{name}': {e:#}")),
        }
    }

    /// The reconciled presence assembly: the persistent Neighbor registry's
    /// advisory reachability (fed by verified Beacons, swarm membership
    /// events, and Contact outcomes) projected into presence rows. The same
    /// truth `status.online_peers` counts — the two surfaces cannot disagree.
    fn who(&self) -> Vec<crate::control::PresenceEntry> {
        let aliases = read_aliases(&self.home);
        let now = now_secs();
        self.station
            .neighbors()
            .into_iter()
            .map(|n| {
                let id = n.station.as_device().to_string();
                let online = n.reachability == runtime::Reachability::Reachable;
                let state = match n.reachability {
                    runtime::Reachability::Reachable => "online",
                    runtime::Reachability::Unreachable => "offline",
                    runtime::Reachability::Unknown => "away",
                };
                let last_seen_secs = if n.last_seen_ms == 0 {
                    0
                } else {
                    now.saturating_sub(n.last_seen_ms / 1_000)
                };
                crate::control::PresenceEntry {
                    nick: aliases.get(&id).cloned().unwrap_or_default(),
                    id,
                    state: state.to_string(),
                    online,
                    last_seen_secs,
                }
            })
            .collect()
    }

    /// The one number both `status` and `who` report as "online".
    fn online_peers(&self) -> usize {
        self.station
            .neighbors()
            .iter()
            .filter(|n| n.reachability == runtime::Reachability::Reachable)
            .count()
    }

    /// Status, subscription, and locally derived projection surfaces.
    fn dispatch_observation(&self, req: Request) -> Response {
        match req {
            Request::Status => self.status(),
            Request::Inbox { clear } => {
                let (entries, unread) = self.inbox_projection();
                if clear {
                    self.write_inbox_watermark(now_secs());
                }
                Response::Inbox { entries, unread }
            }
            // Subscribe is handled by the streaming connection path before
            // dispatch; a one-shot Subscribe cannot be answered on this plane.
            Request::Subscribe { .. } => Response::err("subscribe is a streaming request"),
            other => unreachable!("misclassified observation request: {other:?}"),
        }
    }

    /// Daemon lifecycle and node-local configuration adapters.
    fn dispatch_lifecycle(&self, req: Request) -> Response {
        match req {
            Request::Hello { .. } => Response::Hello {
                protocol_version: crate::control::CONTROL_PROTOCOL_VERSION,
            },
            Request::ConfigReload => Response::Ok { message: None },
            Request::Stop => Response::Ok {
                message: Some("stopping".into()),
            },
            // The SpaceBridge has no legacy in-memory event ring — live
            // clients observe the Station's doorbell stream (`Subscribe`)
            // instead — so the polling log is empty by construction.
            Request::Log { since } => Response::Events {
                events: vec![],
                last: since,
            },
            Request::Diagnose { expected_space } => self.diagnose(expected_space),
            Request::SeedAdd { arg } => self.seed_add(arg.trim()),
            Request::SeedList => self.seed_list(),
            Request::SeedRemove { who } => self.seed_remove(who.trim()),
            Request::MemberAlias { who, name } => self.set_alias(&who, &name),
            other => unreachable!("misclassified lifecycle request: {other:?}"),
        }
    }

    /// The (issues, projects) counts from the docked Session's catalog
    /// snapshot — `None` when the projection is UNAVAILABLE (undocked, or a
    /// query failed). Status reports the truth; it never converts an
    /// unavailable projection into false zeros.
    fn counts(&self) -> Option<(usize, usize, String, String)> {
        use crate::world::contract::{self, IssueQuery};
        let world = contract::world_id();
        if !self.ensure_world_session(&world) {
            return None;
        }
        self.worlds
            .with_primary(&world, |session| {
                let query = |q: IssueQuery| -> Option<serde_json::Value> {
                    let bytes = session
                        .query(runtime::WorldQuery {
                            schema: contract::issue_schema(),
                            schema_version: contract::ISSUE_SCHEMA_VERSION,
                            payload: q.to_json(),
                        })
                        .ok()?
                        .bytes;
                    serde_json::from_slice(&bytes).ok()
                };
                let snapshot = query(IssueQuery::Snapshot)?;
                let catalog = snapshot.get("catalog")?;
                let projects = catalog.get("projects")?.as_object().map(|m| m.len())?;
                // The catalog `name` register is the space's mutable display
                // label (`SpaceRename` writes it); surface it so the rename is
                // visible.
                let name = catalog
                    .get("name")
                    .and_then(|n| n.as_str())
                    .unwrap_or("")
                    .to_string();
                let description = catalog
                    .get("description")
                    .and_then(|n| n.as_str())
                    .unwrap_or("")
                    .to_string();
                let issues = query(IssueQuery::List {
                    project: None,
                    label: None,
                    status: None,
                    mine: None,
                    all: true,
                    me: None,
                })
                .and_then(|v| v.as_array().map(|a| a.len()))?;
                Some((issues, projects, name, description))
            })
            .flatten()
    }

    fn status(&self) -> Response {
        let counts = self.counts();
        let (issues, projects, name, description) =
            counts
                .clone()
                .unwrap_or((0, 0, String::new(), String::new()));
        Response::Status(Box::new(StatusInfo {
            id: crate::crypto::device_from_seed(&self.device_seed).to_string(),
            nick: String::new(),
            name,
            description,
            online_peers: self.online_peers(),
            space: Some(self.station.space_id().as_str().to_string()),
            counts_unavailable: counts.is_none(),
            issues,
            projects,
            membership: if self.mechanics.am_i_member() {
                "member".into()
            } else {
                "pending".into()
            },
            degraded_recovery: self.mechanics.degraded_recovery(),
            recovery: Some(self.mechanics.recovery_status()),
        }))
    }

    fn members(&self) -> Response {
        // Overlay this node's local petnames (`aliases.json`) into the roster so
        // the CLI and the viewer both render a member's name, not a bare actor
        // id. The alias is local, never synced (the trusted half of the identity
        // model) — the daemon is this node, so it is the right place to apply it.
        let mut members = self.mechanics.members();
        let aliases = read_aliases(&self.home);
        if !aliases.is_empty() {
            for m in &mut members {
                if let Some(name) = aliases.get(&m.key).or_else(|| {
                    // Aliases may be keyed by a short `act_` prefix the operator
                    // typed; match a stored key that prefixes this full actor id.
                    aliases
                        .iter()
                        .find(|(k, _)| !k.is_empty() && m.key.starts_with(k.as_str()))
                        .map(|(_, v)| v)
                }) {
                    m.alias = name.clone();
                }
            }
        }
        Response::Members { members }
    }

    /// Add a device to this actor from its hex-encoded consent blob (produced
    /// by the joining machine's `device accept`).
    fn device_add(&self, consent_hex: &str) -> Response {
        let binding: crate::actor::DeviceBinding = match data_encoding::HEXLOWER_PERMISSIVE
            .decode(consent_hex.trim().as_bytes())
            .ok()
            .and_then(|b| postcard::from_bytes(&b).ok())
        {
            Some(b) => b,
            None => return Response::err("device consent blob did not decode"),
        };
        match self.mechanics.device_add(binding) {
            Ok(device) => Response::Ok {
                message: Some(format!("added device {}", device.short())),
            },
            Err(e) => Response::err(format!("{e}")),
        }
    }

    /// This actor's device set, one per line, marking the active local device.
    fn device_list_text(&self) -> String {
        let me = crate::crypto::device_from_seed(&self.device_seed);
        let devices = self.mechanics.device_list();
        if devices.is_empty() {
            return "no devices".to_string();
        }
        devices
            .into_iter()
            .map(|d| {
                let tag = if d == me { " (this device)" } else { "" };
                format!("{}{}", d.as_str(), tag)
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Set (or clear, with an empty name) a local petname for a key. Local to
    /// this node, never broadcast, never part of the signed authority.
    fn set_alias(&self, who: &str, name: &str) -> Response {
        match write_alias(&self.home, who, name) {
            Ok(()) if name.trim().is_empty() => Response::Ok {
                message: Some(format!("cleared the local name for {who}")),
            },
            Ok(()) => Response::Ok {
                message: Some(format!("{who} is now locally known as {name}")),
            },
            Err(e) => Response::err(format!("set alias: {e}")),
        }
    }

    /// The guided-join verifier: project live daemon state into the ordered
    /// onboarding gate list (`docs/UI.md`). Pure over the snapshot the daemon
    /// already computes — the same core the legacy node used.
    fn diagnose(&self, expected_space: Option<String>) -> Response {
        let (issues, projects, _name, _description) =
            self.counts()
                .unwrap_or((0, 0, String::new(), String::new()));
        let space = self.station.space_id().as_str().to_string();
        let membership = if self.mechanics.am_i_member() {
            "member"
        } else {
            "pending"
        };
        let degraded = self.mechanics.degraded_recovery();
        let recovery = self.mechanics.recovery_status();
        let view = crate::diagnose::diagnose(crate::diagnose::DiagnoseInput {
            space: Some(space.as_str()),
            name: "",
            membership,
            online_peers: self.online_peers(),
            projects,
            issues,
            expected_space: expected_space.as_deref(),
            degraded_recovery: &degraded,
            rekey_pending: None,
            local_custody: Some(&recovery.local_custody),
        });
        Response::Diagnosis(Box::new(view))
    }

    /// Pin a bootstrap seed by device id (or an orbital Coordinates link's
    /// approach Station) into the node-local registry.
    fn seed_add(&self, arg: &str) -> Response {
        let (id, space) = match crate::ids::DeviceId::parse(arg.trim()) {
            Some(id) => (id, String::new()),
            None => match runtime::SignedCoordinates::parse_link(arg.trim())
                .ok()
                .and_then(|c| c.verify().ok())
            {
                Some(v) => (v.approach_station.clone(), v.space.as_str().to_string()),
                None => return Response::err("expected a device id or a Coordinates link to pin"),
            },
        };
        let newly = upsert_seed(
            &self.home,
            SeedRecord {
                id: id.clone(),
                nick: String::new(),
                space,
            },
        );
        Response::Ok {
            message: Some(if newly {
                format!("pinned seed {}", id.as_str())
            } else {
                format!("seed {} was already pinned (refreshed)", id.as_str())
            }),
        }
    }

    /// The pinned seed registry with live reachability from the Station's
    /// current neighbor set.
    fn seed_list(&self) -> Response {
        let online: std::collections::BTreeSet<[u8; 32]> = self
            .station
            .neighbors()
            .iter()
            .map(|n| n.station.key_bytes())
            .collect();
        let seeds = load_seeds(&self.home)
            .into_iter()
            .map(|s| {
                // A seed is pinned by device id; a Neighbor is a Station id —
                // both are the same 32-byte key, so reachability compares those.
                let is_online =
                    s.id.key_bytes()
                        .map(|k| online.contains(&k))
                        .unwrap_or(false);
                crate::dto::SeedDto {
                    id: s.id.as_str().to_string(),
                    nick: s.nick,
                    space: s.space,
                    state: if is_online { "online" } else { "offline" }.to_string(),
                    online: is_online,
                }
            })
            .collect();
        Response::Seeds { seeds }
    }

    /// Unpin seeds matching a full id, id-prefix, or nick.
    fn seed_remove(&self, needle: &str) -> Response {
        match remove_seed(&self.home, needle) {
            0 => Response::err("no pinned seed matched that id/nick"),
            n => Response::Ok {
                message: Some(format!("unpinned {n} seed(s)")),
            },
        }
    }

    // ---- membership ceremonies (formatting mirrors the product adapters) -----

    fn space_recover(&self) -> Response {
        use mechanics::ceremony::{SpaceRecovered, SpaceRecovery};
        match self.mechanics.space_recover() {
            Ok(SpaceRecovery::Installed(SpaceRecovered { root, rekey_failed })) => {
                let head = format!("recovered the space — root reset to {}", root.short());
                Response::Ok {
                    message: Some(match rekey_failed {
                        None => format!("{head} and re-keyed"),
                        Some(e) => format!(
                            "{head}, but re-keying failed ({e:#}) — the space is still readable \
                             under the old key until an admin rotates it"
                        ),
                    }),
                }
            }
            Ok(SpaceRecovery::Pending {
                session,
                incomplete,
            }) => {
                let hex = session.to_hex();
                let head = format!(
                    "group recovery under way (session {hex}) — each other holder must approve \
                     it with `space recover-approve {hex}` until the threshold co-signs"
                );
                Response::Ok {
                    message: Some(match incomplete {
                        None => head,
                        Some(e) => format!(
                            "{head}. This device could not add its own share ({e:#}); the request \
                             stands and the other holders can still complete it"
                        ),
                    }),
                }
            }
            Err(e) => Response::err(format!("{e}")),
        }
    }

    fn space_recover_approve(&self, session: String, expect: Vec<String>) -> Response {
        match self.mechanics.space_recover_approve(session, expect) {
            Ok(a) => {
                let roots = a
                    .roots
                    .iter()
                    .map(|r| r.short())
                    .collect::<Vec<_>>()
                    .join(", ");
                Response::Ok {
                    message: Some(match a.incomplete {
                        None => format!(
                            "co-signed the recovery re-rooting the space to {roots} — it installs \
                             once the threshold has co-signed"
                        ),
                        Some(e) => format!(
                            "co-signed the recovery re-rooting the space to {roots}, and that \
                             completed the threshold — but re-keying failed ({e:#}), so the space \
                             is still readable under the old key until an admin rotates it"
                        ),
                    }),
                }
            }
            Err(e) => Response::err(format!("{e}")),
        }
    }

    fn space_elevate(&self, cofounders: Vec<String>, k: u16) -> Response {
        match self.mechanics.space_elevate(cofounders, k) {
            Ok(e) => {
                let message = match (e.grant_request, e.incomplete) {
                    (_, Some(why)) => format!(
                        "proposed a {}-of-{} recovery arrangement (proposal {}) — but this device \
                         could not carry it further ({why:#}); the proposal stands and can still \
                         be authorized",
                        e.k,
                        e.n,
                        e.proposal.to_hex()
                    ),
                    (None, None) => format!(
                        "started {}-of-{} recovery elevation — the DKG completes automatically as \
                         the co-founders' nodes sync; the group key installs once every share is in",
                        e.k, e.n
                    ),
                    (Some(signing), None) => format!(
                        "proposed a {}-of-{} recovery arrangement (proposal {}) — the current \
                         group must authorize it: each holder runs `space elevate-approve {} \
                         --proposal {}`",
                        e.k,
                        e.n,
                        e.proposal.to_hex(),
                        signing.to_hex(),
                        e.proposal.to_hex(),
                    ),
                };
                Response::Ok {
                    message: Some(message),
                }
            }
            Err(e) => Response::err(format!("{e}")),
        }
    }

    fn space_elevate_approve(&self, session: String, proposal: String) -> Response {
        match self.mechanics.space_elevate_approve(session, proposal) {
            Ok(a) => Response::Ok {
                message: Some(format!(
                    "co-signed the authorization for a {}-of-{} arrangement — it takes effect \
                     once the threshold has signed",
                    a.k, a.n
                )),
            },
            Err(e) => Response::err(format!("{e}")),
        }
    }

    fn space_reshare(&self, participants: Vec<String>, k: u16) -> Response {
        match self.mechanics.space_reshare(participants, k) {
            Ok(e) => {
                let message = match (e.grant_request, e.incomplete) {
                    (_, Some(why)) => format!(
                        "proposed resharing the recovery key onto a {}-of-{} arrangement \
                         (proposal {}) — but this device could not carry it further ({why:#}); \
                         the proposal stands and can still be authorized",
                        e.k,
                        e.n,
                        e.proposal.to_hex()
                    ),
                    (Some(signing), None) => format!(
                        "proposed resharing the recovery key onto a {}-of-{} arrangement \
                         (proposal {}) — the current group must authorize it: each holder runs \
                         `space elevate-approve {} --proposal {}`. The key itself does not change.",
                        e.k,
                        e.n,
                        e.proposal.to_hex(),
                        signing.to_hex(),
                        e.proposal.to_hex(),
                    ),
                    (None, None) => format!(
                        "started resharing the recovery key onto a {}-of-{} arrangement — the \
                         redistribution completes automatically as the holders' nodes sync",
                        e.k, e.n
                    ),
                };
                Response::Ok {
                    message: Some(message),
                }
            }
            Err(e) => Response::err(format!("{e}")),
        }
    }

    fn space_custody_export(&self, path: String, passphrase: String) -> Response {
        match self.mechanics.space_custody_export(path, passphrase) {
            Ok(e) => {
                let note = if !e.indispensable {
                    "this arrangement tolerates a lost holder, so no attestation is required to \
                     install it"
                        .to_string()
                } else if e.outstanding == 0 {
                    "every custodian has attested — the arrangement can now install".to_string()
                } else {
                    format!("still waiting on {} custodian(s)", e.outstanding)
                };
                Response::Ok {
                    message: Some(format!(
                        "exported and verified your share package to {} — {note}. Keep it \
                         somewhere the passphrase alone cannot be found.",
                        e.path
                    )),
                }
            }
            Err(e) => Response::err(format!("{e}")),
        }
    }

    fn space_custody_import(&self, path: String, passphrase: String, force: bool) -> Response {
        match self.mechanics.space_custody_import(path, passphrase, force) {
            Ok(i) => {
                let head = format!(
                    "restored and verified your share for ceremony {} — this device can take part \
                     in recovery again",
                    i.ceremony.to_hex()
                );
                Response::Ok {
                    message: Some(match i.incomplete {
                        None => head,
                        Some(e) => format!(
                            "{head}. The ceremony did not advance here ({e:#}); it will retry on \
                             the next sync"
                        ),
                    }),
                }
            }
            Err(e) => Response::err(format!("{e}")),
        }
    }

    /// The addressed-to-you inbox — ONE World query over the derived read
    /// model (plan 04: activity/inbox rebuild from query and are never a
    /// second source of truth). The read watermark is a small local file;
    /// deleting it merely resets "unread".
    fn inbox_projection(&self) -> (Vec<crate::dto::InboxEntry>, u64) {
        use crate::world::contract::IssueQuery;
        let me_actor = self.facts().actor;
        let me_device = crate::crypto::device_from_seed(&self.device_seed)
            .as_str()
            .to_string();
        let Some(rows) = self.session_query_json(IssueQuery::Inbox {
            actor: me_actor.as_str().to_string(),
            exclude_device: Some(me_device),
        }) else {
            return (Vec::new(), 0);
        };
        let watermark = self.read_inbox_watermark();
        let mut entries: Vec<crate::dto::InboxEntry> = Vec::new();
        for e in rows.as_array().map(|a| a.as_slice()).unwrap_or_default() {
            entries.push(crate::dto::InboxEntry {
                ts: e["ts"].as_u64().unwrap_or(0),
                kind: e["kind"].as_str().unwrap_or_default().to_string(),
                reff: e["reff"].as_str().unwrap_or_default().to_string(),
                doc_id: e["doc_id"].as_str().unwrap_or_default().to_string(),
                title: e["title"].as_str().unwrap_or_default().to_string(),
                detail: e["detail"].as_str().unwrap_or_default().to_string(),
                actor: e["actor"].as_str().map(String::from),
                actor_nick: None,
            });
        }
        entries.truncate(200);
        let unread = entries.iter().filter(|e| e.ts > watermark).count() as u64;
        (entries, unread)
    }

    fn inbox_watermark_path(&self) -> PathBuf {
        self.home.join("inbox-read.json")
    }

    fn read_inbox_watermark(&self) -> u64 {
        std::fs::read_to_string(self.inbox_watermark_path())
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0)
    }

    fn write_inbox_watermark(&self, ts: u64) {
        let _ = std::fs::write(self.inbox_watermark_path(), ts.to_string());
    }

    /// Query the docked Session for a JSON projection (role/workflow views).
    fn session_query_json(
        &self,
        query: crate::world::contract::IssueQuery,
    ) -> Option<serde_json::Value> {
        let world = crate::world::contract::world_id();
        if !self.ensure_world_session(&world) {
            return None;
        }
        self.worlds
            .with_primary(&world, |session| {
                let bytes = session
                    .query(runtime::WorldQuery {
                        schema: crate::world::contract::issue_schema(),
                        schema_version: crate::world::contract::ISSUE_SCHEMA_VERSION,
                        payload: query.to_json(),
                    })
                    .ok()?
                    .bytes;
                serde_json::from_slice(&bytes).ok()
            })
            .flatten()
    }

    /// Expand a role's pinned definition (read from the Manifest-pinned
    /// Catalog through the Session) and install the exact assignments as one
    /// Mechanics authority batch. IssuesWorld plans the expansion; Runtime
    /// validates; Mechanics commits authority-first.
    fn access_grant(&self, actor: &str, role: &str, project: Option<&str>) -> Response {
        let Some(subject) = self.mechanics.resolve_actor_ref(actor) else {
            return Response::not_found(format!("no actor matches '{actor}'"));
        };
        let Some(view) = self.session_query_json(crate::world::contract::IssueQuery::RoleShow {
            role: role.to_string(),
        }) else {
            return Response::not_found(format!("no role `{role}` in this space"));
        };
        let conflicts = view["conflict_heads"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        if !conflicts.is_empty() {
            return Response::err(format!(
                "role `{role}` has {} concurrent revision heads — resolve them with                  `role resolve` before assigning",
                conflicts.len()
            ));
        }
        let Some(revision) = view.get("revision").filter(|r| !r.is_null()) else {
            return Response::not_found(format!("role `{role}` has no usable revision"));
        };
        let body = &revision["body"];
        if body["tombstone"].as_bool() == Some(true) {
            return Response::err(format!("role `{role}` is tombstoned"));
        }
        let scope_kind = body["scope_kind"].as_str().unwrap_or("space");
        let world = crate::world::contract::PRODUCT_WORLD;
        let resource = match (scope_kind, project) {
            ("space", None) => mechanics::demand::PolicyResource::space(world),
            ("space", Some(_)) => {
                return Response::err("that is a Space role — it takes no --project")
            }
            ("project", Some(sel)) => {
                let Some(snapshot) =
                    self.session_query_json(crate::world::contract::IssueQuery::Snapshot)
                else {
                    return Response::err("the catalog is unavailable");
                };
                let projects = snapshot["catalog"]["projects"].as_object().cloned();
                let resolved = projects.and_then(|m| {
                    let upper = sel.to_ascii_uppercase();
                    if m.contains_key(sel) {
                        return Some(sel.to_string());
                    }
                    m.iter()
                        .find(|(_, meta)| meta["key"].as_str() == Some(upper.as_str()))
                        .map(|(id, _)| id.clone())
                });
                match resolved {
                    Some(id) => mechanics::demand::PolicyResource::project(world, &id),
                    None => return Response::not_found(format!("no project matches '{sel}'")),
                }
            }
            ("project", None) => {
                return Response::err("that is a Project role — pass -p <project>")
            }
            _ => return Response::err("unrecognized role scope"),
        };
        let assignments: Vec<(
            mechanics::demand::PolicyCapability,
            mechanics::demand::PolicyResource,
        )> = body["capabilities"]
            .as_array()
            .map(|caps| {
                caps.iter()
                    .filter_map(|c| c.as_str())
                    .map(|c| {
                        (
                            mechanics::demand::PolicyCapability::new(world, c),
                            resource.clone(),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        if assignments.is_empty() {
            return Response::err(format!("role `{role}` expands to no capabilities"));
        }
        match self.mechanics.grant_assignments(&subject, &assignments) {
            Ok(granted) => Response::Ok {
                message: Some(format!(
                    "granted {} capability assignment(s) from role `{role}` to {}",
                    granted.len(),
                    subject.short()
                )),
            },
            Err(e) => Response::err(format!("{e}")),
        }
    }

    fn invite(&self, role: Option<&str>, reusable: bool, ttl_hours: Option<u64>) -> Response {
        // Mint an admission-bearing Coordinates link. Accepting the invite is
        // the approval: the capability carries the selected role's exact
        // expanded assignments (default contributor), and redemption is
        // automatic on Contact. `reusable` admits a team (up to the redemption
        // cap) instead of one person.
        let ttl_secs = ttl_hours.unwrap_or(168).max(1).saturating_mul(3600);
        let parent_root = self.station.frontier().root;
        let admission = match self.mechanics.mint_admission(
            &self.device_seed,
            ttl_secs,
            reusable,
            now_secs(),
            role.unwrap_or("contributor"),
            parent_root,
        ) {
            Ok(a) => a,
            Err(e) => return Response::err(format!("mint admission: {e}")),
        };
        match self.mechanics.mint_coordinates(
            &self.device_seed,
            "",
            self.advertised_routes.clone(),
            Some(admission),
        ) {
            Ok(coords) => Response::Ref {
                reff: coords.render(),
            },
            Err(e) => Response::err(format!("mint coordinates: {e}")),
        }
    }

    fn connect(&self, link: &str) -> Response {
        // The manual nudge (W0-S5): a running daemon "connecting" triggers an
        // administrative Contact now, bypassing backoff. Accepts a station id
        // to dial, or a Coordinates link — whose signed approach routes are
        // taught to the transport first, so the dial resolves even after the
        // peer's addresses changed. Coordinates *entry* (store bootstrap)
        // stays `lait join`'s job.
        let link = link.trim();
        let station =
            match crate::ids::DeviceId::parse(link).and_then(|d| StationId::from_device(&d)) {
                Some(station) => Some(station),
                None => runtime::SignedCoordinates::parse_link(link)
                    .ok()
                    .and_then(|c| c.verify().ok())
                    .and_then(|v| {
                        if !v.approach_routes.is_empty() {
                            self.transport
                                .learn(v.approach_station.clone(), &v.approach_routes);
                        }
                        StationId::from_device(&v.approach_station)
                    }),
            };
        match station {
            Some(station) => match self.station.contact(&station, ContactOptions) {
                Ok(outcome) => Response::Ok {
                    message: Some(format!(
                        "contacted — {} bytes moved{}",
                        outcome.bytes_moved,
                        if outcome.convergence.advanced() {
                            ", new material incorporated"
                        } else {
                            ", already converged"
                        }
                    )),
                },
                Err(e) => Response::err(format!("contact: {e:?}")),
            },
            None => Response::err("connect expects a station id or an invite link"),
        }
    }

    /// Serve the control IPC loop until shutdown.
    pub async fn serve(self: Arc<Self>) -> Result<()> {
        let control = control_name(&self.home)?;
        #[cfg(unix)]
        let _ = std::fs::remove_file(crate::config::socket_path(&self.home));
        let listener = ListenerOptions::new()
            .name(control)
            .create_tokio()
            .context("bind control channel")?;
        tracing::info!(
            "space bridge online in space {}",
            self.station.space_id().as_str()
        );
        let idle_window = idle_window_from_env();
        let mut idle_tick = tokio::time::interval(Duration::from_millis(500));
        let mut connections = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                _ = self.shutdown.notified() => break,
                _ = idle_tick.tick() => {
                    // The store watchdog (LOCAL-9): a daemon must never
                    // outlive its store. With the directory gone, this
                    // process can only serve stale memory while blocking its
                    // own clients (presence is a directory scan) — stop
                    // loudly instead.
                    if !self.store_dir().is_dir() {
                        tracing::error!(
                            "orbital store at {} is gone — the SpaceBridge will not \
                             outlive its store; stopping",
                            self.store_dir().display()
                        );
                        self.begin_stop();
                        break;
                    }
                    if self.should_idle_shutdown(idle_window) {
                        tracing::info!("space bridge idle-shutdown after {idle_window:?}");
                        self.begin_stop();
                        break;
                    }
                },
                accept = listener.accept() => match accept {
                    Ok(stream) => {
                        let me = self.clone();
                        connections.spawn(async move { me.handle_conn(stream).await });
                    }
                    Err(e) => {
                        tracing::warn!("control accept error: {e}");
                        break;
                    }
                },
                result = connections.join_next(), if !connections.is_empty() => {
                    if let Some(Err(error)) = result {
                        tracing::warn!(%error, "control connection task failed");
                    }
                }
            }
        }
        // Wake and join every task retaining the bridge before the runner tries
        // to consume it and return the Station to Orbit.
        self.begin_stop();
        tokio::time::timeout(Duration::from_secs(5), async {
            while let Some(result) = connections.join_next().await {
                if let Err(error) = result {
                    tracing::warn!(%error, "control connection task failed during shutdown");
                }
            }
        })
        .await
        .map_err(|_| {
            anyhow!(
                "SpaceBridge control connections did not drain during shutdown ({} remain)",
                connections.len()
            )
        })?;
        let pump = self.pump.lock().await.take();
        if let Some(pump) = pump {
            tokio::time::timeout(Duration::from_secs(5), pump)
                .await
                .map_err(|_| anyhow!("SpaceBridge Observation pump did not drain during shutdown"))?
                .map_err(|error| anyhow!("SpaceBridge Observation pump failed: {error}"))?;
        }
        Ok(())
    }

    /// Consume the bridge and return its Station to a durable Orbit.
    ///
    /// World Sessions are dropped first so dormancy can reject all future
    /// callbacks, drain Station tasks, and release the store lock last.
    pub fn go_dormant(self) -> Result<()> {
        let Self {
            station, worlds, ..
        } = self;
        drop(worlds);
        station
            .go_dormant()
            .map(|_| ())
            .map_err(|e| anyhow!("SpaceBridge dormancy failed: {e:?}"))
    }

    /// This Space's on-disk store directory (the watchdog's liveness probe).
    fn store_dir(&self) -> PathBuf {
        orbital_store_root(&self.home).join(self.station.space_id().as_str())
    }

    /// Whether the idle window has elapsed with nothing to keep us alive: a
    /// non-zero window, no in-flight connections, no neighbors to converge with,
    /// and no activity for at least the window. Mirrors the legacy node's rule.
    fn should_idle_shutdown(&self, window: Duration) -> bool {
        use std::sync::atomic::Ordering;
        if window.is_zero() {
            return false;
        }
        if self.active_conns.load(Ordering::SeqCst) != 0 {
            return false;
        }
        if !self.station.neighbors().is_empty() {
            return false;
        }
        let idle_for = self
            .last_activity
            .lock()
            .map(|t| t.elapsed())
            .unwrap_or_default();
        idle_for >= window
    }

    async fn handle_conn(self: Arc<Self>, stream: LocalStream) {
        use std::sync::atomic::Ordering;
        self.active_conns.fetch_add(1, Ordering::SeqCst);
        // Decrement + stamp activity on every exit path (guard on drop).
        struct ConnGuard<'a>(&'a SpaceBridge);
        impl Drop for ConnGuard<'_> {
            fn drop(&mut self) {
                self.0.active_conns.fetch_sub(1, Ordering::SeqCst);
                if let Ok(mut t) = self.0.last_activity.lock() {
                    *t = std::time::Instant::now();
                }
            }
        }
        let _guard = ConnGuard(&self);

        let (read_half, write_half) = tokio::io::split(stream);
        let mut reader = BufReader::new(read_half);
        let mut line = String::new();
        if reader.read_line(&mut line).await.is_err() {
            return;
        }
        let crate::control::ClientRequest {
            route,
            act_as,
            request: req,
        } = match serde_json::from_str::<crate::control::ClientRequest>(line.trim()) {
            Ok(env) => env,
            Err(e) => {
                let _ = write_line(write_half, &Response::err(format!("bad request: {e}"))).await;
                return;
            }
        };
        // Validate before handling either control-flow request. In particular,
        // an invalidly routed Stop must not tear down this bridge, and a
        // subscription must not bypass the same Space-boundary check applied to
        // ordinary observation requests.
        if let Err(response) = self.validate_route(route.as_ref(), crate::control::classify(&req)) {
            let _ = write_line(write_half, &response).await;
            return;
        }
        if let Request::Subscribe { .. } = req {
            self.stream_subscribe(write_half).await;
            return;
        }
        // Stop is a real teardown request: answer, then signal the serve loop
        // to return (the caller decides whether to exit the process).
        let stop = matches!(req, Request::Stop);
        let resp = self.dispatch(route.as_ref(), req, act_as.as_deref());
        let _ = write_line(write_half, &resp).await;
        if stop {
            self.begin_stop();
        }
    }

    /// One ring's worth of committed state: the doc index keyed by Body id, and
    /// a digest per catalog plane.
    ///
    /// A Body id is a one-way hash of its doc id, so that correspondence only
    /// runs *forwards*: there is nothing to invert, and the only honest way back
    /// is to derive it from the docs the catalog names. Both halves come from
    /// ONE `RingDigest` projection so they cannot describe two different roots,
    /// and the whole thing is read fresh per ring rather than retained — a
    /// derived map keyed by anything but the exact root it was read at can
    /// answer for two roots at once (`tests/mixed_root_guard.rs`).
    ///
    /// `None` when there is no docked Session to read (an un-admitted joiner).
    fn ring_state(&self) -> Option<RingState> {
        let value = self.session_query_json(crate::world::contract::IssueQuery::RingDigest)?;
        // Typed, and all-or-nothing. A plane this daemon cannot decode used to
        // be skipped one entry at a time, which turns a schema mismatch into a
        // resource that silently never refreshes — the exact failure the whole
        // dirty-set exists to prevent. Failing the decode instead drops us to
        // the coarse path, which is visible and correct.
        let view: crate::world::contract::RingDigestView = match serde_json::from_value(value) {
            Ok(view) => view,
            Err(e) => {
                tracing::warn!(error = %e, "ring digest did not decode; ringing coarsely");
                return None;
            }
        };
        let docs = view
            .docs
            .into_iter()
            .map(|d| {
                let project = ProjectRef {
                    project_id: d.project_id,
                    project_key: d.project_key,
                };
                (
                    crate::world::contract::issue_body_id(&d.doc),
                    (d.doc, project),
                )
            })
            .collect();
        let planes = view
            .planes
            .into_iter()
            .map(|p| (p.plane, p.digest))
            .collect();
        Some(RingState { docs, planes })
    }

    /// Translate an Observation's Body scopes into a doorbell's dirty-set.
    ///
    /// This is the whole difference between a doorbell that says "something
    /// happened somewhere" and one a client can act on. Every scope is either the
    /// Space's single catalog Body or one issue Body; the issue ones resolve
    /// through [`Self::doc_index`] into `(project KEY, doc id)` pairs,
    /// which is exactly what the client re-reads by.
    ///
    /// A catalog hit names the *Body*, not the plane inside it — and the Body
    /// holds every structure the space has. The plane is recovered by diffing
    /// per-plane digests against the previous ring, which is the only method
    /// that works for a peer's change as well as our own: convergence ships CRDT
    /// **state**, not operations, so an incorporating node has no path list to
    /// read. Diffing what the state became is symmetric by construction.
    ///
    /// Conservative where it cannot be sure — a dirty flag may over-report,
    /// never under-report. With no baseline (first ring after a restart) every
    /// plane is reported once; a doc scope that names no row (a tombstoned doc
    /// is off every board) falls back to the `Docs` plane rather than being
    /// silently dropped.
    fn dirty_from_scopes(
        &self,
        scopes: &[replica::ids::BodyKey],
    ) -> (Vec<DirtyProject>, Vec<CatalogScope>) {
        let catalog_body = crate::world::contract::catalog_body_id(self.station.space_id());
        let mut catalog_dirty = false;
        let mut docs: Vec<&replica::ids::BodyId> = Vec::new();
        for key in scopes {
            if key.body == catalog_body {
                catalog_dirty = true;
            } else {
                docs.push(&key.body);
            }
        }
        if !catalog_dirty && docs.is_empty() {
            return Default::default();
        }

        let Some(state) = self.ring_state() else {
            // No projection to read at all. Name nothing rather than claim
            // nothing moved — the caller's `reset` path is the honest signal,
            // and an un-admitted joiner has no subscriber to mislead anyway.
            return Default::default();
        };
        let (by_project, missed) = resolve_docs(Some(&state.docs), &docs);

        let mut planes: Vec<CatalogScope> = Vec::new();
        if catalog_dirty || missed {
            let mut baseline = self.ring_planes.lock().expect("ring planes lock");
            match baseline.as_ref() {
                Some(previous) => {
                    for (scope, digest) in &state.planes {
                        if previous.get(scope) != Some(digest) {
                            planes.push(scope.clone());
                        }
                    }
                    // A plane that vanished changed too — emptied is not absent.
                    for scope in previous.keys() {
                        if !state.planes.contains_key(scope) {
                            planes.push(scope.clone());
                        }
                    }
                }
                // Nothing to compare against: report every plane once rather
                // than guess. The next ring diffs normally.
                None => planes.extend(state.planes.keys().cloned()),
            }
            *baseline = Some(state.planes);
            // A doc we could not name is a row-index question, and that is a
            // plane — no need to fall back to the whole catalog for it.
            if missed && !planes.contains(&CatalogScope::Docs) {
                planes.push(CatalogScope::Docs);
            }
        }
        (by_project, planes)
    }

    /// Start the single Observation pump if it is not already running.
    ///
    /// One stream, one translation, one fan-out — see [`Self::doorbells`]. The
    /// pump owns the blocking iterator through a TRACKED blocking task that
    /// blocks in bounded windows and re-checks cancellation between them; the
    /// async side selects on that channel and the teardown watch, so a Stop
    /// wakes it immediately and the worker exits within one window.
    async fn ensure_doorbell_pump(self: &Arc<Self>) {
        use std::sync::atomic::Ordering;
        // Held for the whole of startup: a caller that arrives mid-start blocks
        // here and returns only once the stream is open, never in between.
        let mut running = self.pump.lock().await;
        if let Some(task) = running.as_ref() {
            if !task.is_finished() {
                return;
            }
        }
        if let Some(finished) = running.take() {
            let _ = finished.await;
        }
        // Seed the diff baseline BEFORE opening the stream. A commit landing in
        // between is then counted as already-known rather than as a change with
        // nothing to compare against — and it is covered regardless, because
        // every subscriber sends itself a reset after this returns. Seeding
        // after the stream opened would be the harmful order: the first record
        // would diff against a baseline that already contained it and report no
        // planes at all.
        if let Some(state) = self.ring_state() {
            *self.ring_planes.lock().expect("ring planes lock") = Some(state.planes);
        }
        let world = crate::world::contract::world_id();
        let mut stream = match self
            .worlds
            .with_primary(&world, |session| session.observe(None))
        {
            Some(stream) => stream,
            None => return,
        };
        // Drain the initial reset record: subscribers get their own reset when
        // they attach, so replaying this one would only duplicate it.
        let _ = stream.try_next();

        let daemon = self.clone();
        let task = tokio::spawn(async move {
            let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let worker_cancel = cancel.clone();
            let (tx, mut rx) = tokio::sync::mpsc::channel::<runtime::Observation>(64);
            let worker = tokio::task::spawn_blocking(move || loop {
                if worker_cancel.load(Ordering::SeqCst) {
                    return;
                }
                match stream.next_timeout(Duration::from_millis(250)) {
                    Ok(Some(record)) => {
                        if tx.blocking_send(record).is_err() {
                            return; // the pump went away
                        }
                    }
                    Ok(None) => continue, // idle window: re-check cancellation
                    Err(_) => return,     // station dormant: stream closed
                }
            });
            let mut stop_rx = daemon.stop_tx.subscribe();
            loop {
                if *stop_rx.borrow() {
                    break;
                }
                tokio::select! {
                    changed = stop_rx.changed() => {
                        if changed.is_err() || *stop_rx.borrow() {
                            break;
                        }
                    }
                    record = rx.recv() => {
                        let Some(record) = record else { break }; // worker ended
                        // Send even with no receivers: an `Err` here means only
                        // that nobody is listening right now, which is not a
                        // reason to stop translating or to tear the pump down.
                        let _ = daemon.doorbells.send(daemon.frame_for(&record));
                    }
                }
            }
            cancel.store(true, Ordering::SeqCst);
            drop(rx);
            let _ = worker.await;
        });
        *running = Some(task);
    }

    /// Translate one Observation into the frame every subscriber receives.
    ///
    /// The Observation names Bodies; a client re-reads by project and doc.
    /// This is what makes the frame actionable — and a local commit and a
    /// peer's incorporated one arrive on the same stream, so both ring alike.
    fn frame_for(&self, record: &runtime::Observation) -> Doorbell {
        let (dirty_by_project, dirty_catalog) = self.dirty_from_scopes(&record.scopes);
        let body_news = !dirty_by_project.is_empty() || !dirty_catalog.is_empty();
        Doorbell {
            epoch: record.epoch.as_u64(),
            seq: record.sequence,
            reset: record.reset,
            dirty_by_project,
            dirty_catalog,
            authority_advanced: record.authority,
            // Authority alone advances no feed: it is membership news, and
            // `Activity` projects issue history. One record can carry both.
            activity_advanced: body_news,
            presence_advanced: record.authority,
        }
    }

    async fn stream_subscribe(self: &Arc<Self>, mut write_half: tokio::io::WriteHalf<LocalStream>) {
        // Without standing there is no Session to observe yet — emit the reset
        // and return; the client re-subscribes after admission.
        let world = crate::world::contract::world_id();
        if !self.ensure_world_session(&world) {
            let reset = Doorbell {
                reset: true,
                ..Default::default()
            };
            let _ = write_line_half(&mut write_half, &reset).await;
            return;
        }
        // Attach to the fan-out BEFORE reading the epoch we reset against, so
        // nothing published in between falls into the gap. A broadcast receiver
        // buffers from the moment it subscribes.
        let mut frames = self.doorbells.subscribe();
        // Returns only once the pump is READY, so the reset below is never sent
        // against a stream that has not opened yet.
        self.ensure_doorbell_pump().await;
        let epoch = match self
            .worlds
            .with_primary(&world, |session| session.epoch().as_u64())
        {
            Some(epoch) => epoch,
            None => return,
        };
        let reset = Doorbell {
            epoch,
            seq: 0,
            reset: true,
            ..Default::default()
        };
        if write_line_half(&mut write_half, &reset).await.is_err() {
            return;
        }

        let mut stop_rx = self.stop_tx.subscribe();
        loop {
            if *stop_rx.borrow() {
                break;
            }
            tokio::select! {
                changed = stop_rx.changed() => {
                    if changed.is_err() || *stop_rx.borrow() {
                        break;
                    }
                }
                frame = frames.recv() => {
                    let frame = match frame {
                        Ok(frame) => frame,
                        // Dropped frames: this subscriber's position is
                        // meaningless, which is exactly what a reset says.
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => Doorbell {
                            epoch,
                            reset: true,
                            ..Default::default()
                        },
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    };
                    if write_line_half(&mut write_half, &frame).await.is_err() {
                        break;
                    }
                }
            }
        }
    }

    /// Latch teardown: the atomic (for worker threads), the watch (for live
    /// subscriptions), and the serve loop's notify.
    fn begin_stop(&self) {
        self.stopping
            .store(true, std::sync::atomic::Ordering::SeqCst);
        // `send` discards the value when no receiver exists. A pump can be
        // spawned but not yet have subscribed, so shutdown must replace the
        // latched value for late receivers as well.
        self.stop_tx.send_replace(true);
        self.shutdown.notify_one();
    }
}

/// One ring's committed state, read at a single pinned root.
struct RingState {
    /// `Body id -> (doc id, the project that holds it)`.
    docs: std::collections::HashMap<replica::ids::BodyId, (String, ProjectRef)>,
    /// Digest per catalog plane.
    planes: std::collections::BTreeMap<CatalogScope, String>,
}

/// Group observed issue Bodies by project, reporting whether any of them was
/// absent from the index — the caller turns that miss into a coarser frame.
fn resolve_docs(
    index: Option<&std::collections::HashMap<replica::ids::BodyId, (String, ProjectRef)>>,
    docs: &[&replica::ids::BodyId],
) -> (Vec<DirtyProject>, bool) {
    let mut by_project: std::collections::BTreeMap<ProjectRef, Vec<String>> = Default::default();
    let mut missed = false;
    for body in docs {
        match index.and_then(|i| i.get(*body)) {
            Some((doc, project)) => by_project
                .entry(project.clone())
                .or_default()
                .push(doc.clone()),
            None => missed = true,
        }
    }
    let dirty = by_project
        .into_iter()
        .map(|(project, docs)| DirtyProject {
            project_id: project.project_id,
            project_key: project.project_key,
            docs,
        })
        .collect();
    (dirty, missed)
}

static CLOCK: std::sync::OnceLock<SystemUlidSource> = std::sync::OnceLock::new();

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Where a co-located sponsored agent's identity seed lives: `agents/<name>/
/// secret.key` under the daemon's home. Per-agent, tiny (64 hex bytes), beside
/// the one shared store — Architecture B: N signing identities, O(1) storage.
fn agent_seed_path(home: &Path, name: &str) -> PathBuf {
    home.join("agents").join(name).join("secret.key")
}

/// Create (first call) or load a co-located agent identity seed under `home`.
/// The seed is the agent's identity — self-certifying, reconstructable, and
/// persisted outside any working-directory sandbox (§10 finding 1).
fn load_or_create_agent_seed(home: &Path, name: &str) -> Result<[u8; 32]> {
    let path = agent_seed_path(home, name);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if path.exists() {
        load_agent_seed(home, name)
    } else {
        let seed = crate::crypto::random_seed();
        std::fs::write(&path, data_encoding::HEXLOWER.encode(&seed))?;
        Ok(seed)
    }
}

/// Load an existing co-located agent identity seed; errors if none is provisioned.
fn load_agent_seed(home: &Path, name: &str) -> Result<[u8; 32]> {
    let path = agent_seed_path(home, name);
    let hex = std::fs::read_to_string(&path)
        .with_context(|| format!("no agent identity '{name}' provisioned"))?;
    let raw = data_encoding::HEXLOWER_PERMISSIVE
        .decode(hex.trim().as_bytes())
        .map_err(|e| anyhow!("parse agent seed: {e}"))?;
    raw.as_slice()
        .try_into()
        .map_err(|_| anyhow!("agent seed must be 32 bytes"))
}

/// The idle-shutdown window, from `LAIT_IDLE_SECS` (0 disables), else 30 min —
/// the same contract the legacy node honors.
fn idle_window_from_env() -> Duration {
    const DEFAULT: Duration = Duration::from_secs(30 * 60);
    match std::env::var("LAIT_IDLE_SECS") {
        Ok(s) => s
            .trim()
            .parse::<u64>()
            .map(Duration::from_secs)
            .unwrap_or(DEFAULT),
        Err(_) => DEFAULT,
    }
}

fn comms_options(
    transport: Arc<dyn Transport>,
    seed: [u8; 32],
    mechanics: &OrbitalMechanics,
    bootstrap: Vec<crate::ids::DeviceId>,
    advertise: Vec<runtime::beacon::RouteHint>,
) -> CommsOptions {
    let export = mechanics.clone();
    let frontier = mechanics.clone();
    CommsOptions {
        transport,
        station_seed: seed,
        mechanics: ContactMechanics {
            source: Arc::new(mechanics.clone()),
            incorporator: Arc::new(Mutex::new(mechanics.clone()))
                as Arc<Mutex<dyn AuthorityIncorporator + Send>>,
            export: Arc::new(move || export.export_records()),
            frontier: Arc::new(move || frontier.current_frontier()),
        },
        gossip: Some(GossipOptions {
            bootstrap,
            advertise,
            // The heartbeat floor's base; emission is edge-triggered
            // (contact_driver §4.1) and this only bounds staleness.
            beacon_interval: Duration::from_secs(10),
        }),
        whole_deadline: Duration::from_secs(30),
        progress_deadline: Duration::from_secs(10),
        route_lease: Duration::from_secs(120),
    }
}

async fn write_line<T: serde::Serialize>(
    mut write_half: tokio::io::WriteHalf<LocalStream>,
    value: &T,
) -> std::io::Result<()> {
    write_line_half(&mut write_half, value).await
}

async fn write_line_half<T: serde::Serialize>(
    write_half: &mut tokio::io::WriteHalf<LocalStream>,
    value: &T,
) -> std::io::Result<()> {
    let mut out = serde_json::to_string(value)
        .unwrap_or_else(|_| "{\"kind\":\"error\",\"message\":\"encode failure\"}".to_string());
    out.push('\n');
    write_half.write_all(out.as_bytes()).await?;
    write_half.flush().await
}

/// One pinned bootstrap seed — a deliberately-placed anchor a cold client
/// converges through. The id is the identity; nick/space are advisory.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct SeedRecord {
    id: crate::ids::DeviceId,
    #[serde(default)]
    nick: String,
    #[serde(default)]
    space: String,
}

fn seeds_path(home: &Path) -> PathBuf {
    home.join("seeds.json")
}

/// Load the pinned seed registry, dropping (at warn) any record whose id is not
/// a device key so one bad row never unpins the rest.
fn load_seeds(home: &Path) -> Vec<SeedRecord> {
    let Ok(data) = std::fs::read_to_string(seeds_path(home)) else {
        return Vec::new();
    };
    let rows: Vec<SeedRecord> = serde_json::from_str(&data).unwrap_or_default();
    rows.into_iter()
        .filter(|r| crate::ids::DeviceId::parse(r.id.as_str()).is_some())
        .collect()
}

fn save_seeds(home: &Path, seeds: &[SeedRecord]) {
    if let Ok(data) = serde_json::to_string_pretty(seeds) {
        let _ = std::fs::write(seeds_path(home), data);
    }
}

/// Upsert a seed keyed by id (nick/space refresh in place). Returns whether it
/// was newly pinned.
fn upsert_seed(home: &Path, rec: SeedRecord) -> bool {
    let mut seeds = load_seeds(home);
    if let Some(existing) = seeds.iter_mut().find(|s| s.id == rec.id) {
        existing.nick = rec.nick;
        existing.space = rec.space;
        save_seeds(home, &seeds);
        false
    } else {
        seeds.push(rec);
        save_seeds(home, &seeds);
        true
    }
}

/// Unpin seeds matching a full id, a ≥6-char id prefix, or a nick. Returns the
/// count removed.
fn remove_seed(home: &Path, needle: &str) -> usize {
    let mut seeds = load_seeds(home);
    let before = seeds.len();
    seeds.retain(|s| {
        let id = s.id.as_str();
        !(id == needle || (needle.len() >= 6 && id.starts_with(needle)) || s.nick == needle)
    });
    let removed = before - seeds.len();
    if removed > 0 {
        save_seeds(home, &seeds);
    }
    removed
}

/// The local petname map (`aliases.json` beside the home).
fn read_aliases(home: &Path) -> std::collections::BTreeMap<String, String> {
    std::fs::read(home.join("aliases.json"))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

/// Set or clear a **local** petname for a key in `aliases.json` beside the
/// home. Local to this node, never synced; an empty `name` clears the entry.
fn write_alias(home: &Path, who: &str, name: &str) -> Result<()> {
    let path = home.join("aliases.json");
    let mut map: std::collections::BTreeMap<String, String> = std::fs::read(&path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    let who = who.trim().to_string();
    if name.trim().is_empty() {
        map.remove(&who);
    } else {
        map.insert(who, name.trim().to_string());
    }
    std::fs::write(&path, serde_json::to_vec_pretty(&map)?)?;
    Ok(())
}

/// Run one process-backed SpaceBridge on `home`, holding the per-home lock for
/// its lifetime. Identity is the process-global one.
pub async fn run_space_bridge(home: PathBuf, factory: &dyn TransportFactory) -> Result<()> {
    let device_seed = load_or_create_identity(&crate::config::identity_dir()?)?;
    run_space_bridge_with(home, device_seed, factory).await
}

/// The injectable process adapter: everything [`run_space_bridge`] does, but it
/// takes an explicit device seed. Several bridges may run in one process; the
/// process layout is deployment policy rather than part of Space semantics.
pub async fn run_space_bridge_with(
    home: PathBuf,
    device_seed: [u8; 32],
    factory: &dyn TransportFactory,
) -> Result<()> {
    SpaceBridgeRunner::start(home, device_seed, factory)
        .await?
        .run()
        .await
}
