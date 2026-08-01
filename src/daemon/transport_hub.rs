//! Identity-keyed transport ownership and inbound Space demultiplexing.
//!
//! One device identity owns one concrete transport endpoint. Each active
//! StationHost receives a scoped view: outbound work and gossip delegate to the
//! shared endpoint, while inbound Contact/presence connections arrive only on
//! that Space's queue. The hub reads the bounded opening frame solely to select
//! the queue, then replays it unchanged; Runtime remains the authority that
//! verifies the protocol, peer, signature, and Space binding.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use tokio::sync::{mpsc, watch, Mutex, Semaphore};

use comms::policy::Network;
use comms::{
    Alpn, GossipReceiver, GossipSender, Incoming, IncomingConnection, PeerId, Stream, Topic,
    Transport, TransportFactory,
};
use issues::ids::{DeviceId, SpaceId};

const SPACE_INCOMING_BUFFER: usize = 16;
const MAX_PENDING_OPENERS: usize = 64;
const OPENING_FRAME_DEADLINE: Duration = Duration::from_secs(5);

type SpaceBytes = [u8; 29];
type HubSlot = Arc<Mutex<Option<Arc<IdentityTransportHub>>>>;

/// A process-level factory that shares one transport endpoint per device key.
pub(crate) struct TransportHubFactory {
    inner: Arc<dyn TransportFactory>,
    hubs: StdMutex<HashMap<DeviceId, HubSlot>>,
    stopping: AtomicBool,
}

impl TransportHubFactory {
    pub(crate) fn new(inner: Arc<dyn TransportFactory>) -> Self {
        Self {
            inner,
            hubs: StdMutex::new(HashMap::new()),
            stopping: AtomicBool::new(false),
        }
    }

    fn slot(&self, identity: DeviceId) -> HubSlot {
        self.hubs
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .entry(identity)
            .or_insert_with(|| Arc::new(Mutex::new(None)))
            .clone()
    }
}

#[async_trait]
impl TransportFactory for TransportHubFactory {
    async fn build(
        &self,
        _identity_seed: &[u8; 32],
        _network: &Network,
        _protocols: comms::Protocols<'_>,
    ) -> Result<Arc<dyn Transport>> {
        Err(anyhow!(
            "the identity transport hub requires an explicit Space scope"
        ))
    }

    async fn build_scoped(
        &self,
        identity_seed: &[u8; 32],
        network: &Network,
        protocols: comms::Protocols<'_>,
        space: &SpaceId,
    ) -> Result<Arc<dyn Transport>> {
        if self.stopping.load(Ordering::Acquire) {
            return Err(anyhow!("the identity transport hub is shutting down"));
        }
        let identity = mechanics::actor::device_from_seed(identity_seed);
        let slot = self.slot(identity.clone());
        let mut occupied = slot.lock().await;
        if self.stopping.load(Ordering::Acquire) {
            return Err(anyhow!("the identity transport hub is shutting down"));
        }

        let hub = match occupied.as_ref() {
            Some(hub) => {
                hub.require_compatible(network, protocols)?;
                hub.clone()
            }
            None => {
                let transport = self.inner.build(identity_seed, network, protocols).await?;
                if transport.my_id() != identity {
                    transport.shutdown().await;
                    return Err(anyhow!(
                        "transport factory returned identity {}, expected {}",
                        transport.my_id(),
                        identity
                    ));
                }
                let hub = IdentityTransportHub::start(transport, network, protocols);
                *occupied = Some(hub.clone());
                hub
            }
        };
        hub.register(space)
    }

    async fn shutdown(&self) {
        if self.stopping.swap(true, Ordering::AcqRel) {
            return;
        }
        let slots: Vec<_> = self
            .hubs
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .drain()
            .map(|(_, slot)| slot)
            .collect();
        for slot in slots {
            if let Some(hub) = slot.lock().await.take() {
                hub.shutdown().await;
            }
        }
        self.inner.shutdown().await;
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum NetworkKey {
    Public,
    Local(String),
    Isolated,
}

impl From<&Network> for NetworkKey {
    fn from(network: &Network) -> Self {
        match network {
            Network::Public => Self::Public,
            Network::Local(local) => Self::Local(local.relay.clone()),
            Network::Isolated => Self::Isolated,
        }
    }
}

fn normalized_alpns(protocols: comms::Protocols<'_>) -> Vec<Vec<u8>> {
    let mut values: Vec<_> = protocols.all().map(|alpn| alpn.to_vec()).collect();
    values.sort();
    values.dedup();
    values
}

#[derive(Clone)]
struct RouteTarget {
    token: u64,
    incoming: mpsc::Sender<Incoming>,
    /// One sender per registered session ALPN, not one per Space.
    ///
    /// Two planes on one queue share its slots and its single reader: a backlog
    /// on either is a stall on both, and whichever driver happens to be parked
    /// takes the next connection whatever plane it was for. Two entries, so a
    /// linear scan beats a map.
    session_lanes: Arc<[(comms::Alpn, mpsc::Sender<IncomingConnection>)]>,
    stopping: watch::Sender<bool>,
}

impl RouteTarget {
    fn lane(&self, alpn: &[u8]) -> Option<&mpsc::Sender<IncomingConnection>> {
        self.session_lanes
            .iter()
            .find(|(lane, _)| *lane == alpn)
            .map(|(_, sender)| sender)
    }
}

struct IdentityTransportHub {
    transport: Arc<dyn Transport>,
    network: NetworkKey,
    alpns: Vec<Vec<u8>>,
    /// The session ALPNs, still distinguishable.
    ///
    /// `alpns` above is the flattened union used for the endpoint-compatibility
    /// check; once flattened, which of them carry sessions is unrecoverable —
    /// and that is exactly the question every route now has to answer.
    session_alpns: Arc<[comms::Alpn]>,
    routes: Arc<StdMutex<HashMap<SpaceBytes, RouteTarget>>>,
    next_token: AtomicU64,
    stopping: watch::Sender<bool>,
    accept_task: StdMutex<Option<tokio::task::JoinHandle<()>>>,
    session_task: StdMutex<Option<tokio::task::JoinHandle<()>>>,
}

impl IdentityTransportHub {
    fn start(
        transport: Arc<dyn Transport>,
        network: &Network,
        protocols: comms::Protocols<'_>,
    ) -> Arc<Self> {
        let hub = Arc::new(Self {
            transport: transport.clone(),
            network: NetworkKey::from(network),
            alpns: normalized_alpns(protocols),
            session_alpns: protocols.session.to_vec().into(),
            routes: Arc::new(StdMutex::new(HashMap::new())),
            next_token: AtomicU64::new(1),
            stopping: watch::Sender::new(false),
            accept_task: StdMutex::new(None),
            session_task: StdMutex::new(None),
        });
        let task = tokio::spawn(run_accept_pump(
            transport.clone(),
            hub.routes.clone(),
            hub.stopping.subscribe(),
        ));
        *hub.accept_task
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(task);
        let session_task = tokio::spawn(run_session_pump(
            transport,
            hub.routes.clone(),
            hub.session_alpns.clone(),
            hub.stopping.subscribe(),
        ));
        *hub.session_task
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(session_task);
        hub
    }

    fn require_compatible(&self, network: &Network, protocols: comms::Protocols<'_>) -> Result<()> {
        let requested_network = NetworkKey::from(network);
        if self.network != requested_network {
            return Err(anyhow!(
                "one device identity cannot use two network policies in one Lait daemon \
                 ({:?} and {:?})",
                self.network,
                requested_network
            ));
        }
        let requested_alpns = normalized_alpns(protocols);
        if self.alpns != requested_alpns {
            return Err(anyhow!(
                "one device identity requested incompatible protocol sets"
            ));
        }
        Ok(())
    }

    fn register(self: &Arc<Self>, space: &SpaceId) -> Result<Arc<dyn Transport>> {
        let space_bytes = SpaceBytes::try_from(space.as_str().as_bytes())
            .map_err(|_| anyhow!("Space id does not have the canonical 29-byte shape"))?;
        let token = self.next_token.fetch_add(1, Ordering::Relaxed);
        let (incoming_tx, incoming_rx) = mpsc::channel(SPACE_INCOMING_BUFFER);
        // One session queue per plane. Still one registration and one route: a
        // second `build_scoped` for this Space is refused below, so the split
        // has to happen inside the one view rather than by minting two.
        let mut lanes = Vec::with_capacity(self.session_alpns.len());
        let mut queues = Vec::with_capacity(self.session_alpns.len());
        for &alpn in self.session_alpns.iter() {
            let (session_tx, session_rx) = mpsc::channel(SPACE_INCOMING_BUFFER);
            lanes.push((alpn, session_tx));
            queues.push((alpn, Some(session_rx)));
        }
        let stopping = watch::Sender::new(false);
        let target = RouteTarget {
            token,
            incoming: incoming_tx,
            session_lanes: lanes.into(),
            stopping: stopping.clone(),
        };
        let mut routes = self
            .routes
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if routes.contains_key(&space_bytes) {
            return Err(anyhow!(
                "device {} already has an active Station in Space {}; \
                 a remote peer cannot address two same-device Stations in one Space",
                self.transport.my_id(),
                space
            ));
        }
        routes.insert(space_bytes, target);
        drop(routes);

        Ok(Arc::new(ScopedTransport {
            hub: self.clone(),
            space: space_bytes,
            token,
            incoming: Mutex::new(incoming_rx),
            session_queues: StdMutex::new(queues),
            stopping,
            stopped: AtomicBool::new(false),
        }))
    }

    fn unregister(&self, space: SpaceBytes, token: u64) {
        let mut routes = self
            .routes
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if routes
            .get(&space)
            .is_some_and(|target| target.token == token)
        {
            if let Some(target) = routes.remove(&space) {
                target.stopping.send_replace(true);
            }
        }
    }

    async fn shutdown(&self) {
        if self.stopping.send_replace(true) {
            return;
        }
        let targets: Vec<_> = self
            .routes
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .drain()
            .map(|(_, target)| target)
            .collect();
        for target in targets {
            target.stopping.send_replace(true);
        }
        self.transport.shutdown().await;
        // Both pumps. `session_task` was spawned and stored and never awaited,
        // which is a task outliving the hub that owns it — the kind of leak
        // that only shows up as a Station that will not stop.
        let tasks = [
            self.accept_task
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .take(),
            self.session_task
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .take(),
        ];
        for task in tasks.into_iter().flatten() {
            if let Err(error) = task.await {
                tracing::debug!(%error, "identity transport pump failed");
            }
        }
    }
}

async fn run_accept_pump(
    transport: Arc<dyn Transport>,
    routes: Arc<StdMutex<HashMap<SpaceBytes, RouteTarget>>>,
    mut stopping: watch::Receiver<bool>,
) {
    let permits = Arc::new(Semaphore::new(MAX_PENDING_OPENERS));
    let mut dispatches = tokio::task::JoinSet::new();
    loop {
        if *stopping.borrow() {
            break;
        }
        tokio::select! {
            changed = stopping.changed() => {
                if changed.is_err() || *stopping.borrow() {
                    break;
                }
            }
            incoming = transport.accept() => {
                let Some(incoming) = incoming else {
                    break;
                };
                let permit = tokio::select! {
                    permit = permits.clone().acquire_owned() => match permit {
                        Ok(permit) => permit,
                        Err(_) => break,
                    },
                    changed = stopping.changed() => {
                        if changed.is_err() || *stopping.borrow() {
                            break;
                        }
                        continue;
                    }
                };
                dispatches.spawn(dispatch_incoming(
                    incoming,
                    routes.clone(),
                    stopping.clone(),
                    permit,
                ));
            }
            result = dispatches.join_next(), if !dispatches.is_empty() => {
                if let Some(Err(error)) = result {
                    tracing::debug!(%error, "identity transport dispatcher failed");
                }
            }
        }
    }

    while let Some(result) = dispatches.join_next().await {
        if let Err(error) = result {
            tracing::debug!(%error, "identity transport dispatcher failed during shutdown");
        }
    }
}

/// The connection half of the pump.
///
/// Same shape as [`run_accept_pump`] and same bounds — a permit is taken before
/// a dispatcher is spawned, so a flood of openers cannot outrun the cap by
/// queueing tasks. What differs is what gets routed: a whole connection, after
/// one bounded opening read on one control flow, rather than a framed stream
/// per protocol message.
async fn run_session_pump(
    transport: Arc<dyn Transport>,
    routes: Arc<StdMutex<HashMap<SpaceBytes, RouteTarget>>>,
    session_alpns: Arc<[comms::Alpn]>,
    mut stopping: watch::Receiver<bool>,
) {
    // One budget per plane, not one per device. A shared pool lets a stalled
    // plane hold every pending slot for every Space — the same head-of-line
    // failure the split queues remove, one level up, and the one that would
    // make "a saturated transfer cannot delay a cursor" false.
    let permits: Vec<(comms::Alpn, Arc<Semaphore>)> = session_alpns
        .iter()
        .map(|&alpn| (alpn, Arc::new(Semaphore::new(MAX_PENDING_OPENERS))))
        .collect();
    let mut dispatches = tokio::task::JoinSet::new();
    loop {
        if *stopping.borrow() {
            break;
        }
        tokio::select! {
            changed = stopping.changed() => {
                if changed.is_err() || *stopping.borrow() {
                    break;
                }
            }
            incoming = transport.accept_connection() => {
                let Some(incoming) = incoming else {
                    break;
                };
                // The ALPN is known before anything is read or spent, and the
                // registered set is the hub's own — so an unregistered protocol
                // is refused here rather than after a dispatcher task and an
                // opening read have been spent on it.
                let Some(plane_permits) = permits
                    .iter()
                    .find(|(alpn, _)| *alpn == incoming.alpn.as_slice())
                    .map(|(_, plane_permits)| plane_permits.clone())
                else {
                    incoming.connection.close(REFUSED_CODE, b"");
                    continue;
                };
                let permit = tokio::select! {
                    permit = plane_permits.acquire_owned() => match permit {
                        Ok(permit) => permit,
                        Err(_) => break,
                    },
                    changed = stopping.changed() => {
                        if changed.is_err() || *stopping.borrow() {
                            break;
                        }
                        continue;
                    }
                };
                dispatches.spawn(dispatch_connection(
                    incoming,
                    routes.clone(),
                    stopping.clone(),
                    permit,
                ));
            }
            result = dispatches.join_next(), if !dispatches.is_empty() => {
                if let Some(Err(error)) = result {
                    tracing::debug!(%error, "identity transport session dispatcher failed");
                }
            }
        }
    }

    while let Some(result) = dispatches.join_next().await {
        if let Err(error) = result {
            tracing::debug!(%error, "identity transport session dispatcher failed during shutdown");
        }
    }
}

/// Read one bounded opening off the connection's first flow, derive the Space,
/// and hand the **whole connection** to that Space's route.
///
/// The opening is read here and not replayed. A framed stream needs its first
/// message put back because the protocol above expects to read it; a plane's
/// opening is consumed by the routing decision and delivered alongside the
/// connection, so the owner does not re-parse what the hub already parsed.
async fn dispatch_connection(
    incoming: IncomingConnection,
    routes: Arc<StdMutex<HashMap<SpaceBytes, RouteTarget>>>,
    mut hub_stopping: watch::Receiver<bool>,
    _permit: tokio::sync::OwnedSemaphorePermit,
) {
    // Shadowed so the permit can be moved into the routed connection below.
    let _permit = _permit;
    if *hub_stopping.borrow() {
        return;
    }

    let opening = tokio::select! {
        changed = hub_stopping.changed() => {
            let _ = changed;
            return;
        }
        read = tokio::time::timeout(
            OPENING_FRAME_DEADLINE,
            read_opening(incoming.connection.as_ref()),
        ) => match read {
            Ok(Ok(opening)) => opening,
            _ => {
                incoming.connection.close(REFUSED_CODE, b"");
                return;
            }
        }
    };

    let Some(space) = session_opening_space(&opening) else {
        incoming.connection.close(REFUSED_CODE, b"");
        return;
    };
    let target = routes
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .get(&space)
        .cloned();
    let Some(target) = target else {
        // Coarse on purpose: telling an unadmitted peer apart from an unknown
        // Space is an oracle for which Spaces this device holds.
        incoming.connection.close(REFUSED_CODE, b"");
        return;
    };
    // Routed by the ALPN, not by the opening's `plane` field. Both are in hand
    // here and only one of them was fixed by the handshake — routing on the
    // peer's own assertion would turn a claim into a dispatch decision.
    // `judge` still refuses an opening whose plane disagrees with its ALPN.
    let Some(lane) = target.lane(&incoming.alpn).cloned() else {
        incoming.connection.close(REFUSED_CODE, b"");
        return;
    };

    let mut route_stopping = target.stopping.subscribe();
    if *route_stopping.borrow() || *hub_stopping.borrow() {
        incoming.connection.close(REFUSED_CODE, b"");
        return;
    }
    // The permit rides with the connection. Releasing it here would let the
    // pending-opener budget count a connection as finished the moment it was
    // routed, which is exactly when it starts costing something.
    let routed = IncomingConnection {
        from: incoming.from,
        alpn: incoming.alpn,
        connection: Box::new(HeldConnection {
            inner: incoming.connection,
            _permit,
        }),
        opening,
    };
    tokio::select! {
        sent = lane.send(routed) => {
            if let Err(refused) = sent {
                refused.0.connection.close(REFUSED_CODE, b"");
            }
        }
        changed = route_stopping.changed() => {
            let _ = changed;
        }
        changed = hub_stopping.changed() => {
            let _ = changed;
        }
    }
}

/// A routed connection that still holds its pending-opener permit.
///
/// The permit has to outlive the routing decision, not end with it: a
/// connection is cheapest at the moment it is handed over and most expensive
/// afterwards, so releasing the slot on delivery would bound the wrong thing.
/// Wrapping rather than threading a permit through every caller keeps the
/// Space's owner from having to know the hub has a budget at all — the same
/// shape `ReplayStream` already uses for the framed pump.
struct HeldConnection {
    inner: Box<dyn comms::Connection>,
    _permit: tokio::sync::OwnedSemaphorePermit,
}

#[async_trait]
impl comms::Connection for HeldConnection {
    fn peer(&self) -> PeerId {
        self.inner.peer()
    }

    fn alpn(&self) -> Vec<u8> {
        self.inner.alpn()
    }

    async fn open_bi(&self) -> Result<(Box<dyn comms::SendFlow>, Box<dyn comms::RecvFlow>)> {
        self.inner.open_bi().await
    }

    async fn accept_bi(
        &self,
    ) -> Result<Option<(Box<dyn comms::SendFlow>, Box<dyn comms::RecvFlow>)>> {
        self.inner.accept_bi().await
    }

    async fn open_uni(&self) -> Result<Box<dyn comms::SendFlow>> {
        self.inner.open_uni().await
    }

    async fn accept_uni(&self) -> Result<Option<Box<dyn comms::RecvFlow>>> {
        self.inner.accept_uni().await
    }

    fn send_datagram(&self, payload: &[u8]) -> Result<()> {
        self.inner.send_datagram(payload)
    }

    async fn read_datagram(&self) -> Result<Option<Vec<u8>>> {
        self.inner.read_datagram().await
    }

    fn datagram_capacity(&self) -> Option<usize> {
        self.inner.datagram_capacity()
    }

    fn close(&self, code: u32, reason: &[u8]) {
        self.inner.close(code, reason);
    }

    async fn closed(&self) {
        self.inner.closed().await;
    }
}

/// The close code a hub uses when it will not route a connection.
///
/// One code for every reason. Distinguishing "no such Space" from "not
/// admitted" from "shutting down" would tell a peer what this device holds.
const REFUSED_CODE: u32 = 1;

/// Read the opening from the connection's first flow, bounded before anything
/// is allocated for it.
async fn read_opening(connection: &dyn comms::Connection) -> Result<Vec<u8>> {
    let mut recv = connection
        .accept_uni()
        .await?
        .ok_or_else(|| anyhow!("the peer opened no control flow"))?;
    recv.read_to_end(runtime::plane::bounds::MAX_OPENING_BYTES)
        .await
}

fn session_opening_space(opening: &[u8]) -> Option<SpaceBytes> {
    let open = runtime::plane::Open::decode_canonical(opening).ok()?;
    Some(open.space)
}

async fn dispatch_incoming(
    mut incoming: Incoming,
    routes: Arc<StdMutex<HashMap<SpaceBytes, RouteTarget>>>,
    mut hub_stopping: watch::Receiver<bool>,
    _permit: tokio::sync::OwnedSemaphorePermit,
) {
    if *hub_stopping.borrow() {
        return;
    }
    let Some(opening_limit) = opening_limit(&incoming.alpn) else {
        return;
    };
    let first = tokio::select! {
        changed = hub_stopping.changed() => {
            let _ = changed;
            return;
        }
        received = tokio::time::timeout(
            OPENING_FRAME_DEADLINE,
            incoming.stream.recv_bounded(opening_limit),
        ) => {
            match received {
                Ok(Ok(Some(frame))) => frame,
                _ => return,
            }
        }
    };
    let Some(space) = opening_space(&incoming.alpn, &first) else {
        return;
    };
    let target = routes
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .get(&space)
        .cloned();
    let Some(target) = target else {
        return;
    };
    incoming.stream = Box::new(ReplayStream {
        first: Some(first),
        inner: incoming.stream,
    });

    let mut route_stopping = target.stopping.subscribe();
    if *route_stopping.borrow() || *hub_stopping.borrow() {
        return;
    }
    tokio::select! {
        _ = target.incoming.send(incoming) => {}
        changed = route_stopping.changed() => {
            let _ = changed;
        }
        changed = hub_stopping.changed() => {
            let _ = changed;
        }
    }
}

fn opening_limit(alpn: &[u8]) -> Option<usize> {
    if alpn == runtime::plane::contact::CONTACT_ALPN {
        Some(runtime::plane::contact::MAX_FRAME)
    } else if alpn == runtime::neighbor::PRESENCE_ALPN {
        Some(runtime::neighbor::MAX_MESSAGE)
    } else {
        None
    }
}

fn opening_space(alpn: &[u8], first: &[u8]) -> Option<SpaceBytes> {
    if alpn == runtime::plane::contact::CONTACT_ALPN {
        if first.len() > runtime::plane::contact::MAX_FRAME {
            return None;
        }
        runtime::plane::contact::Offer::decode(first)
            .ok()
            .map(|hello| hello.space)
    } else if alpn == runtime::neighbor::PRESENCE_ALPN {
        runtime::neighbor::PresenceProbe::decode(first)
            .ok()
            .map(|probe| probe.space)
    } else {
        None
    }
}

struct ReplayStream {
    first: Option<Vec<u8>>,
    inner: Box<dyn Stream>,
}

#[async_trait]
impl Stream for ReplayStream {
    async fn send(&mut self, frame: &[u8]) -> Result<()> {
        self.inner.send(frame).await
    }

    async fn recv(&mut self) -> Result<Option<Vec<u8>>> {
        match self.first.take() {
            Some(first) => Ok(Some(first)),
            None => self.inner.recv().await,
        }
    }

    async fn finish(&mut self) -> Result<()> {
        self.inner.finish().await
    }

    async fn wait_closed(&mut self) {
        self.inner.wait_closed().await;
    }
}

struct ScopedTransport {
    hub: Arc<IdentityTransportHub>,
    space: SpaceBytes,
    token: u64,
    incoming: Mutex<mpsc::Receiver<Incoming>>,
    /// One un-taken queue per registered session ALPN.
    ///
    /// A `std` mutex because taking one is a synchronous hand-over at mount
    /// time, not an await on any hot path.
    session_queues: StdMutex<Vec<(comms::Alpn, Option<mpsc::Receiver<IncomingConnection>>)>>,
    stopping: watch::Sender<bool>,
    stopped: AtomicBool,
}

impl ScopedTransport {
    fn unregister(&self) {
        if !self.stopped.swap(true, Ordering::AcqRel) {
            self.stopping.send_replace(true);
            self.hub.unregister(self.space, self.token);
        }
    }

    fn ensure_running(&self) -> Result<()> {
        if self.stopped.load(Ordering::Acquire) || *self.hub.stopping.borrow() {
            Err(anyhow!("the Space transport is shut down"))
        } else {
            Ok(())
        }
    }
}

impl Drop for ScopedTransport {
    fn drop(&mut self) {
        self.unregister();
    }
}

#[async_trait]
impl Transport for ScopedTransport {
    fn my_id(&self) -> PeerId {
        self.hub.transport.my_id()
    }

    fn learn(&self, peer: PeerId, addrs: &[SocketAddr]) {
        if self.ensure_running().is_ok() {
            self.hub.transport.learn(peer, addrs);
        }
    }

    async fn connect(&self, peer: PeerId, alpn: Alpn) -> Result<Box<dyn Stream>> {
        self.ensure_running()?;
        self.hub.transport.connect(peer, alpn).await
    }

    async fn accept(&self) -> Option<Incoming> {
        let mut stopping = self.stopping.subscribe();
        if *stopping.borrow() {
            return None;
        }
        let mut incoming = self.incoming.lock().await;
        tokio::select! {
            value = incoming.recv() => value,
            _ = stopping.wait_for(|value| *value) => None,
        }
    }

    async fn connect_session(
        &self,
        peer: PeerId,
        alpn: Alpn,
    ) -> Result<Box<dyn comms::Connection>> {
        self.ensure_running()?;
        self.hub.transport.connect_session(peer, alpn).await
    }

    /// A scoped view has no undivided session door — sessions arrive per plane
    /// through [`Transport::take_session_queue`].
    ///
    /// `None` rather than the trait's `pending()`: a caller reaching for this
    /// has mis-wired a driver, and an immediate end-of-stream says so on the
    /// first poll. Parking would make that mistake indistinguishable from a
    /// Station nobody has dialled.
    async fn accept_connection(&self) -> Option<IncomingConnection> {
        None
    }

    fn take_session_queue(&self, alpn: comms::Alpn) -> Option<comms::ConnectionQueue> {
        self.session_queues
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .iter_mut()
            .find(|(lane, _)| *lane == alpn)
            .and_then(|(_, queue)| queue.take())
    }

    fn advertised_addrs(&self) -> Vec<SocketAddr> {
        self.hub.transport.advertised_addrs()
    }

    async fn advertised_routes(&self, deadline: Duration) -> Result<Vec<SocketAddr>> {
        self.ensure_running()?;
        self.hub.transport.advertised_routes(deadline).await
    }

    fn is_isolated(&self) -> bool {
        self.hub.transport.is_isolated()
    }

    async fn subscribe(
        &self,
        topic: Topic,
        bootstrap: &[PeerId],
    ) -> Result<(Box<dyn GossipSender>, Box<dyn GossipReceiver>)> {
        self.ensure_running()?;
        self.hub.transport.subscribe(topic, bootstrap).await
    }

    async fn shutdown(&self) {
        self.unregister();
    }
}

#[cfg(test)]
mod tests {
    /// The Freight queue for a scoped view.
    ///
    /// A scoped view has no undivided session door any more — each plane owns
    /// its own queue, and taking one is how a driver gets its connections. The
    /// tests below say which plane they mean rather than accepting whatever
    /// arrived first, which is the property the split exists to give them.
    fn freight_queue(view: &std::sync::Arc<dyn Transport>) -> comms::ConnectionQueue {
        view.take_session_queue(runtime::plane::FREIGHT_ALPN)
            .expect("the Freight queue is taken exactly once")
    }

    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use comms::mem::MemNet;

    struct MemFactory {
        net: MemNet,
        builds: AtomicUsize,
    }

    #[async_trait]
    impl TransportFactory for MemFactory {
        async fn build(
            &self,
            identity_seed: &[u8; 32],
            _network: &Network,
            _protocols: comms::Protocols<'_>,
        ) -> Result<Arc<dyn Transport>> {
            self.builds.fetch_add(1, Ordering::SeqCst);
            Ok(Arc::new(
                self.net
                    .peer(mechanics::actor::device_from_seed(identity_seed)),
            ))
        }
    }

    fn space(n: u8) -> SpaceId {
        SpaceId::from_digest([n; 16])
    }

    fn space_bytes(space: &SpaceId) -> SpaceBytes {
        SpaceBytes::try_from(space.as_str().as_bytes()).unwrap()
    }

    const ALPNS: &[Alpn] = &[
        runtime::plane::contact::CONTACT_ALPN,
        runtime::neighbor::PRESENCE_ALPN,
    ];
    const SESSION_ALPNS: &[Alpn] = &[runtime::plane::FREIGHT_ALPN, runtime::plane::LIVE_ALPN];
    fn protocols() -> comms::Protocols<'static> {
        comms::Protocols {
            framed: ALPNS,
            session: SESSION_ALPNS,
        }
    }

    fn session_open(space: &SpaceId) -> Vec<u8> {
        runtime::plane::Open {
            plane: runtime::plane::Plane::Freight,
            protocol_version: runtime::plane::FREIGHT_PROTOCOL_VERSION,
            features: 0,
            space: space_bytes(space),
            initiator_station: [1u8; 32],
            responder_station: [2u8; 32],
            connection_id: [3u8; 16],
            connection_epoch: [4u8; 16],
            authority_frontier: Vec::new(),
            requested_lanes: vec![runtime::plane::stream_kind::CONTROL],
        }
        .encode()
    }

    /// Dial a session and send its opening on the first flow, which is what the
    /// hub reads to decide where the connection goes.
    async fn dial_session(
        from: &Arc<dyn Transport>,
        to: PeerId,
        space: &SpaceId,
    ) -> Box<dyn comms::Connection> {
        let connection = from
            .connect_session(to, runtime::plane::FREIGHT_ALPN)
            .await
            .expect("dial");
        let mut opening = connection.open_uni().await.expect("open");
        opening
            .write_all(&session_open(space))
            .await
            .expect("write opening");
        opening.finish().expect("finish");
        connection
    }

    /// Dial one named plane, so a test can be about which queue a connection
    /// lands in rather than about whichever one happened to be read first.
    async fn dial_plane(
        from: &Arc<dyn Transport>,
        to: PeerId,
        space: &SpaceId,
        plane: runtime::plane::Plane,
    ) -> Box<dyn comms::Connection> {
        let connection = from.connect_session(to, plane.alpn()).await.expect("dial");
        let mut opening = connection.open_uni().await.expect("open");
        let open = runtime::plane::Open {
            plane,
            protocol_version: plane.protocol_version(),
            features: 0,
            space: <[u8; 29]>::try_from(space.as_str().as_bytes()).expect("space"),
            initiator_station: [1u8; 32],
            responder_station: [2u8; 32],
            connection_id: [3u8; 16],
            connection_epoch: [4u8; 16],
            authority_frontier: Vec::new(),
            requested_lanes: Vec::new(),
        };
        opening
            .write_all(&open.encode())
            .await
            .expect("write opening");
        opening.finish().expect("finish");
        connection
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn each_plane_gets_its_own_queue_and_neither_drains_the_other() {
        // The reason the split exists. One queue per Space meant two drivers
        // racing one receiver: each would take strictly alternating connections
        // and refuse half of what it was handed as a foreign ALPN — which the
        // driver's own comment calls "a routing bug on our side". Freight would
        // have broken the day Live shipped.
        //
        // Asserted in the order that catches it: Live is dialled FIRST and read
        // SECOND. A shared queue would hand the Live connection to whoever
        // asked first, so reading Freight would either block or return the
        // wrong connection.
        let inner = Arc::new(MemFactory {
            net: MemNet::new(),
            builds: AtomicUsize::new(0),
        });
        let factory = TransportHubFactory::new(inner.clone());
        let network = Network::Isolated;
        let seed_a = [71; 32];
        let seed_b = [72; 32];
        let space = space(7);

        let dialer = factory
            .build_scoped(&seed_a, &network, protocols(), &space)
            .await
            .unwrap();
        let listener = factory
            .build_scoped(&seed_b, &network, protocols(), &space)
            .await
            .unwrap();
        let peer_b = mechanics::actor::device_from_seed(&seed_b);

        let mut live_queue = listener
            .take_session_queue(runtime::plane::LIVE_ALPN)
            .expect("the Live queue");
        let mut freight_queue = listener
            .take_session_queue(runtime::plane::FREIGHT_ALPN)
            .expect("the Freight queue");

        let _live = dial_plane(&dialer, peer_b.clone(), &space, runtime::plane::Plane::Live).await;
        let _freight = dial_plane(
            &dialer,
            peer_b.clone(),
            &space,
            runtime::plane::Plane::Freight,
        )
        .await;

        let freight = tokio::time::timeout(Duration::from_secs(5), freight_queue.recv())
            .await
            .expect("the Freight connection was not delayed by the Live one")
            .expect("a Freight connection");
        assert_eq!(freight.alpn, runtime::plane::FREIGHT_ALPN.to_vec());

        let live = tokio::time::timeout(Duration::from_secs(5), live_queue.recv())
            .await
            .expect("the Live connection was still waiting for its own reader")
            .expect("a Live connection");
        assert_eq!(live.alpn, runtime::plane::LIVE_ALPN.to_vec());

        // And neither queue holds the other's traffic.
        assert!(
            tokio::time::timeout(Duration::from_millis(200), freight_queue.recv())
                .await
                .is_err(),
            "the Freight queue received something that was not Freight"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_queue_is_handed_over_once() {
        // A second handle on the same connections is the mis-wiring the split
        // exists to prevent, so it is not expressible.
        let inner = Arc::new(MemFactory {
            net: MemNet::new(),
            builds: AtomicUsize::new(0),
        });
        let factory = TransportHubFactory::new(inner.clone());
        let view = factory
            .build_scoped(&[73; 32], &Network::Isolated, protocols(), &space(8))
            .await
            .unwrap();
        assert!(view
            .take_session_queue(runtime::plane::FREIGHT_ALPN)
            .is_some());
        assert!(
            view.take_session_queue(runtime::plane::FREIGHT_ALPN)
                .is_none(),
            "a second taker got a second handle on one plane's connections"
        );
        // An ALPN this view never registered has no queue to take.
        assert!(view.take_session_queue(b"lait/not-a-plane/1").is_none());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_whole_connection_routes_to_the_space_its_opening_names() {
        // The framed pump replays the opener because the protocol above wants
        // to read it. A plane's opening is consumed by the routing decision, so
        // what must be right here is *where the connection went*, not what the
        // owner can still read off it.
        let inner = Arc::new(MemFactory {
            net: MemNet::new(),
            builds: AtomicUsize::new(0),
        });
        let factory = TransportHubFactory::new(inner.clone());
        let network = Network::Isolated;
        let seed_a = [51; 32];
        let seed_b = [52; 32];
        let space_a = space(1);
        let space_b = space(2);

        let a_space_a = factory
            .build_scoped(&seed_a, &network, protocols(), &space_a)
            .await
            .unwrap();
        let b_space_a = factory
            .build_scoped(&seed_b, &network, protocols(), &space_a)
            .await
            .unwrap();
        let b_space_b = factory
            .build_scoped(&seed_b, &network, protocols(), &space_b)
            .await
            .unwrap();

        let peer_b = mechanics::actor::device_from_seed(&seed_b);
        let dialed = dial_session(&a_space_a, peer_b.clone(), &space_a).await;

        let routed = tokio::time::timeout(Duration::from_secs(5), freight_queue(&b_space_a).recv())
            .await
            .expect("routed in time")
            .expect("a connection");
        assert_eq!(routed.alpn, runtime::plane::FREIGHT_ALPN.to_vec());
        assert_eq!(routed.from, mechanics::actor::device_from_seed(&seed_a));
        // The bytes the hub read to decide, handed over rather than replayed.
        // Reading a flow consumes it, so without this the Space's owner would
        // have to guess at what the peer said — or the two would parse it
        // separately and be free to disagree.
        assert_eq!(
            routed.opening,
            session_open(&space_a),
            "the routed connection carries the opening the hub parsed"
        );
        assert_eq!(
            runtime::plane::Open::decode_canonical(&routed.opening)
                .expect("and it is still canonical")
                .space,
            space_bytes(&space_a)
        );

        // And Space B, on the same device endpoint, saw nothing.
        assert!(
            tokio::time::timeout(Duration::from_millis(200), freight_queue(&b_space_b).recv())
                .await
                .is_err(),
            "a connection belongs to the Space its opening named"
        );
        dialed.close(0, b"done");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_connection_for_an_unknown_space_is_refused_without_saying_why() {
        let inner = Arc::new(MemFactory {
            net: MemNet::new(),
            builds: AtomicUsize::new(0),
        });
        let factory = TransportHubFactory::new(inner.clone());
        let network = Network::Isolated;
        let seed_a = [53; 32];
        let seed_b = [54; 32];
        let space_a = space(1);
        let unknown = space(9);

        let a_space_a = factory
            .build_scoped(&seed_a, &network, protocols(), &space_a)
            .await
            .unwrap();
        let b_space_a = factory
            .build_scoped(&seed_b, &network, protocols(), &space_a)
            .await
            .unwrap();

        let peer_b = mechanics::actor::device_from_seed(&seed_b);
        let dialed = dial_session(&a_space_a, peer_b, &unknown).await;

        // The dialer learns only that it was closed.
        tokio::time::timeout(Duration::from_secs(5), dialed.closed())
            .await
            .expect("refused in time");
        assert!(
            tokio::time::timeout(Duration::from_millis(200), freight_queue(&b_space_a).recv())
                .await
                .is_err(),
            "an unknown Space routes nowhere"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_connection_whose_opening_never_arrives_is_dropped_rather_than_held() {
        // The leak this bounds. An opener that dials and then says nothing must
        // cost one pending slot for a deadline, not a route and not forever.
        let inner = Arc::new(MemFactory {
            net: MemNet::new(),
            builds: AtomicUsize::new(0),
        });
        let factory = TransportHubFactory::new(inner.clone());
        let network = Network::Isolated;
        let seed_a = [55; 32];
        let seed_b = [56; 32];
        let space_a = space(1);

        let a_space_a = factory
            .build_scoped(&seed_a, &network, protocols(), &space_a)
            .await
            .unwrap();
        let b_space_a = factory
            .build_scoped(&seed_b, &network, protocols(), &space_a)
            .await
            .unwrap();

        let peer_b = mechanics::actor::device_from_seed(&seed_b);
        let silent = a_space_a
            .connect_session(peer_b, runtime::plane::FREIGHT_ALPN)
            .await
            .expect("dial");

        assert!(
            tokio::time::timeout(Duration::from_millis(300), freight_queue(&b_space_a).recv())
                .await
                .is_err(),
            "a silent opener routes nothing"
        );
        drop(silent);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn shutting_a_space_down_stops_routing_connections_to_it() {
        let inner = Arc::new(MemFactory {
            net: MemNet::new(),
            builds: AtomicUsize::new(0),
        });
        let factory = TransportHubFactory::new(inner.clone());
        let network = Network::Isolated;
        let seed_a = [57; 32];
        let seed_b = [58; 32];
        let space_a = space(1);

        let a_space_a = factory
            .build_scoped(&seed_a, &network, protocols(), &space_a)
            .await
            .unwrap();
        let b_space_a = factory
            .build_scoped(&seed_b, &network, protocols(), &space_a)
            .await
            .unwrap();

        b_space_a.shutdown().await;
        assert!(
            freight_queue(&b_space_a).recv().await.is_none(),
            "a shut-down Space answers None rather than parking"
        );

        let peer_b = mechanics::actor::device_from_seed(&seed_b);
        let dialed = dial_session(&a_space_a, peer_b, &space_a).await;
        tokio::time::timeout(Duration::from_secs(5), dialed.closed())
            .await
            .expect("refused in time");

        factory.shutdown().await;
        assert!(
            a_space_a
                .connect_session(
                    mechanics::actor::device_from_seed(&seed_b),
                    runtime::plane::FREIGHT_ALPN
                )
                .await
                .is_err(),
            "a shut-down hub refuses new dials"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn one_identity_endpoint_demultiplexes_and_replays_openers_by_space() {
        let inner = Arc::new(MemFactory {
            net: MemNet::new(),
            builds: AtomicUsize::new(0),
        });
        let factory = TransportHubFactory::new(inner.clone());
        let network = Network::Isolated;
        let seed_a = [31; 32];
        let seed_b = [32; 32];
        let space_a = space(1);
        let space_b = space(2);

        let a_space_a = factory
            .build_scoped(&seed_a, &network, protocols(), &space_a)
            .await
            .unwrap();
        let a_space_b = factory
            .build_scoped(&seed_a, &network, protocols(), &space_b)
            .await
            .unwrap();
        let b_space_a = factory
            .build_scoped(&seed_b, &network, protocols(), &space_a)
            .await
            .unwrap();
        let b_space_b = factory
            .build_scoped(&seed_b, &network, protocols(), &space_b)
            .await
            .unwrap();
        assert_eq!(
            inner.builds.load(Ordering::SeqCst),
            2,
            "one concrete endpoint is built per device identity, not per Space"
        );

        let peer_b = mechanics::actor::device_from_seed(&seed_b);
        let responder = peer_b.key_bytes().unwrap();
        let hello = runtime::plane::contact::Offer::sign(
            [0u8; 32],
            runtime::plane::contact::CONTACT_PROTOCOL,
            space_bytes(&space_a),
            responder,
            [9; 32],
            runtime::plane::contact::ContactId::from_bytes([7; 16]),
            [0; 32],
            0,
            [0; 32],
            &seed_a,
        )
        .unwrap()
        .encode();
        let mut contact = a_space_a
            .connect(peer_b.clone(), runtime::plane::contact::CONTACT_ALPN)
            .await
            .unwrap();
        contact.send(&hello).await.unwrap();

        let mut incoming = tokio::time::timeout(Duration::from_secs(1), b_space_a.accept())
            .await
            .expect("Space A receives its Contact")
            .expect("Space A queue remains open");
        assert_eq!(incoming.alpn, runtime::plane::contact::CONTACT_ALPN);
        assert_eq!(
            incoming.stream.recv().await.unwrap(),
            Some(hello),
            "the hub replays the exact opening frame for Runtime verification"
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(50), b_space_b.accept())
                .await
                .is_err(),
            "Space B must not consume Space A's Contact"
        );

        let probe = runtime::neighbor::PresenceProbe::sign(
            runtime::neighbor::PRESENCE_PROTOCOL,
            space_bytes(&space_b),
            responder,
            [8; 32],
            &seed_a,
        )
        .unwrap()
        .encode();
        let mut presence = a_space_b
            .connect(peer_b, runtime::neighbor::PRESENCE_ALPN)
            .await
            .unwrap();
        presence.send(&probe).await.unwrap();
        let mut incoming = tokio::time::timeout(Duration::from_secs(1), b_space_b.accept())
            .await
            .expect("Space B receives its presence probe")
            .expect("Space B queue remains open");
        assert_eq!(incoming.stream.recv().await.unwrap(), Some(probe));

        factory.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn scoped_shutdown_and_slow_openers_do_not_take_down_sibling_spaces() {
        let inner = Arc::new(MemFactory {
            net: MemNet::new(),
            builds: AtomicUsize::new(0),
        });
        let factory = TransportHubFactory::new(inner);
        let network = Network::Isolated;
        let seed_a = [41; 32];
        let seed_b = [42; 32];
        let space_a = space(3);
        let space_b = space(4);

        let a_space_a = factory
            .build_scoped(&seed_a, &network, protocols(), &space_a)
            .await
            .unwrap();
        let a_space_b = factory
            .build_scoped(&seed_a, &network, protocols(), &space_b)
            .await
            .unwrap();
        let b_space_a = factory
            .build_scoped(&seed_b, &network, protocols(), &space_a)
            .await
            .unwrap();
        let b_space_b = factory
            .build_scoped(&seed_b, &network, protocols(), &space_b)
            .await
            .unwrap();
        let duplicate = match factory
            .build_scoped(&seed_a, &network, protocols(), &space_a)
            .await
        {
            Ok(_) => panic!("duplicate identity/Space registration must fail"),
            Err(error) => error,
        };
        assert!(duplicate
            .to_string()
            .contains("already has an active Station"));

        let peer_b = mechanics::actor::device_from_seed(&seed_b);
        let responder = peer_b.key_bytes().unwrap();
        let _slow = a_space_a
            .connect(peer_b.clone(), runtime::plane::contact::CONTACT_ALPN)
            .await
            .unwrap();

        let probe = runtime::neighbor::PresenceProbe::sign(
            runtime::neighbor::PRESENCE_PROTOCOL,
            space_bytes(&space_b),
            responder,
            [6; 32],
            &seed_a,
        )
        .unwrap()
        .encode();
        let mut presence = a_space_b
            .connect(peer_b.clone(), runtime::neighbor::PRESENCE_ALPN)
            .await
            .unwrap();
        presence.send(&probe).await.unwrap();
        let _incoming = tokio::time::timeout(Duration::from_secs(1), b_space_b.accept())
            .await
            .expect("a slow Space A opener cannot head-of-line block Space B")
            .expect("Space B queue remains open");

        a_space_a.shutdown().await;
        assert!(
            a_space_a
                .connect(peer_b.clone(), runtime::neighbor::PRESENCE_ALPN)
                .await
                .is_err(),
            "a dormant Space cannot keep dialing"
        );
        let mut next = a_space_b
            .connect(peer_b, runtime::neighbor::PRESENCE_ALPN)
            .await
            .expect("the sibling Space retains the shared endpoint");
        next.send(&probe).await.unwrap();
        let _incoming = tokio::time::timeout(Duration::from_secs(1), b_space_b.accept())
            .await
            .expect("sibling Space still receives")
            .expect("Space B queue remains open");

        b_space_a.shutdown().await;
        factory.shutdown().await;
    }
}
