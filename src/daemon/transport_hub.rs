//! Identity-keyed transport ownership and inbound Space demultiplexing.
//!
//! One device identity owns one concrete transport endpoint. Each active
//! SpaceBridge receives a scoped view: outbound work and gossip delegate to the
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

use crate::ids::{DeviceId, SpaceId};
use crate::net::Network;
use crate::transport::{
    Alpn, GossipReceiver, GossipSender, Incoming, PeerId, Stream, Topic, Transport,
    TransportFactory,
};

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
        _alpns: &[Alpn],
    ) -> Result<Arc<dyn Transport>> {
        Err(anyhow!(
            "the identity transport hub requires an explicit Space scope"
        ))
    }

    async fn build_scoped(
        &self,
        identity_seed: &[u8; 32],
        network: &Network,
        alpns: &[Alpn],
        space: &SpaceId,
    ) -> Result<Arc<dyn Transport>> {
        if self.stopping.load(Ordering::Acquire) {
            return Err(anyhow!("the identity transport hub is shutting down"));
        }
        let identity = crate::crypto::device_from_seed(identity_seed);
        let slot = self.slot(identity.clone());
        let mut occupied = slot.lock().await;
        if self.stopping.load(Ordering::Acquire) {
            return Err(anyhow!("the identity transport hub is shutting down"));
        }

        let hub = match occupied.as_ref() {
            Some(hub) => {
                hub.require_compatible(network, alpns)?;
                hub.clone()
            }
            None => {
                let transport = self.inner.build(identity_seed, network, alpns).await?;
                if transport.my_id() != identity {
                    transport.shutdown().await;
                    return Err(anyhow!(
                        "transport factory returned identity {}, expected {}",
                        transport.my_id(),
                        identity
                    ));
                }
                let hub = IdentityTransportHub::start(transport, network, alpns);
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

fn normalized_alpns(alpns: &[Alpn]) -> Vec<Vec<u8>> {
    let mut values: Vec<_> = alpns.iter().map(|alpn| alpn.to_vec()).collect();
    values.sort();
    values.dedup();
    values
}

#[derive(Clone)]
struct RouteTarget {
    token: u64,
    incoming: mpsc::Sender<Incoming>,
    stopping: watch::Sender<bool>,
}

struct IdentityTransportHub {
    transport: Arc<dyn Transport>,
    network: NetworkKey,
    alpns: Vec<Vec<u8>>,
    routes: Arc<StdMutex<HashMap<SpaceBytes, RouteTarget>>>,
    next_token: AtomicU64,
    stopping: watch::Sender<bool>,
    accept_task: StdMutex<Option<tokio::task::JoinHandle<()>>>,
}

impl IdentityTransportHub {
    fn start(transport: Arc<dyn Transport>, network: &Network, alpns: &[Alpn]) -> Arc<Self> {
        let hub = Arc::new(Self {
            transport: transport.clone(),
            network: NetworkKey::from(network),
            alpns: normalized_alpns(alpns),
            routes: Arc::new(StdMutex::new(HashMap::new())),
            next_token: AtomicU64::new(1),
            stopping: watch::Sender::new(false),
            accept_task: StdMutex::new(None),
        });
        let task = tokio::spawn(run_accept_pump(
            transport,
            hub.routes.clone(),
            hub.stopping.subscribe(),
        ));
        *hub.accept_task
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(task);
        hub
    }

    fn require_compatible(&self, network: &Network, alpns: &[Alpn]) -> Result<()> {
        let requested_network = NetworkKey::from(network);
        if self.network != requested_network {
            return Err(anyhow!(
                "one device identity cannot use two network policies in one Lait daemon \
                 ({:?} and {:?})",
                self.network,
                requested_network
            ));
        }
        let requested_alpns = normalized_alpns(alpns);
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
        let stopping = watch::Sender::new(false);
        let target = RouteTarget {
            token,
            incoming: incoming_tx,
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
        let task = self
            .accept_task
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take();
        if let Some(task) = task {
            if let Err(error) = task.await {
                tracing::debug!(%error, "identity transport accept pump failed");
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
    if alpn == runtime::contact::CONTACT_ALPN {
        Some(runtime::contact::MAX_FRAME)
    } else if alpn == runtime::PRESENCE_ALPN {
        Some(runtime::neighbor_presence::MAX_MESSAGE)
    } else {
        None
    }
}

fn opening_space(alpn: &[u8], first: &[u8]) -> Option<SpaceBytes> {
    if alpn == runtime::contact::CONTACT_ALPN {
        if first.len() > runtime::contact::MAX_FRAME {
            return None;
        }
        runtime::ContactHello::decode(first)
            .ok()
            .map(|hello| hello.space)
    } else if alpn == runtime::PRESENCE_ALPN {
        runtime::PresenceProbe::decode(first)
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
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::transport::mem::MemNet;

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
            _alpns: &[Alpn],
        ) -> Result<Arc<dyn Transport>> {
            self.builds.fetch_add(1, Ordering::SeqCst);
            Ok(Arc::new(
                self.net
                    .peer(crate::crypto::device_from_seed(identity_seed)),
            ))
        }
    }

    fn space(n: u8) -> SpaceId {
        SpaceId::from_digest([n; 16])
    }

    fn space_bytes(space: &SpaceId) -> SpaceBytes {
        SpaceBytes::try_from(space.as_str().as_bytes()).unwrap()
    }

    const ALPNS: &[Alpn] = &[runtime::contact::CONTACT_ALPN, runtime::PRESENCE_ALPN];

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
            .build_scoped(&seed_a, &network, ALPNS, &space_a)
            .await
            .unwrap();
        let a_space_b = factory
            .build_scoped(&seed_a, &network, ALPNS, &space_b)
            .await
            .unwrap();
        let b_space_a = factory
            .build_scoped(&seed_b, &network, ALPNS, &space_a)
            .await
            .unwrap();
        let b_space_b = factory
            .build_scoped(&seed_b, &network, ALPNS, &space_b)
            .await
            .unwrap();
        assert_eq!(
            inner.builds.load(Ordering::SeqCst),
            2,
            "one concrete endpoint is built per device identity, not per Space"
        );

        let peer_b = crate::crypto::device_from_seed(&seed_b);
        let responder = peer_b.key_bytes().unwrap();
        let hello = runtime::ContactHello::sign(
            runtime::contact::CONTACT_PROTOCOL,
            space_bytes(&space_a),
            responder,
            [9; 32],
            runtime::ContactId::from_bytes([7; 16]),
            0,
            [0; 32],
            &seed_a,
        )
        .unwrap()
        .encode();
        let mut contact = a_space_a
            .connect(peer_b.clone(), runtime::contact::CONTACT_ALPN)
            .await
            .unwrap();
        contact.send(&hello).await.unwrap();

        let mut incoming = tokio::time::timeout(Duration::from_secs(1), b_space_a.accept())
            .await
            .expect("Space A receives its Contact")
            .expect("Space A queue remains open");
        assert_eq!(incoming.alpn, runtime::contact::CONTACT_ALPN);
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

        let probe = runtime::PresenceProbe::sign(
            runtime::PRESENCE_PROTOCOL,
            space_bytes(&space_b),
            responder,
            [8; 32],
            &seed_a,
        )
        .unwrap()
        .encode();
        let mut presence = a_space_b
            .connect(peer_b, runtime::PRESENCE_ALPN)
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
            .build_scoped(&seed_a, &network, ALPNS, &space_a)
            .await
            .unwrap();
        let a_space_b = factory
            .build_scoped(&seed_a, &network, ALPNS, &space_b)
            .await
            .unwrap();
        let b_space_a = factory
            .build_scoped(&seed_b, &network, ALPNS, &space_a)
            .await
            .unwrap();
        let b_space_b = factory
            .build_scoped(&seed_b, &network, ALPNS, &space_b)
            .await
            .unwrap();
        let duplicate = match factory
            .build_scoped(&seed_a, &network, ALPNS, &space_a)
            .await
        {
            Ok(_) => panic!("duplicate identity/Space registration must fail"),
            Err(error) => error,
        };
        assert!(duplicate
            .to_string()
            .contains("already has an active Station"));

        let peer_b = crate::crypto::device_from_seed(&seed_b);
        let responder = peer_b.key_bytes().unwrap();
        let _slow = a_space_a
            .connect(peer_b.clone(), runtime::contact::CONTACT_ALPN)
            .await
            .unwrap();

        let probe = runtime::PresenceProbe::sign(
            runtime::PRESENCE_PROTOCOL,
            space_bytes(&space_b),
            responder,
            [6; 32],
            &seed_a,
        )
        .unwrap()
        .encode();
        let mut presence = a_space_b
            .connect(peer_b.clone(), runtime::PRESENCE_ALPN)
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
                .connect(peer_b.clone(), runtime::PRESENCE_ALPN)
                .await
                .is_err(),
            "a dormant Space cannot keep dialing"
        );
        let mut next = a_space_b
            .connect(peer_b, runtime::PRESENCE_ALPN)
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
