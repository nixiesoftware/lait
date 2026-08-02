// Hosting uses validated protocol counters and bounded scheduling arithmetic;
// conversions adapt fixed-width durable and runtime identifiers.
#![allow(clippy::arithmetic_side_effects, clippy::as_conversions)]

//! Application hosting for one active Station.
//!
//! It composes [`SpaceAuthority`] (authority/keys/membership over signed
//! material), a [`Runtime`] hosting the build's registered Worlds, and a
//! [`Station`] with the comms Contact plane. [`WorldRouter`] owns one
//! host per installed World. The process adapter serves Space-owned
//! `control::Request`/`Response` IPC and product-neutral [`Call`] envelopes,
//! while an owning Daemon can invoke a World host directly in-process.
//! Product requests route through the call handler registered in their injected
//! [`WorldPackage`](crate::orbital::WorldPackage); peer exchange is
//! Contact/Convergence over `comms`;
//! invitation is Coordinates v1; `Subscribe` streams the Station's
//! `ObservationStream` as `Doorbell` frames.
//!
//! Every control request has an explicit terminal owner (see
//! `tests/control_classification.rs`): product intents/queries route to the
//! World Session; membership, admission, device, key and the FROST
//! recovery/elevation/custody ceremonies are served by [`SpaceAuthority`]
//! over the mechanics primitives; seeds, diagnose, and log are node-local
//! lifecycle concerns. There is no catch-all refusal.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, Context as AnyhowContext, Result};
use interprocess::local_socket::{
    tokio::{prelude::*, Stream as LocalStream},
    ListenerOptions,
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use mechanics::recovery::Completion;
use mechanics::{ids::SpaceId, station::Key};
use replica::body::WorldId;
use replica::convergence::AuthorityIncorporator;
use runtime::{
    plane::contact::Authority, plane::Activation, plane::CommsOptions, plane::GossipOptions,
    world::LocalIdentity, Runtime, Session, Station,
};

use crate::config::{acquire_daemon_lock, load_or_create_identity, DaemonLock};
use crate::control::OrbitAddress;
use crate::control::{
    control_name, ContentCall, ContentErrorCode, ContentReply, ControlRoute, Doorbell, Request,
    RequestOwner, Response, StatusInfo, UploadReader,
};
use crate::orbital::{
    orbital_store_root, unsupported_store_at, SpaceAuthority, WorldPackages, WorldRouter,
};
use comms::{Transport, TransportFactory};
use runtime::world::call::{Access, Call, Code, Context, Reply};

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
/// The compatibility-process adapter gives it a per-home control listener; a general
/// Lait daemon can instead hold several instances and route by Space id.
pub struct StationHost {
    /// The durable local Orbit occupied by this host's Station.
    address: OrbitAddress,
    mechanics: SpaceAuthority,
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
    worlds: WorldRouter,
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
    /// The one enriched-doorbell fan-out.
    ///
    /// Subscribers read this rather than each opening their own Observation
    /// stream. Translating changed Bodies into a dirty-set is per-*Observation*
    /// work, not per-subscriber work — and once that translation holds a diff
    /// baseline it MUST be, because two subscribers each advancing the same
    /// baseline would leave the second one seeing no change at all.
    observations: ObservationHub,
    /// The live task feeding the Observation fan-out, when one exists.
    ///
    /// The mutex is deliberately **held across startup**. A second subscriber
    /// waits until the winner has opened the Observation stream, so "ready" is
    /// the only state it can observe. Retaining the JoinHandle also lets
    /// shutdown signal and join this owner before Station dormancy.
    /// Control connections currently being served (idle-shutdown suppressor).
    active_conns: std::sync::atomic::AtomicU64,
    /// When the last control connection was accepted or completed — the idle
    /// clock's reference point.
    last_activity: Mutex<tokio::time::Instant>,
    /// The undrained tail of the Live plane's signal broadcast.
    ///
    /// Subscribed once, at activation, rather than per request: a signal is an
    /// event and not a state anyone can re-read, so a receiver created when
    /// somebody asks would only ever see what arrived after the asking.
    ///
    /// This *is* the bounded queue, and there is deliberately no second one in
    /// front of it. A tokio broadcast ring overwrites its **oldest** slot when
    /// it is full and tells the lagging reader how many it lost — which is
    /// exactly the rule a signal needs. A caret superseded by a newer caret has
    /// lost nothing; an invitation superseded by a ping has lost the
    /// invitation, so what goes is what somebody has had the longest chance to
    /// act on rather than what they have not seen yet.
    signals: SignalInbox,
}

struct ObservationHub {
    fanout: tokio::sync::broadcast::Sender<Doorbell>,
    pump: tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl ObservationHub {
    fn new() -> Self {
        Self {
            fanout: tokio::sync::broadcast::channel(256).0,
            pump: tokio::sync::Mutex::new(None),
        }
    }
}

struct SignalInbox {
    receiver: Mutex<tokio::sync::broadcast::Receiver<runtime::signal::DeliveredSignal>>,
}

impl SignalInbox {
    fn new(receiver: tokio::sync::broadcast::Receiver<runtime::signal::DeliveredSignal>) -> Self {
        Self {
            receiver: Mutex::new(receiver),
        }
    }

    fn drain(
        &self,
        mut actor_for: impl FnMut(&mechanics::station::Key) -> Option<String>,
    ) -> Response {
        use tokio::sync::broadcast::error::TryRecvError;
        let mut queue = self
            .receiver
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut signals = Vec::new();
        let mut dropped = 0u64;
        loop {
            match queue.try_recv() {
                Ok(delivered) => {
                    let Some(actor) = actor_for(&delivered.from) else {
                        continue;
                    };
                    signals.push(crate::control::SignalEntry {
                        actor,
                        connection_id: data_encoding::HEXLOWER.encode(&delivered.connection_id),
                        connection_epoch: data_encoding::HEXLOWER
                            .encode(&delivered.connection_epoch),
                        signal: signal_body(&delivered.signal),
                    });
                }
                Err(TryRecvError::Lagged(missed)) => dropped = dropped.saturating_add(missed),
                Err(TryRecvError::Empty | TryRecvError::Closed) => break,
            }
        }
        Response::Signals { signals, dropped }
    }
}

struct PrincipalFacts {
    actor: String,
    device: String,
}

fn response_message(response: Response) -> String {
    match response {
        Response::Error { message, .. } => message,
        other => format!("{other:?}"),
    }
}

struct StationHostActivity<'a>(&'a StationHost);

impl Drop for StationHostActivity<'_> {
    fn drop(&mut self) {
        use std::sync::atomic::Ordering;
        self.0.active_conns.fetch_sub(1, Ordering::SeqCst);
        if let Ok(mut activity) = self.0.last_activity.lock() {
            *activity = tokio::time::Instant::now();
        }
    }
}

/// An owned in-process StationHost lifecycle.
///
/// The runner holds the per-home daemon lock from before activation until after
/// the Station has gone dormant. It is joinable by its host; dropping or
/// aborting the task is not a successful shutdown path.
pub(crate) struct StationRunner {
    home: PathBuf,
    host: Arc<StationHost>,
    _lock: DaemonLock,
}

/// A non-owning signal for an in-process StationHost runner.
///
/// Weak by design: retaining a stop handle must not keep the host alive while
/// the runner waits for every control/session owner to drain.
#[derive(Clone)]
pub(crate) struct StationStop {
    host: std::sync::Weak<StationHost>,
}

impl StationStop {
    pub(crate) fn stop(&self) {
        if let Some(host) = self.host.upgrade() {
            host.begin_stop();
        }
    }
}

impl StationRunner {
    /// Acquire the Orbit's process-wide lease and activate its Station.
    pub(crate) async fn start(
        home: PathBuf,
        device_seed: [u8; 32],
        factory: &dyn TransportFactory,
        packages: WorldPackages,
    ) -> Result<Self> {
        let lock = acquire_daemon_lock(&home)?;
        let host = Arc::new(StationHost::open(&home, device_seed, factory, packages).await?);
        Ok(Self {
            home,
            host,
            _lock: lock,
        })
    }

    pub(crate) fn stop_handle(&self) -> StationStop {
        StationStop {
            host: Arc::downgrade(&self.host),
        }
    }

    pub(crate) fn host(&self) -> std::sync::Weak<StationHost> {
        Arc::downgrade(&self.host)
    }

    /// Serve until stopped, drain every host owner, return the Station to its
    /// Orbit, and only then release the per-home lease.
    pub(crate) async fn run(self) -> Result<()> {
        let serve_result = self.host.clone().serve().await;
        self.host.begin_stop();

        tokio::time::timeout(Duration::from_secs(5), async {
            while Arc::strong_count(&self.host) != 1 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .map_err(|_| {
            anyhow!(
                "StationHost clients did not drain during shutdown ({} owners remain)",
                Arc::strong_count(&self.host).saturating_sub(1)
            )
        })?;

        let Self { home, host, _lock } = self;
        let host = Arc::try_unwrap(host)
            .map_err(|_| anyhow!("StationHost still shared after client drain"))?;
        let dormancy_result = host.vacate();

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

impl StationHost {
    /// Open and activate the orbital stack for a home, then dock the routing
    /// Session. Refuses a pre-orbital home.
    pub async fn open(
        home: &Path,
        device_seed: [u8; 32],
        factory: &dyn TransportFactory,
        packages: WorldPackages,
    ) -> Result<Self> {
        if let Some(err) = unsupported_store_at(home) {
            return Err(anyhow!("{err}"));
        }
        let space = discover_space(home)?;
        let mechanics = SpaceAuthority::open(&orbital_store_root(home), &space, &device_seed)?;

        let (registry, worlds) = packages
            .build()
            .map_err(|e| anyhow!("world registry: {e:?}"))?;
        let rt = Runtime::open(
            orbital_store_root(home),
            registry,
            Arc::new(mechanics.clone()),
            Arc::new(mechanics.clone()),
        );

        let network = comms::policy::Network::from_env()?;
        let transport = factory
            .build_scoped(
                &device_seed,
                &network,
                comms::Protocols {
                    framed: &[
                        runtime::plane::contact::CONTACT_ALPN,
                        runtime::neighbor::PRESENCE_ALPN,
                    ],
                    session: &[runtime::plane::FREIGHT_ALPN, runtime::plane::LIVE_ALPN],
                },
                &space,
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
        let mut bootstrap: Vec<issues::ids::DeviceId> =
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
            runtime::neighbor::Catalog::load(&orbital_store_root(home).join(space.as_str()), &space)
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
            .acquire(&space)
            .map_err(|e| anyhow!("acquire orbit: {e:?}"))?
            .open(Activation {
                content: Default::default(),
                // Both planes on, which is what `lait/freight/1` being
                // advertised has always implied and, until now, has not meant:
                // the ALPN was registered and no driver owned it, so a peer
                // that dialled completed a handshake and was turned away.
                planes: Default::default(),
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
        for world in worlds.world_ids().cloned().collect::<Vec<_>>() {
            let _ = worlds.ensure_primary(&station, &world, &identity);
        }

        // The implementation self-check. Receipts pin whichever implementation
        // id is ACTIVE in the ledger — not this build's — so a build whose
        // descriptor has moved on would silently attest an implementation it
        // is not. Say so at open; `lait issues world-upgrade` (admin) activates this
        // build's id.
        {
            use runtime::world::AuthorityView;
            let device = mechanics::actor::device_from_seed(&device_seed);
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

        // Before the Station moves into the struct, and before anything can be
        // delivered: a receiver only holds what arrives after it exists.
        let signals = SignalInbox::new(station.signals());

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
            observations: ObservationHub::new(),
            active_conns: std::sync::atomic::AtomicU64::new(0),
            last_activity: Mutex::new(tokio::time::Instant::now()),
            signals,
        })
    }

    /// Ensure the primary identity has a Session for `world`, docking lazily
    /// once standing is held.
    fn ensure_world_session(&self, world: &WorldId) -> bool {
        self.worlds
            .ensure_primary(&self.station, world, &self.identity)
            .is_ok()
    }

    fn track_activity(&self) -> StationHostActivity<'_> {
        use std::sync::atomic::Ordering;
        self.active_conns.fetch_add(1, Ordering::SeqCst);
        StationHostActivity(self)
    }

    /// Direct host-local entry for a versioned product call.
    ///
    /// Owned Station placements invoke this without traversing the per-Orbit
    /// compatibility socket. The explicit Orbit/Space address is repeated and
    /// validated here because the host remains the terminal boundary owner.
    pub(crate) fn call_world(
        &self,
        address: &OrbitAddress,
        call: &Call,
        act_as: Option<&str>,
    ) -> Reply {
        let _activity = self.track_activity();
        if address != &self.address {
            return Reply::error(
                call,
                Code::InvalidCall,
                format!(
                    "World call targets Orbit {} in Space {}, but this host occupies \
                     Orbit {} in Space {}",
                    address.orbit, address.space, self.address.orbit, self.address.space
                ),
            );
        }
        self.route_world_call(call, act_as)
    }

    /// Direct host-local entry for a content call, minus the body.
    ///
    /// Blocking on purpose: every branch commits through the Replica's one
    /// writer, and a caller must be on a blocking thread before it gets here.
    /// The reason is the shared writer mutex, not the filesystem — holding it
    /// on a runtime thread stalls the Contact driver as surely as it stalls
    /// this call.
    pub(crate) fn content_call(
        &self,
        address: &OrbitAddress,
        call: &ContentCall,
        body: Option<UploadReader>,
    ) -> (ContentReply, Vec<u8>) {
        let _activity = self.track_activity();
        if address != &self.address {
            return (
                ContentReply::error(
                    ContentErrorCode::Invalid,
                    format!(
                        "content call targets Orbit {} in Space {}, but this host \
                         occupies Orbit {} in Space {}",
                        address.orbit, address.space, self.address.orbit, self.address.space
                    ),
                ),
                Vec::new(),
            );
        }
        match call {
            ContentCall::Stat { content } => {
                let Some(content) = parse_content_ref(content) else {
                    return (invalid_content_id(), Vec::new());
                };
                match self.station.content_stat(&self.identity, &content) {
                    Ok(status) => (
                        ContentReply::ContentStatus {
                            content: data_encoding::HEXLOWER.encode(content.as_bytes()),
                            plaintext_len: status.plaintext_len,
                            chunk_count: status.chunk_count,
                            resident_chunks: status.resident_chunks,
                            pinned: status.pinned,
                        },
                        Vec::new(),
                    ),
                    Err(error) => (content_refusal(&error), Vec::new()),
                }
            }
            ContentCall::Read {
                content,
                offset,
                len,
            } => {
                let Some(content) = parse_content_ref(content) else {
                    return (invalid_content_id(), Vec::new());
                };
                if *len > runtime::plane::freight::content::MAX_RANGE_BYTES as u64 {
                    return (
                        ContentReply::error(
                            ContentErrorCode::Bounds,
                            format!(
                                "one range may carry at most {} bytes",
                                runtime::plane::freight::content::MAX_RANGE_BYTES
                            ),
                        ),
                        Vec::new(),
                    );
                }
                match self
                    .station
                    .content_read(&self.identity, &content, *offset, *len as usize)
                {
                    Ok(bytes) => (
                        ContentReply::ContentStream {
                            len: bytes.len() as u64,
                        },
                        bytes,
                    ),
                    Err(error) => (content_refusal(&error), Vec::new()),
                }
            }
            ContentCall::Write { operation } => {
                let Some(operation) = parse_operation_id(operation) else {
                    return (
                        ContentReply::error(
                            ContentErrorCode::Invalid,
                            "an operation id is 16 bytes of lowercase hex",
                        ),
                        Vec::new(),
                    );
                };
                let Some(mut body) = body else {
                    return (
                        ContentReply::error(
                            ContentErrorCode::Invalid,
                            "a content write carries a body",
                        ),
                        Vec::new(),
                    );
                };
                match self
                    .station
                    .content_write(&self.identity, operation, &mut body)
                {
                    Ok(content) => {
                        let plaintext_len = self
                            .station
                            .content_stat(&self.identity, &content)
                            .map(|status| status.plaintext_len)
                            .unwrap_or_default();
                        (
                            ContentReply::ContentWritten {
                                content: data_encoding::HEXLOWER.encode(content.as_bytes()),
                                plaintext_len,
                            },
                            Vec::new(),
                        )
                    }
                    Err(error) => (content_refusal(&error), Vec::new()),
                }
            }
            ContentCall::Forget { content } => {
                let Some(content) = parse_content_ref(content) else {
                    return (invalid_content_id(), Vec::new());
                };
                match self.station.content_forget(&self.identity, &content) {
                    Ok(()) => (ContentReply::ContentForgotten, Vec::new()),
                    Err(error) => (content_refusal(&error), Vec::new()),
                }
            }
        }
    }

    /// The declared-length ceiling this Station will accept for one upload.
    pub(crate) fn max_content_len(&self) -> u64 {
        self.station.max_content_len()
    }

    /// Route a product call through its registered World package, or refuse
    /// with a typed "not admitted yet" when this device holds no standing.
    fn route_world_call(&self, call: &Call, act_as: Option<&str>) -> Reply {
        if let Err(error) = call.validate() {
            return Reply::error(call, error.code, error.message());
        }
        let world = call.world();
        let Some(host) = self.worlds.host(world) else {
            return Reply::error(
                call,
                Code::UnsupportedOperation,
                format!(
                    "World '{world}' is not enabled in Space {}",
                    self.address.space
                ),
            );
        };
        let Some(control) = host.control() else {
            return Reply::error(
                call,
                Code::UnsupportedOperation,
                format!("World '{world}' has no application call handler"),
            );
        };
        let access = match control.access(call) {
            Ok(access) => access,
            Err(error) => return Reply::error(call, error.code, error.message()),
        };

        // Resolve which local identity signs this request: the daemon's primary
        // (human) when no selector, else a sponsored local agent provisioned
        // under this home. One store, N signing identities (Architecture B).
        let seed = match self.acting_seed(act_as) {
            Ok(s) => s,
            Err(response) => return Reply::error(call, Code::Denied, response_message(response)),
        };
        let device = mechanics::actor::device_from_seed(&seed);

        // Hard partial-view guard (docs/plans/09 §10 finding 3): a *delegated*
        // identity — a sponsored agent — must not AUTHOR against a view it knows
        // is incomplete. "Close what's done" against a missing-epoch partial
        // view could act on issues it cannot see. A human acting for themselves
        // gets the loud `whoami`/`sync` signal and judges; an agent is stopped
        // by construction. Reads are always allowed (that is how it re-syncs).
        if access == Access::Command {
            use runtime::world::AuthorityView;
            if let Some(actor) = self.mechanics.resolve(&device).map(|r| r.actor) {
                if self.mechanics.is_agent(&actor) {
                    let divergence = self.mechanics.view_divergence();
                    if !divergence.is_empty() {
                        return Reply::error(
                            call,
                            Code::Denied,
                            format!(
                                "refusing to author against a partial view — {}. Run `sync` \
                                 until whole first; a delegated agent must not act on World \
                                 state it cannot see. Nothing was changed.",
                                divergence.join("; ")
                            ),
                        );
                    }
                }
            }
        }

        // The primary (human) uses the World-keyed primary Session; a sponsored
        // agent docks lazily under the same World, sharing the one Replica.
        if act_as.is_none() {
            if !self.ensure_world_session(world) {
                return Reply::error(
                    call,
                    Code::Unavailable,
                    "not admitted to this space yet — run `lait connect` to reach an admin \
                     and complete admission before using this World",
                );
            }
            self.worlds
                .with_primary(world, |session| {
                    self.call_with(control, call, session, &self.identity, &seed)
                })
                .unwrap_or_else(|| {
                    Reply::error(
                        call,
                        Code::Unavailable,
                        "the World Session became unavailable while handling the call",
                    )
                })
        } else {
            let identity = Runtime::identity_from_seed(&seed);
            match self
                .worlds
                .with_agent(&self.station, world, &identity, |session| {
                    self.call_with(control, call, session, &identity, &seed)
                }) {
                Ok(response) => response,
                Err(_) => Reply::error(
                    call,
                    Code::Denied,
                    "this agent identity holds no standing in the space yet — a human member \
                     must sponsor it (`lait members agent <key>`) before it can author. \
                     Nothing was changed.",
                ),
            }
        }
    }

    /// Route one product request through a docked Session for a specific
    /// identity. Product-specific resolution, minting, retry, and rendering are
    /// owned by the package's control adapter.
    fn call_with(
        &self,
        control: &dyn runtime::world::call::Handler,
        call: &Call,
        session: &Session,
        identity: &LocalIdentity,
        seed: &[u8; 32],
    ) -> Reply {
        let facts = self.facts_for(seed);
        let reply = control.call(
            call,
            &Context {
                session,
                identity,
                actor: &facts.actor,
                device: &facts.device,
            },
        );
        match reply.validate_for(call) {
            Ok(()) => {
                self.deliver_nudges(control.nudges(
                    call,
                    &reply,
                    &Context {
                        session,
                        identity,
                        actor: &facts.actor,
                        device: &facts.device,
                    },
                ));
                reply
            }
            Err(error) => Reply::error(call, error.code, error.message()),
        }
    }

    /// Which present peers each nudge reaches.
    ///
    /// Pulled out of `deliver_nudges` because this is the whole policy and the rest
    /// is plumbing: given who is here and who a World named, decide what goes where.
    /// A method on `StationHost` would need a Station, a transport and a docked
    /// Session to test three rules that need none of them.
    ///
    /// Actor equality and nothing looser. A nudge names one actor, and the only
    /// question is which of the sessions currently open belong to it — a peer with
    /// two devices is two Stations under one actor and hears once per device, which
    /// is what having two devices means.
    fn reachable<'a>(
        here: &'a [(mechanics::station::Key, String)],
        nudges: &'a [runtime::world::call::Nudge],
    ) -> Vec<(&'a mechanics::station::Key, &'a runtime::world::call::Nudge)> {
        nudges
            .iter()
            .flat_map(|nudge| {
                here.iter()
                    .filter(move |(_, actor)| actor == &nudge.actor)
                    .map(move |(station, _)| (station, nudge))
            })
            .collect()
    }

    /// Whether a World actually declared what its handler asked to send.
    ///
    /// `Nudge`'s own doc states this as a fact about the host, and for a
    /// while the host did not do it. A handler is ordinary product code: it can
    /// name a schema that was never registered, or build a payload past the
    /// ceiling its own registration declared, and neither is caught by anything
    /// between it and the wire.
    fn declares(
        &self,
        world: &replica::body::WorldId,
        nudge: &runtime::world::call::Nudge,
    ) -> bool {
        let Some(registration) = self.station.descriptor(world) else {
            return false;
        };
        let Some(schema) = replica::body::SchemaId::parse(&nudge.schema) else {
            return false;
        };
        registration
            .signal_schemas
            .iter()
            .find(|declared| declared.name == schema)
            .is_some_and(|declared| nudge.payload.len() <= declared.max_payload_bytes as usize)
    }

    /// Hand a World's nudges to whichever peers are actually here.
    ///
    /// **Presence is the gate, and it is better information than a preference
    /// pane.** Linear picks a channel from what a person configured months ago;
    /// this picks from whether they are looking at the product right now. A peer
    /// with no session is not queued for and not retried — the durable record is
    /// already committed and already converging, and it is their path.
    ///
    /// The World said who and what. This says whether they are reachable, which
    /// is the half a World must not know: one that could see who is connected
    /// would be a World holding a delivery plane.
    fn deliver_nudges(&self, nudges: Vec<runtime::world::call::Nudge>) {
        if nudges.is_empty() {
            return;
        }
        let live = self.station.live();
        // Resolved once. `actor_for` asks Mechanics per Station, and a fan-out
        // to a dozen followers would otherwise ask about the same peer a dozen
        // times.
        //
        // A Station that does not resolve is dropped rather than delivered to
        // under an invented identity — the same rule the live view follows.
        let here: Vec<(mechanics::station::Key, String)> = live
            .present_stations()
            .into_iter()
            .filter_map(|station| {
                let actor = self.actor_for(&station)?;
                Some((station, actor))
            })
            .collect();
        let world_id = crate::world::contract::world_id();
        let world = world_id.as_str().to_string();
        for (station, nudge) in Self::reachable(&here, &nudges) {
            if !self.declares(&world_id, nudge) {
                // Refused here rather than sent for the peer to refuse. The
                // receiver checks the same thing — a signal naming a schema its
                // World never declared is one nobody reviewed — so sending it
                // spends an outbox slot and a flow to be told what this side
                // already knew, and a World whose handler is wrong would spend
                // them on every call.
                continue;
            }
            let signal = runtime::plane::Signal::WorldSignal {
                world: world.clone(),
                schema: nudge.schema.clone(),
                payload: nudge.payload.clone(),
            };
            // A full outbox is not reported to anyone. The record behind every
            // nudge is durable, so a refused one costs timeliness and nothing
            // else — and telling the *sender* would be telling them something
            // about the receiver's queue.
            live.nudge(station, signal);
        }
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

    /// Principal facts for a specific local identity seed.
    fn facts_for(&self, seed: &[u8; 32]) -> PrincipalFacts {
        use runtime::world::AuthorityView;
        let device = mechanics::actor::device_from_seed(seed);
        let actor = self
            .mechanics
            .resolve(&device)
            .map(|r| r.actor.as_str().to_string())
            .unwrap_or_default();
        PrincipalFacts {
            device: device.as_str().to_string(),
            actor,
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
    /// request. A missing route remains valid for Space-owned requests accepted
    /// directly by a StationHost.
    fn validate_route(
        &self,
        route: Option<&ControlRoute>,
        _owner: RequestOwner,
    ) -> std::result::Result<(), Response> {
        let Some(route) = route else {
            return Ok(());
        };
        let actual_space = self.station.space_id();
        let wrong_space = |space: &mechanics::ids::SpaceId| {
            Response::err(format!(
                "request targets Space {space}, but this host owns {actual_space}"
            ))
        };
        let wrong_orbit = |address: &OrbitAddress| {
            Response::err(format!(
                "request targets local Orbit {}, but this host occupies {}",
                address.orbit, self.address.orbit
            ))
        };
        match route {
            ControlRoute::Daemon => {
                Err(Response::err("daemon-scoped request reached a StationHost"))
            }
            ControlRoute::Orbit { address } => {
                if address.orbit != self.address.orbit {
                    return Err(wrong_orbit(address));
                }
                if &address.space != actual_space {
                    return Err(wrong_space(&address.space));
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
                let Some(world_id) = WorldId::parse(world) else {
                    return Err(Response::err(format!("invalid World id '{world}'")));
                };
                if !self.worlds.contains(&world_id) {
                    return Err(Response::err(format!(
                        "World '{world}' is not enabled in Space {actual_space}"
                    )));
                }
                Err(Response::err(
                    "control requests cannot be sent through a WorldHost; \
                     send a versioned World call",
                ))
            }
        }
    }

    /// Ring the authority plane, if there is a docked Session to ring through.
    /// Before admission there is no Session and nothing is subscribed anyway.
    fn publish_authority_advanced(&self) {
        let docked = self
            .worlds
            .world_ids()
            .any(|world| self.ensure_world_session(world));
        if !docked {
            return;
        }
        self.worlds.with_any_primary(|session| {
            session.publish_authority_advanced();
        });
    }

    /// Membership, admission, device, key, ceremony and custody requests —
    /// served by [`SpaceAuthority`] over the mechanics primitives.
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
            Request::WorldActivate { world } => {
                let Some(world_id) = WorldId::parse(&world) else {
                    return Response::err("invalid World id");
                };
                let Some(host) = self.worlds.host(&world_id) else {
                    return Response::not_found(format!("World '{world}' is not hosted"));
                };
                let ours = *host.reviewed_implementation();
                match self
                    .mechanics
                    .activate_implementation(world_id.as_str(), ours)
                {
                    Ok(()) => Response::Ok {
                        message: Some(format!(
                            "implementation {} is active for {} (no-op if it already was)",
                            data_encoding::HEXLOWER.encode(&ours[..8]),
                            world_id,
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
                let device = mechanics::actor::device_from_seed(&self.device_seed).to_string();
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
            Request::AssignmentList { actor } => {
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
            Request::AssignmentGrant { actor, assignments } => {
                let Some(subject) = self.mechanics.resolve_actor_ref(&actor) else {
                    return Response::not_found(format!("no actor matches '{actor}'"));
                };
                let assignments = assignments
                    .into_iter()
                    .map(|assignment| {
                        (
                            mechanics::authorization::PolicyCapability::new(
                                &assignment.world,
                                &assignment.capability,
                            ),
                            mechanics::authorization::Resource {
                                world: assignment.world,
                                segments: assignment.resource,
                            },
                        )
                    })
                    .collect::<Vec<_>>();
                match self.mechanics.grant_assignments(&subject, &assignments) {
                    Ok(granted) => Response::Ok {
                        message: Some(format!(
                            "installed {} assignment(s) for {}",
                            granted.len(),
                            subject.short()
                        )),
                    },
                    Err(error) => Response::err(format!("{error}")),
                }
            }
            Request::AssignmentRevoke { grant_id } => {
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
            other => Response::err(format!("request is not a Mechanics operation: {other:?}")),
        }
    }

    /// Publish what this node is looking at, so peers can draw a face.
    ///
    /// The send side of the Live plane, and the only thing that puts this
    /// Station *into* anybody else's transient table. Without it every viewer
    /// renders other people and nobody renders this one.
    ///
    /// Replace-all: the declaration is what is on screen now, so a scope that has
    /// left the set is retired on every peer rather than left to expire.
    ///
    /// **Truncated at the subscription ceiling rather than sent whole.** A peer
    /// refuses an entire `Subscribe` frame carrying more scopes than a connection
    /// may hold, so one declaration past the cap does not lose the excess — it
    /// loses *everything*, silently, on every peer, until the declaration
    /// changes. Somebody with a hundred tabs open would simply stop existing.
    ///
    /// A doc id is not validated here and cannot be: the Body id is a hash of the
    /// string as given, so every string is a legal input and an unresolvable one
    /// names a Body nothing publishes under. That is an empty answer rather than
    /// an error, and it is what a stale link should get.
    fn watching(
        &self,
        issues: &[String],
        carets: &[crate::control::WatchingCaret],
        typing: &[crate::control::WatchingTyping],
    ) -> Response {
        use runtime::plane::live::LocalPublication;
        use runtime::transient::{Target, TransientPayload};

        let world = crate::world::contract::world_id().as_str().to_string();
        // Cursor state is the most perishable declaration, so admit it before
        // the passive Body scopes. A tab collection at the subscription ceiling
        // should lose an old facepile entry before it loses the caret currently
        // moving under somebody's hands.
        let mut candidates = Vec::new();
        let issue_set: std::collections::BTreeSet<_> = issues.iter().map(String::as_str).collect();
        for caret in carets {
            if !issue_set.contains(caret.issue.as_str()) || caret.field != "description" {
                continue;
            }
            let body = crate::world::contract::issue_body_id(&caret.issue).as_bytes();
            let Some(anchor) = self
                .station
                .live()
                .anchor(&world, body, &caret.field, caret.anchor)
            else {
                continue;
            };
            let payload = match caret.focus.filter(|focus| *focus != caret.anchor) {
                Some(focus) => {
                    let Some(focus) = self
                        .station
                        .live()
                        .anchor(&world, body, &caret.field, focus)
                    else {
                        continue;
                    };
                    TransientPayload::Selection { anchor, focus }
                }
                None => TransientPayload::Caret { anchor },
            };
            candidates.push(LocalPublication {
                scope: Target::Field {
                    world: world.clone(),
                    body,
                    field: caret.field.clone(),
                },
                payload,
            });
        }
        for active in typing {
            if !issue_set.contains(active.issue.as_str()) || active.field != "description" {
                continue;
            }
            candidates.push(LocalPublication {
                scope: Target::Typing {
                    world: world.clone(),
                    body: crate::world::contract::issue_body_id(&active.issue).as_bytes(),
                    field: active.field.clone(),
                },
                payload: TransientPayload::Typing,
            });
        }
        candidates.extend(issues.iter().map(|doc| LocalPublication {
            scope: Target::Body {
                world: world.clone(),
                body: crate::world::contract::issue_body_id(doc).as_bytes(),
            },
            payload: TransientPayload::Presence,
        }));

        // A subscription is counted by Target, not by payload. Keep at most one
        // publication for each scope (the first browser session wins when this
        // Station has two tabs in one field) and never let distinct scopes cross
        // the wire ceiling. The receiver rejects an oversize set as a whole.
        let mut admitted = std::collections::BTreeSet::new();
        let mut publications = Vec::new();
        for publication in candidates {
            if admitted.contains(&publication.scope) {
                continue;
            }
            if admitted.len() >= runtime::transient::MAX_SUBSCRIPTIONS {
                break;
            }
            admitted.insert(publication.scope.clone());
            publications.push(publication);
        }
        publications.sort_by(|a, b| {
            (&a.scope, a.payload.kind() as u8).cmp(&(&b.scope, b.payload.kind() as u8))
        });
        self.station.live().declare_local_publications(publications);
        Response::Ok { message: None }
    }

    /// Connect/neighbor/Contact requests — served by the Station.
    fn dispatch_station(&self, req: Request) -> Response {
        match req {
            Request::Connect { ticket } => self.connect(&ticket),
            Request::Who => Response::Who { peers: self.who() },
            Request::Live {
                since_generation,
                issue,
            } => self.live(since_generation, issue.as_deref()),
            Request::Watching {
                issues,
                carets,
                typing,
            } => self.watching(&issues, &carets, &typing),
            Request::Signals => self.drain_signals(),
            Request::Sync => self.sync(),
            other => Response::err(format!("request is not a Station operation: {other:?}")),
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
        let device = mechanics::actor::device_from_seed(&seed);
        let did = mechanics::actor::did_key_from_device(&device);
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
        Response::Whoami(issues::dto::WhoamiDto {
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
                let device = mechanics::actor::device_from_seed(&seed);
                let did = mechanics::actor::did_key_from_device(&device).unwrap_or_default();
                // Name it here, where the name is already known. Without this the
                // one command that creates an agent leaves it `unnamed` on the one
                // surface built to show agents — and the browser draws a coding
                // tool's brand mark off this petname, so an unnamed agent is also
                // an unrecognisable one.
                let _ = write_alias(&self.home, actor.as_str(), name);
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
                let online = n.reachability == runtime::neighbor::Reachability::Reachable;
                let state = match n.reachability {
                    runtime::neighbor::Reachability::Reachable => "online",
                    runtime::neighbor::Reachability::Unreachable => "offline",
                    runtime::neighbor::Reachability::Unknown => "away",
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

    /// The actor a Station speaks for, or `None`.
    ///
    /// The Station's own authority view answers it — the same question the Live
    /// plane's admission asked before it accepted the session. A peer that lost
    /// standing between admission and this read resolves to nothing and its row
    /// disappears, which is the right outcome for a surface whose entire subject
    /// is who is here right now.
    ///
    /// The device id is never a fallback. `PresenceEntry.id` is a device and
    /// `MemberDto.key` is an actor; the viewer colours an avatar by hashing
    /// whatever string it is handed, so one person on two devices would arrive
    /// as two people in two colours.
    fn actor_for(&self, station: &Key) -> Option<String> {
        use runtime::world::AuthorityView;
        self.mechanics
            .admit_peer(station)
            .map(|resolution| resolution.actor.as_str().to_string())
    }

    /// What this Station currently believes about who is doing what.
    ///
    /// Modelled on [`Self::who`] and reporting a different truth: `who` is the
    /// durable Neighbor registry's reachability, this is the Live plane's
    /// transient table, which nothing journals and nothing replays.
    fn live(&self, since_generation: Option<u64>, issue: Option<&str>) -> Response {
        let handle = self.station.live();
        // The issue's Body id, by the same one-way derivation the Issues World
        // commits under. It only runs this direction, which is why an unscoped
        // read hands back Body ids a browser cannot name.
        let want = issue.map(|doc| {
            (
                crate::world::contract::world_id().as_str().to_string(),
                crate::world::contract::issue_body_id(doc).as_bytes(),
            )
        });
        // `LiveNarrow::Body` and never a scope. Scope narrowing is *equality*,
        // and an issue's caret and typing rows are `Field` and `Typing` over
        // the same Body — so asking for `Body` returns the presence rows
        // and silently drops the two a caret surface exists to draw.
        let narrow = match &want {
            Some((world, body)) => runtime::plane::live::LiveNarrow::Body { world, body: *body },
            None => runtime::plane::live::LiveNarrow::Everything,
        };
        let view = handle.view_narrowed(narrow, tokio::time::Instant::now());
        let entries: Vec<_> = view.entries.iter().collect();

        // Equality and never an ordering: the counter wraps. It is also not the
        // whole answer — `uncertain` is derived per read from a slot's age and
        // nothing bumps the counter when one crosses the grace window, so a
        // caller told "unchanged" would draw a caret as certain until the slot
        // expired half a minute later. Once every row is already uncertain
        // nothing more can flip, and the cheap answer is true again.
        if since_generation == Some(view.generation) && entries.iter().all(|entry| entry.uncertain)
        {
            return Response::LiveUnchanged {
                generation: view.generation,
            };
        }
        Response::Live {
            generation: view.generation,
            partial: view.partial,
            entries: entries
                .into_iter()
                .filter_map(|entry| self.live_entry(entry))
                .collect(),
        }
    }

    /// One transient row, or nothing when its Station names no actor here.
    fn live_entry(
        &self,
        entry: &runtime::plane::live::LiveEntry,
    ) -> Option<crate::control::LiveEntry> {
        Some(crate::control::LiveEntry {
            actor: self.actor_for(&entry.station)?,
            scope: live_scope(&entry.scope),
            kind: transient_kind(entry.kind).to_string(),
            age_ms: entry.age_ms,
            uncertain: entry.uncertain,
            caret: entry.caret.map(caret_position),
            focus: entry.focus.map(caret_position),
        })
    }

    /// Take everything the signal queue holds and leave it empty.
    ///
    /// The loop does not stop at a lag. A full ring overwrote its oldest slots
    /// and moved this reader past them; what follows is still there and still
    /// worth handing over, so the count is accumulated and the drain continues.
    fn drain_signals(&self) -> Response {
        self.signals.drain(|station| self.actor_for(station))
    }

    /// The one number both `status` and `who` report as "online".
    fn online_peers(&self) -> usize {
        self.station
            .neighbors()
            .iter()
            .filter(|n| n.reachability == runtime::neighbor::Reachability::Reachable)
            .count()
    }

    /// Generic status and subscription projection surfaces.
    fn dispatch_observation(&self, req: Request) -> Response {
        match req {
            Request::Status => self.status(),
            // Subscribe is handled by the streaming connection path before
            // dispatch; a one-shot Subscribe cannot be answered on this plane.
            Request::Subscribe { .. } => Response::err("subscribe is a streaming request"),
            other => Response::err(format!(
                "request is not an observation operation: {other:?}"
            )),
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
            // The StationHost has no legacy in-memory event ring — live
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
            other => Response::err(format!("request is not a lifecycle operation: {other:?}")),
        }
    }

    /// The (issues, projects) counts from the docked Session's catalog
    /// snapshot — `None` when the projection is UNAVAILABLE (undocked, or a
    /// query failed). Status reports the truth; it never converts an
    /// unavailable projection into false zeros.
    fn counts(&self) -> Option<(usize, usize, String, String)> {
        // A Station can start before this device is admitted. Once authority
        // advances, status must retry lazy docking; otherwise every projector
        // remains absent even after its replicated Bodies arrive.
        let docked = self
            .worlds
            .world_ids()
            .any(|world| self.ensure_world_session(world));
        if !docked {
            return None;
        }
        self.worlds
            .status()
            .map(|status| (status.items, status.groups, status.name, status.description))
    }

    fn status(&self) -> Response {
        let counts = self.counts();
        let (issues, projects, name, description) =
            counts
                .clone()
                .unwrap_or((0, 0, String::new(), String::new()));
        Response::Status(Box::new(StatusInfo {
            id: mechanics::actor::device_from_seed(&self.device_seed).to_string(),
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
        let binding: mechanics::actor::DeviceBinding = match data_encoding::HEXLOWER_PERMISSIVE
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
        let me = mechanics::actor::device_from_seed(&self.device_seed);
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
        // Store the canonical actor id, never the fragment that was typed. The
        // read path matches a stored key against the *full* `act_…` id, so an
        // entry keyed by a bare hex prefix can never match anything: the write
        // succeeds, the confirmation prints, and the name never appears. Resolve
        // first, and say so when nothing answers.
        let resolved = self.resolve_member_key(who);
        let key = match (&resolved, name.trim().is_empty()) {
            (Some(k), _) => k.clone(),
            // Clearing may legitimately target an entry no longer backed by a
            // member — including a dead one written before this resolved.
            (None, true) => who.trim().to_string(),
            (None, false) => {
                return Response::err(format!(
                    "no member here matches '{who}' — name one by its full actor id or a \
                     prefix of it (`lait members` lists them)"
                ))
            }
        };
        match write_alias(&self.home, &key, name) {
            Ok(()) if name.trim().is_empty() => Response::Ok {
                message: Some(format!("cleared the local name for {who}")),
            },
            Ok(()) => Response::Ok {
                message: Some(format!("{who} is now locally known as {name}")),
            },
            Err(e) => Response::err(format!("set alias: {e}")),
        }
    }

    /// The full actor id for a who-ref: an exact key, a prefix of one (with or
    /// without the `act_` namespace), or an existing petname.
    fn resolve_member_key(&self, who: &str) -> Option<String> {
        let who = who.trim();
        if who.is_empty() {
            return None;
        }
        let members = self.mechanics.members();
        let bare = who.strip_prefix("act_").unwrap_or(who);
        let prefixed = format!("act_{bare}");
        if let Some(m) = members
            .iter()
            .find(|m| m.key == who || m.key.starts_with(who) || m.key.starts_with(&prefixed))
        {
            return Some(m.key.clone());
        }
        let aliases = read_aliases(&self.home);
        aliases
            .iter()
            .find(|(_, v)| v.eq_ignore_ascii_case(who))
            .map(|(k, _)| k.clone())
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
            local_custody: Some(&recovery.custody),
        });
        Response::Diagnosis(Box::new(view))
    }

    /// Pin a bootstrap seed by device id (or an orbital Coordinates link's
    /// approach Station) into the node-local registry.
    fn seed_add(&self, arg: &str) -> Response {
        let (id, space) = match issues::ids::DeviceId::parse(arg.trim()) {
            Some(id) => (id, String::new()),
            None => match runtime::coordinates::SignedCoordinates::parse_link(arg.trim())
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
                issues::dto::SeedDto {
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
        use mechanics::recovery::{SpaceRecovered, SpaceRecovery};
        match self.mechanics.space_recover() {
            Ok(SpaceRecovery::Installed(SpaceRecovered { root, completion })) => {
                let head = format!("recovered the space — root reset to {}", root.short());
                Response::Ok {
                    message: Some(match completion {
                        Completion::Complete => format!("{head} and re-keyed"),
                        Completion::InstalledButUnfinished(_) => format!(
                            "{head}, but re-keying did not finish — the space is still readable \
                             under the old key until an admin rotates it"
                        ),
                    }),
                }
            }
            Ok(SpaceRecovery::Pending {
                session,
                completion,
            }) => {
                let hex = session.to_hex();
                let head = format!(
                    "group recovery under way (session {hex}) — each other holder must approve \
                     it with `space recover-approve {hex}` until the threshold co-signs"
                );
                Response::Ok {
                    message: Some(match completion {
                        Completion::Complete => head,
                        Completion::InstalledButUnfinished(_) => format!(
                            "{head}. This device could not add its own share; the request \
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
                    message: Some(match a.completion {
                        Completion::Complete => format!(
                            "co-signed the recovery re-rooting the space to {roots} — it installs \
                             once the threshold has co-signed"
                        ),
                        Completion::InstalledButUnfinished(_) => format!(
                            "co-signed the recovery re-rooting the space to {roots}, and that \
                             completed the threshold — but re-keying did not finish, so the space \
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
                let message = match (e.grant_request, e.completion) {
                    (_, Completion::InstalledButUnfinished(_)) => format!(
                        "proposed a {}-of-{} recovery arrangement (proposal {}) — but this device \
                         could not carry it further; the proposal stands and can still \
                         be authorized",
                        e.k,
                        e.n,
                        e.proposal.to_hex()
                    ),
                    (None, Completion::Complete) => format!(
                        "started {}-of-{} recovery elevation — the DKG completes automatically as \
                         the co-founders' nodes sync; the group key installs once every share is in",
                        e.k, e.n
                    ),
                    (Some(signing), Completion::Complete) => format!(
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
                let message = match (e.grant_request, e.completion) {
                    (_, Completion::InstalledButUnfinished(_)) => format!(
                        "proposed resharing the recovery key onto a {}-of-{} arrangement \
                         (proposal {}) — but this device could not carry it further; \
                         the proposal stands and can still be authorized",
                        e.k,
                        e.n,
                        e.proposal.to_hex()
                    ),
                    (Some(signing), Completion::Complete) => format!(
                        "proposed resharing the recovery key onto a {}-of-{} arrangement \
                         (proposal {}) — the current group must authorize it: each holder runs \
                         `space elevate-approve {} --proposal {}`. The key itself does not change.",
                        e.k,
                        e.n,
                        e.proposal.to_hex(),
                        signing.to_hex(),
                        e.proposal.to_hex(),
                    ),
                    (None, Completion::Complete) => format!(
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
                    message: Some(match i.completion {
                        Completion::Complete => head,
                        Completion::InstalledButUnfinished(_) => format!(
                            "{head}. The ceremony did not advance here; it will retry on \
                             the next sync"
                        ),
                    }),
                }
            }
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
        let station = match issues::ids::DeviceId::parse(link).and_then(|d| Key::from_device(&d)) {
            Some(station) => Some(station),
            None => runtime::coordinates::SignedCoordinates::parse_link(link)
                .ok()
                .and_then(|c| c.verify().ok())
                .and_then(|v| {
                    if !v.approach_routes.is_empty() {
                        self.transport
                            .learn(v.approach_station.clone(), &v.approach_routes);
                    }
                    Key::from_device(&v.approach_station)
                }),
        };
        match station {
            Some(station) => match self.station.contact(&station) {
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
            "Station host online in space {}",
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
                            "orbital store at {} is gone — the StationHost will not \
                             outlive its store; stopping",
                            self.store_dir().display()
                        );
                        self.begin_stop();
                        break;
                    }
                    if self.should_idle_shutdown(idle_window) {
                        tracing::info!("Station host idle-shutdown after {idle_window:?}");
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
        // Wake and join every task retaining the host before the runner tries
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
                "StationHost control connections did not drain during shutdown ({} remain)",
                connections.len()
            )
        })?;
        let pump = self.observations.pump.lock().await.take();
        if let Some(pump) = pump {
            tokio::time::timeout(Duration::from_secs(5), pump)
                .await
                .map_err(|_| anyhow!("StationHost Observation pump did not drain during shutdown"))?
                .map_err(|error| anyhow!("StationHost Observation pump failed: {error}"))?;
        }
        Ok(())
    }

    /// Consume the host and return its Station to a durable Orbit.
    ///
    /// World Sessions are dropped first so dormancy can reject all future
    /// callbacks, drain Station tasks, and release the store lock last.
    pub fn vacate(self) -> Result<()> {
        let Self {
            station, worlds, ..
        } = self;
        drop(worlds);
        station
            .vacate()
            .map(|_| ())
            .map_err(|e| anyhow!("StationHost dormancy failed: {e:?}"))
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

    /// Serve one content call: its body in, its answer out.
    ///
    /// Three things have to happen in the right order. The declared length is
    /// checked against operator policy **before** a byte is read, because
    /// reading first and refusing after is a free way to make this process
    /// spend a Station's disk budget. The body is read through the same
    /// `BufReader` that consumed the header, because that reader already holds
    /// the first bytes of the body. And the sealing runs on a blocking thread,
    /// because it takes the Replica's one writer.
    async fn serve_content(
        self: Arc<Self>,
        reader: BufReader<tokio::io::ReadHalf<LocalStream>>,
        mut write_half: tokio::io::WriteHalf<LocalStream>,
        request: crate::control::ContentClientRequest,
    ) {
        let ceiling = self.max_content_len();
        if request.body_len > ceiling {
            // Refused without reading the body, so the channel is out of step
            // and the connection ends here. One connection is one request, so
            // there is nothing to resynchronise for.
            let _ = write_line_half(
                &mut write_half,
                &ContentReply::error(
                    ContentErrorCode::Bounds,
                    format!(
                        "this Station accepts at most {ceiling} bytes in one content; \
                         the request declared {}",
                        request.body_len
                    ),
                ),
            )
            .await;
            return;
        }
        let address = match &request.route {
            ControlRoute::Orbit { address } | ControlRoute::World { address, .. } => {
                address.clone()
            }
            ControlRoute::Daemon => {
                let _ = write_line_half(
                    &mut write_half,
                    &ContentReply::error(
                        ContentErrorCode::Invalid,
                        "a content call requires an explicit Space route",
                    ),
                )
                .await;
                return;
            }
        };

        let expects_body = matches!(request.content, crate::control::ContentCall::Write { .. });
        let (body, pump) = crate::control::upload_body(reader, request.body_len);
        let host = self.clone();
        let call = request.content.clone();
        let work = tokio::task::spawn_blocking(move || {
            host.content_call(&address, &call, expects_body.then_some(body))
        });
        // The pump has to run while the sealer consumes it, and the sealer is
        // on another thread: awaiting them in sequence would deadlock at the
        // first full channel.
        let mut stopping = self.stop_tx.subscribe();
        let (_, sealed) = tokio::join!(pump, work);
        let (reply, payload) = match sealed {
            Ok(answer) => answer,
            Err(_) => (
                ContentReply::error(ContentErrorCode::Storage, "the content call did not finish"),
                Vec::new(),
            ),
        };
        // A stop that landed mid-upload is reported rather than answered, so a
        // caller does not read "written" from a process that is going away.
        if *stopping.borrow_and_update() && !matches!(reply, ContentReply::ContentError { .. }) {
            let _ = write_line_half(
                &mut write_half,
                &ContentReply::error(ContentErrorCode::Storage, "this Space is shutting down"),
            )
            .await;
            return;
        }
        if write_line_half(&mut write_half, &reply).await.is_err() {
            return;
        }
        if !payload.is_empty() {
            let _ = write_half.write_all(&payload).await;
            let _ = write_half.flush().await;
        }
    }

    async fn handle_conn(self: Arc<Self>, stream: LocalStream) {
        let _activity = self.track_activity();

        let (read_half, write_half) = tokio::io::split(stream);
        let mut reader = BufReader::new(read_half);
        let mut line = String::new();
        {
            // Bounded for the same reason the daemon's is: a header is a
            // bounded thing, and an unbounded `read_line` is a memory attack
            // that needs no authorization.
            use tokio::io::AsyncReadExt;
            let mut bounded = (&mut reader).take(crate::control::MAX_CONTROL_LINE_BYTES);
            if bounded.read_line(&mut line).await.is_err() {
                return;
            }
        }
        let value = match serde_json::from_str::<serde_json::Value>(line.trim()) {
            Ok(value) => value,
            Err(error) => {
                let _ =
                    write_line(write_half, &Response::err(format!("bad request: {error}"))).await;
                return;
            }
        };
        if value.get("content").is_some() {
            let request: crate::control::ContentClientRequest = match serde_json::from_value(value)
            {
                Ok(request) => request,
                Err(error) => {
                    let _ = write_line(
                        write_half,
                        &ContentReply::error(
                            ContentErrorCode::Invalid,
                            format!("bad content call: {error}"),
                        ),
                    )
                    .await;
                    return;
                }
            };
            // `self` is cloned rather than moved: the activity guard taken at
            // the top of this function borrows it for the whole call, and
            // dropping it late is what keeps the idle timer honest about a
            // long upload.
            self.clone()
                .serve_content(reader, write_half, request)
                .await;
            return;
        }
        if value.get("call").is_some() {
            let crate::control::WorldClientRequest {
                route,
                act_as,
                call,
            } = match serde_json::from_value(value) {
                Ok(request) => request,
                Err(error) => {
                    let _ = write_line(
                        write_half,
                        &Response::err(format!("bad World call: {error}")),
                    )
                    .await;
                    return;
                }
            };
            let reply = match route {
                ControlRoute::World { address, world } => {
                    let route_world = WorldId::parse(&world);
                    if route_world.as_ref() != Some(call.world()) {
                        Reply::error(
                            &call,
                            Code::InvalidCall,
                            format!(
                                "World route addresses '{world}', but the call addresses '{}'",
                                call.world()
                            ),
                        )
                    } else {
                        self.call_world(&address, &call, act_as.as_deref())
                    }
                }
                _ => Reply::error(
                    &call,
                    Code::InvalidCall,
                    "World call requires an explicit World route",
                ),
            };
            let _ = write_line(write_half, &reply).await;
            return;
        }
        let crate::control::ClientRequest {
            route,
            if_running: _,
            act_as,
            request: req,
        } = match serde_json::from_value::<crate::control::ClientRequest>(value) {
            Ok(env) => env,
            Err(e) => {
                let _ = write_line(write_half, &Response::err(format!("bad request: {e}"))).await;
                return;
            }
        };
        // Validate before handling either control-flow request. In particular,
        // an invalidly routed Stop must not tear down this host, and a
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

    /// Start the single Observation pump if it is not already running.
    ///
    /// One stream, one translation, one fan-out. The
    /// pump owns the blocking iterator through a TRACKED blocking task that
    /// blocks in bounded windows and re-checks cancellation between them; the
    /// async side selects on that channel and the teardown watch, so a Stop
    /// wakes it immediately and the worker exits within one window.
    async fn ensure_doorbell_pump(self: &Arc<Self>) {
        use std::sync::atomic::Ordering;
        // Held for the whole of startup: a caller that arrives mid-start blocks
        // here and returns only once the stream is open, never in between.
        let mut running = self.observations.pump.lock().await;
        if let Some(task) = running.as_ref() {
            if !task.is_finished() {
                return;
            }
        }
        if let Some(finished) = running.take() {
            let _ = finished.await;
        }
        // Every package seeds its own projection baseline before the stream
        // opens, so product state never lives on the generic Station host.
        self.worlds.start_projectors(self.station.space_id());
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
            let (tx, mut rx) = tokio::sync::mpsc::channel::<runtime::world::Observation>(64);
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
                        let _ = daemon.observations.fanout.send(daemon.frame_for(&record));
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
    fn frame_for(&self, record: &runtime::world::Observation) -> Doorbell {
        let projected = self.worlds.project(self.station.space_id(), record);
        let dirty_by_project = projected.dirty_by_project;
        let dirty_catalog = projected.dirty_catalog;
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
        let mut frames = self.observations.fanout.subscribe();
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

/// Render a Body id the way the rest of the tree renders one.
///
/// The raw 16 bytes would arrive over this channel as a JSON list of sixteen
/// numbers, which is a shape nothing else here uses and nothing can compare
/// against a Body id printed anywhere else.
fn render_body(body: &[u8; 16]) -> String {
    replica::body::BodyId::from_bytes(*body).render()
}

fn live_scope(scope: &runtime::transient::Target) -> crate::control::LiveScope {
    use crate::control::LiveScope;
    use runtime::transient::Target;
    match scope {
        Target::Body { world, body } => LiveScope::Body {
            world: world.clone(),
            body: render_body(body),
        },
        Target::Material { world, body } => LiveScope::Material {
            world: world.clone(),
            body: render_body(body),
        },
        Target::Field { world, body, field } => LiveScope::Field {
            world: world.clone(),
            body: render_body(body),
            field: field.clone(),
        },
        Target::Typing { world, body, field } => LiveScope::Typing {
            world: world.clone(),
            body: render_body(body),
            field: field.clone(),
        },
        Target::Content { content } => LiveScope::Content {
            content: data_encoding::HEXLOWER.encode(content),
        },
        Target::World { world, schema, key } => LiveScope::World {
            world: world.clone(),
            schema: schema.clone(),
            key: key.clone(),
        },
    }
}

fn caret_position(state: runtime::plane::live::CaretState) -> crate::control::CaretPosition {
    use crate::control::CaretPosition;
    use runtime::plane::live::CaretState;
    match state {
        CaretState::At(position) => CaretPosition::At { position },
        CaretState::Drifted => CaretPosition::Drifted,
        CaretState::Unresolved => CaretPosition::Unresolved,
    }
}

fn transient_kind(kind: runtime::transient::TransientKind) -> &'static str {
    use runtime::transient::TransientKind;
    match kind {
        TransientKind::Presence => "presence",
        TransientKind::Caret => "caret",
        TransientKind::Selection => "selection",
        TransientKind::Typing => "typing",
        TransientKind::Residency => "residency",
    }
}

fn signal_body(signal: &runtime::plane::Signal) -> crate::control::SignalBody {
    use crate::control::SignalBody;
    use runtime::plane::{InviteKind, Signal};
    match signal {
        Signal::Ping { nonce } => SignalBody::Ping {
            nonce: data_encoding::HEXLOWER.encode(nonce),
        },
        Signal::Acknowledge { nonce } => SignalBody::Acknowledge {
            nonce: data_encoding::HEXLOWER.encode(nonce),
        },
        Signal::Attention { scope } => SignalBody::Attention {
            scope: live_scope(scope),
        },
        Signal::SessionInvite { kind, scope } => SignalBody::SessionInvite {
            invite: match kind {
                InviteKind::Collaborate => "collaborate".into(),
            },
            scope: live_scope(scope),
        },
        Signal::FileOffer {
            content,
            plaintext_len,
            display_name,
            media_type,
        } => SignalBody::FileOffer {
            content: data_encoding::HEXLOWER.encode(content),
            plaintext_len: *plaintext_len,
            display_name: display_name.clone(),
            media_type: media_type.clone(),
        },
        Signal::WorldSignal {
            world,
            schema,
            payload,
        } => SignalBody::WorldSignal {
            world: world.clone(),
            schema: schema.clone(),
            payload_b64: data_encoding::BASE64.encode(payload),
        },
    }
}

/// Unix seconds now. Delegates to `mechanics::wallclock` so tests can
/// freeze it, and so the pre-epoch decision lives in one place rather
/// than in four copies of this function.
fn now_secs() -> u64 {
    mechanics::wallclock::now_secs()
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
        let seed = mechanics::actor::random_seed().context("generate agent seed")?;
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
    mechanics: &SpaceAuthority,
    bootstrap: Vec<issues::ids::DeviceId>,
    advertise: Vec<runtime::beacon::RouteHint>,
) -> CommsOptions {
    let export = mechanics.clone();
    let frontier = mechanics.clone();
    CommsOptions {
        transport,
        station_seed: seed,
        authority: Authority {
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

/// A content id as it travels on the control channel: 32 bytes of lowercase
/// hex, and nothing else accepted.
fn parse_content_ref(raw: &str) -> Option<replica::content::ContentRef> {
    let bytes = data_encoding::HEXLOWER.decode(raw.as_bytes()).ok()?;
    Some(replica::content::ContentRef {
        content_id: <[u8; 32]>::try_from(bytes.as_slice()).ok()?,
    })
}

fn parse_operation_id(raw: &str) -> Option<[u8; 16]> {
    let bytes = data_encoding::HEXLOWER.decode(raw.as_bytes()).ok()?;
    <[u8; 16]>::try_from(bytes.as_slice()).ok()
}

fn invalid_content_id() -> ContentReply {
    ContentReply::error(
        ContentErrorCode::Invalid,
        "a content id is 32 bytes of lowercase hex",
    )
}

/// Translate a content refusal into the vocabulary a local surface maps to its
/// own status codes.
///
/// `Unknown` deliberately says nothing about whether the content exists
/// elsewhere: a caller that could tell "not here" from "never heard of it"
/// would have an oracle for what a Space contains, answerable by guessing ids.
fn content_refusal(error: &runtime::plane::freight::content::Failure) -> ContentReply {
    use runtime::plane::freight::content::Failure as E;
    let (code, message) = match error {
        E::Denied { demand } => (
            ContentErrorCode::Denied,
            format!("refused: {}", String::from_utf8_lossy(demand)),
        ),
        E::Unknown => (
            ContentErrorCode::Unknown,
            "no descriptor for that content here".to_string(),
        ),
        E::NotResident => (
            ContentErrorCode::NotResident,
            "the descriptor is here and the bytes are not".to_string(),
        ),
        E::Bounds => (
            ContentErrorCode::Bounds,
            "the range is outside what this call may return".to_string(),
        ),
        other => (ContentErrorCode::Storage, other.to_string()),
    };
    ContentReply::error(code, message)
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
    id: issues::ids::DeviceId,
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
        .filter(|r| issues::ids::DeviceId::parse(r.id.as_str()).is_some())
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

/// Run one process-backed StationHost on `home`, holding the per-home lock for
/// its lifetime. Identity is the process-global one.
pub async fn run_station_process(home: PathBuf, factory: &dyn TransportFactory) -> Result<()> {
    let device_seed = load_or_create_identity(&crate::config::identity_dir()?)?;
    run_station_process_with(home, device_seed, factory).await
}

/// The injectable process adapter with an explicit device seed. Several hosts
/// may run in one process; the
/// process layout is deployment policy rather than part of Space semantics.
pub async fn run_station_process_with(
    home: PathBuf,
    device_seed: [u8; 32],
    factory: &dyn TransportFactory,
) -> Result<()> {
    run_station_process_with_packages(home, device_seed, factory, crate::world::packages()).await
}

/// Run a StationHost with an explicitly supplied compile-time World package
/// set. This is the product-neutral composition seam used by Daemon; the
/// convenience wrappers above preserve the issue tracker's existing entry
/// points.
pub async fn run_station_process_with_packages(
    home: PathBuf,
    device_seed: [u8; 32],
    factory: &dyn TransportFactory,
    packages: WorldPackages,
) -> Result<()> {
    StationRunner::start(home, device_seed, factory, packages)
        .await?
        .run()
        .await
}

#[cfg(test)]
mod tests {
    use runtime::plane::live::LiveNarrow;
    use runtime::transient::Target;

    const BODY: [u8; 16] = [7u8; 16];
    const OTHER: [u8; 16] = [8u8; 16];

    #[test]
    fn narrowing_to_an_issue_keeps_every_scope_over_its_body() {
        // What a scoped read has to answer with. A person reading the issue, a
        // caret in its description and a typing flag in its title are three
        // different scopes over one Body, and matching by scope equality — as
        // the transient table's own filter does — would hand back the first and
        // silently drop the two a caret surface exists to draw.
        let world = crate::world::contract::world_id().as_str().to_string();
        let over_the_body = [
            Target::Body {
                world: world.clone(),
                body: BODY,
            },
            Target::Material {
                world: world.clone(),
                body: BODY,
            },
            Target::Field {
                world: world.clone(),
                body: BODY,
                field: "description".into(),
            },
            Target::Typing {
                world: world.clone(),
                body: BODY,
                field: "title".into(),
            },
        ];
        for scope in &over_the_body {
            assert!(
                LiveNarrow::Body {
                    world: &world,
                    body: BODY
                }
                .admits(scope),
                "{scope:?} is about that Body and was dropped"
            );
        }
    }

    #[test]
    fn narrowing_to_an_issue_admits_nothing_about_another_body() {
        let world = crate::world::contract::world_id().as_str().to_string();
        // A Body is addressed by World *and* id: operation ids collide across
        // documents of one activation, so a match on the id alone is not a
        // lookup miss, it is a plausible and silently wrong answer.
        let elsewhere = [
            Target::Body {
                world: world.clone(),
                body: OTHER,
            },
            Target::Field {
                world: "com.example.other".into(),
                body: BODY,
                field: "description".into(),
            },
            Target::Content { content: [0u8; 32] },
            Target::World {
                world: world.clone(),
                schema: "s".into(),
                key: "k".into(),
            },
        ];
        for scope in &elsewhere {
            assert!(
                !LiveNarrow::Body {
                    world: &world,
                    body: BODY
                }
                .admits(scope),
                "{scope:?} is not about that Body and was kept"
            );
        }
    }

    #[test]
    fn an_issue_is_named_by_its_doc_id_and_an_alias_names_nothing() {
        // The derivation is a hash of the string as given. A viewer that sent
        // `ENG-12` would ask about a Body nothing publishes under and be
        // answered an empty table for ever, with nothing anywhere to say so.
        let doc = "iss_01jz0000000000000000000000";
        assert_ne!(
            crate::world::contract::issue_body_id(doc).as_bytes(),
            crate::world::contract::issue_body_id("ENG-12").as_bytes()
        );
    }
}
#[cfg(test)]
/// Who a World's nudges actually reach.
mod nudge_delivery {
    use mechanics::station::Key;
    use runtime::world::call::Nudge;

    fn station(seed: u8) -> Key {
        Key::from_device(&mechanics::actor::device_from_seed(&[seed; 32])).expect("station")
    }

    fn nudge(actor: &str) -> Nudge {
        Nudge {
            actor: actor.into(),
            schema: "assigned".into(),
            payload: vec![1, 2, 3],
        }
    }

    /// The same helper the delivery path uses. Free rather than a method so it
    /// can be exercised without a Station, a transport and a docked Session.
    fn reachable<'a>(here: &'a [(Key, String)], nudges: &'a [Nudge]) -> Vec<(&'a Key, &'a Nudge)> {
        super::StationHost::reachable(here, nudges)
    }

    #[test]
    fn a_nudge_reaches_only_sessions_belonging_to_the_actor_it_names() {
        // Presence is the gate. Somebody who is not here is not queued for, and
        // somebody who is here under a different actor is not somebody else's
        // notification.
        let here = vec![
            (station(1), "act_alice".to_string()),
            (station(2), "act_bob".to_string()),
        ];
        let nudges = vec![nudge("act_bob")];
        let sent = reachable(&here, &nudges);
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].0, &station(2));
    }

    #[test]
    fn an_actor_with_nobody_here_reaches_nobody_and_is_not_an_error() {
        // The durable record is already committed and already converging, and it
        // is the absent peer's path. Queueing here would make this a mailbox.
        let here = vec![(station(1), "act_alice".to_string())];
        assert!(reachable(&here, &[nudge("act_carol")]).is_empty());
        assert!(reachable(&[], &[nudge("act_alice")]).is_empty());
    }

    #[test]
    fn two_devices_of_one_person_each_hear_once() {
        // Which is what having two devices means. The alternative — picking one
        // — would mean the notification landed on whichever machine the fan-out
        // happened to see first.
        let here = vec![
            (station(1), "act_alice".to_string()),
            (station(2), "act_alice".to_string()),
        ];
        let nudges = [nudge("act_alice")];
        let sent = reachable(&here, &nudges);
        assert_eq!(sent.len(), 2);
        assert_ne!(sent[0].0, sent[1].0);
    }

    #[test]
    fn several_nudges_each_find_their_own_actor() {
        // A comment on an issue with three people on it is three nudges, and
        // each has to land on its own — a fan-out that matched the first actor
        // and stopped would tell one person and silently drop the rest.
        let here = vec![
            (station(1), "act_alice".to_string()),
            (station(2), "act_bob".to_string()),
            (station(3), "act_carol".to_string()),
        ];
        let nudges = vec![nudge("act_alice"), nudge("act_carol")];
        let sent = reachable(&here, &nudges);
        assert_eq!(sent.len(), 2);
        let reached: Vec<_> = sent.iter().map(|(s, _)| *s).collect();
        assert!(reached.contains(&&station(1)));
        assert!(reached.contains(&&station(3)));
        assert!(!reached.contains(&&station(2)));
    }

    #[test]
    fn actor_matching_is_equality_and_not_a_prefix() {
        // An actor id is a canonical string. Anything looser here would deliver
        // one person's notifications to another whose id happened to start the
        // same way, which is a real shape for base32 identifiers.
        let here = vec![(station(1), "act_alice".to_string())];
        assert!(reachable(&here, &[nudge("act_ali")]).is_empty());
        assert!(reachable(&here, &[nudge("act_alicexx")]).is_empty());
        assert_eq!(reachable(&here, &[nudge("act_alice")]).len(), 1);
    }
}
