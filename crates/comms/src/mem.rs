//! An **in-process, deterministic** [`Transport`] — the whole network in one
//! process, over channels, with no iroh and no sockets.
//!
//! This is what makes the *real daemon* testable hermetically: build N
//! [`MemTransport`]s off one [`MemNet`] switchboard, hand each to a daemon, and
//! they dial/gossip/accept through the same code paths as production — but the
//! "network" is a `HashMap` and some channels, so it is offline, instant, and
//! reproducible on every OS. It is the seed of the deterministic network
//! simulator (controllable delivery: drop/delay/partition) sketched in the
//! testing scope; this draft is the connectivity core.
//!
//! Contract fidelity notes (the iroh impl is the contract where they diverge):
//! frames travel whole over channels, so mem can never truncate — the
//! *truncation consequence* of skipping [`Stream::wait_closed`] is only
//! observable on iroh, but the *ordering* obligation (the accepter parks until
//! the dialer is done and drops) is modeled faithfully here. `connect` succeeds
//! whenever the peer is *registered* on the switchboard; liveness is
//! membership, so a "down" peer must have been [`Transport::shutdown`].

use std::collections::{BTreeSet, HashMap};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex as StdMutex};

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use tokio::sync::{broadcast, mpsc, watch, Mutex as TokioMutex};

use super::{
    Alpn, GossipEvent, GossipReceiver, GossipSender, Incoming, IncomingConnection, PeerId,
    RecvFlow, SendFlow, Stream, Topic, Transport,
};

/// The shared switchboard every in-memory peer is wired to. Cloneable; all clones
/// share one registry, so peers created from it can reach each other.
#[derive(Clone, Default)]
pub struct MemNet(Arc<StdMutex<Inner>>);

#[derive(Default)]
struct Inner {
    /// Inbound-connection inbox per peer.
    peers: HashMap<PeerId, mpsc::UnboundedSender<Incoming>>,
    /// Inbound multi-flow-connection inbox per peer. Separate from `peers`
    /// because a connection handed over whole and a connection wrapped in a
    /// framed stream are two different deliveries, and one consumes it.
    sessions: HashMap<PeerId, mpsc::UnboundedSender<IncomingConnection>>,
    /// One broadcast bus per gossip topic.
    topics: HashMap<Topic, broadcast::Sender<TopicMsg>>,
    /// Delivery policy. `None` is a perfect network and costs nothing —
    /// `MemNet::new` stays exactly what it was.
    faults: Option<(Faults, Seeded)>,
    /// Unordered peer pairs that cannot hear each other, held smallest-first
    /// so a partition is symmetric by construction rather than by discipline.
    /// `BTreeSet` rather than `HashSet` because this is iterated when healing,
    /// and a hash order is one more thing that would vary between runs.
    partitions: BTreeSet<(PeerId, PeerId)>,
    /// Counters, for a simulation to assert it did what it says.
    delivered: Delivered,
}

#[derive(Clone)]
enum TopicMsg {
    Join(PeerId),
    Data(PeerId, Vec<u8>),
}

/// How reliably this network delivers.
///
/// The default is a perfect network — every existing caller of [`MemNet::new`]
/// gets exactly the behaviour it had before this existed, which is the point:
/// a fault injector that changes the default silently rewrites thirty tests'
/// meaning.
///
/// Faults are drawn from ONE seeded generator, so a run is reproducible from
/// its seed. That is the whole reason this is here rather than in a test
/// helper: a simulation whose faults come from `rand::thread_rng` finds bugs
/// nobody can reproduce.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Faults {
    /// Percent of deliveries silently lost. The sender believes it sent.
    pub drop_percent: u8,
    /// Percent of deliveries made twice. A receiver that is not idempotent
    /// fails here rather than in production.
    pub duplicate_percent: u8,
}

impl Faults {
    /// Delivers everything, twice-delivers nothing.
    pub const PERFECT: Self = Self {
        drop_percent: 0,
        duplicate_percent: 0,
    };

    /// A plausibly bad network: one delivery in eight lost, one in twenty
    /// doubled. Chosen to be frequent enough that a short simulation actually
    /// exercises loss, rather than realistic — a realistic rate would need a
    /// very long run to find anything.
    pub const LOSSY: Self = Self {
        drop_percent: 12,
        duplicate_percent: 5,
    };
}

impl Default for Faults {
    fn default() -> Self {
        Self::PERFECT
    }
}

/// splitmix64. Written out rather than taken as a dependency so this network's
/// entropy has exactly one source: a crate that seeds itself, or a `HashMap`
/// iteration order, silently reintroduces the nondeterminism a seed exists to
/// remove — and the failure mode is a bug report nobody can replay.
#[derive(Debug, Clone, Copy)]
struct Seeded(u64);

impl Seeded {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn percent(&mut self, chance: u8) -> bool {
        chance > 0 && self.next() % 100 < u64::from(chance)
    }
}

/// What a run actually did. Asserting on these is how a simulation proves it
/// injected the faults it claims to have injected — a "chaos" test that never
/// dropped anything is a slow way of testing the happy path.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Delivered {
    pub sent: u64,
    pub dropped: u64,
    pub duplicated: u64,
    pub partitioned: u64,
}

impl MemNet {
    /// A perfect network. Unchanged: every delivery lands, exactly once.
    pub fn new() -> Self {
        Self::default()
    }

    /// A network that loses and doubles deliveries, from a seed.
    ///
    /// The seed is the only entropy, so a failure reproduces exactly — print it
    /// and the run can be replayed on any machine. That is the property the
    /// whole technique rests on, and the reason faults live here rather than in
    /// whatever test happens to want them.
    pub fn seeded(seed: u64, faults: Faults) -> Self {
        let net = Self::default();
        net.with_inner(|inner| inner.faults = Some((faults, Seeded(seed))));
        net
    }

    /// Cut `a` and `b` off from each other, in both directions.
    ///
    /// Symmetric because a real partition is: there is no link that carries
    /// packets one way. Storing the pair smallest-first makes that a property
    /// of the representation rather than of remembering to insert twice.
    pub fn partition(&self, a: &PeerId, b: &PeerId) {
        let pair = Self::pair(a, b);
        self.with_inner(|inner| {
            inner.partitions.insert(pair);
        });
    }

    /// Reconnect everything. The simulation's healing phase.
    pub fn heal(&self) {
        self.with_inner(|inner| inner.partitions.clear());
    }

    /// What this network actually did.
    pub fn delivered(&self) -> Delivered {
        self.with_inner(|inner| inner.delivered)
    }

    fn pair(a: &PeerId, b: &PeerId) -> (PeerId, PeerId) {
        if a <= b {
            (a.clone(), b.clone())
        } else {
            (b.clone(), a.clone())
        }
    }

    fn with_inner<T>(&self, f: impl FnOnce(&mut Inner) -> T) -> T {
        let mut inner = self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        f(&mut inner)
    }

    /// Whether `from` can currently reach `to`, and what the policy says about
    /// this particular delivery. Counted as it decides, so the totals are what
    /// happened rather than what was intended.
    fn admit(&self, from: &PeerId, to: &PeerId) -> Verdict {
        let pair = Self::pair(from, to);
        self.with_inner(|inner| {
            inner.delivered.sent = inner.delivered.sent.saturating_add(1);
            if inner.partitions.contains(&pair) {
                inner.delivered.partitioned = inner.delivered.partitioned.saturating_add(1);
                return Verdict::Blocked;
            }
            let Some((faults, rng)) = inner.faults.as_mut() else {
                return Verdict::Once;
            };
            if rng.percent(faults.drop_percent) {
                inner.delivered.dropped = inner.delivered.dropped.saturating_add(1);
                return Verdict::Blocked;
            }
            if rng.percent(faults.duplicate_percent) {
                inner.delivered.duplicated = inner.delivered.duplicated.saturating_add(1);
                return Verdict::Twice;
            }
            Verdict::Once
        })
    }

    /// Whether a gossip frame from `from` reaches `me`.
    ///
    /// Gossip is decided at the RECEIVER, unlike a direct connection: a
    /// broadcast goes to a bus rather than to a peer, so the sender does not
    /// know who is listening. Filtering per subscriber is also the more
    /// faithful model — a partition means each side independently stops
    /// hearing the other, not that the message was never spoken.
    fn gossip_reaches(&self, from: &PeerId, me: &PeerId) -> bool {
        if from == me {
            return true;
        }
        let pair = Self::pair(from, me);
        self.with_inner(|inner| {
            // Counted here, not at the sender: a broadcast has one send and N
            // possible deliveries, and it is the deliveries that either happen
            // or do not. Counting the broadcast instead would report `sent=1`
            // beside `dropped=4`, which reads as a contradiction.
            inner.delivered.sent = inner.delivered.sent.saturating_add(1);
            if inner.partitions.contains(&pair) {
                inner.delivered.partitioned = inner.delivered.partitioned.saturating_add(1);
                return false;
            }
            let Some((faults, rng)) = inner.faults.as_mut() else {
                return true;
            };
            if rng.percent(faults.drop_percent) {
                inner.delivered.dropped = inner.delivered.dropped.saturating_add(1);
                return false;
            }
            true
        })
    }

    /// Attach a new peer `id` to this network and return its transport.
    pub fn peer(&self, id: PeerId) -> MemTransport {
        let (tx, rx) = mpsc::unbounded_channel();
        let (session_tx, session_rx) = mpsc::unbounded_channel();
        {
            let mut inner = self
                .0
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            inner.peers.insert(id.clone(), tx);
            inner.sessions.insert(id.clone(), session_tx);
        }
        MemTransport {
            id,
            net: self.clone(),
            incoming: TokioMutex::new(rx),
            incoming_sessions: TokioMutex::new(session_rx),
        }
    }

    fn topic_bus(&self, topic: Topic) -> broadcast::Sender<TopicMsg> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .topics
            .entry(topic)
            .or_insert_with(|| broadcast::channel(256).0)
            .clone()
    }
}

/// What the policy decided about one delivery.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verdict {
    /// Lost, or the pair is partitioned. The sender is told nothing — a real
    /// dropped packet does not report itself.
    Blocked,
    Once,
    Twice,
}

/// One peer's view of the in-memory network.
pub struct MemTransport {
    id: PeerId,
    net: MemNet,
    incoming: TokioMutex<mpsc::UnboundedReceiver<Incoming>>,
    incoming_sessions: TokioMutex<mpsc::UnboundedReceiver<IncomingConnection>>,
}

/// A framed duplex stream backed by a pair of channels.
struct MemStream {
    /// `None` after [`Stream::finish`]: dropping the sender is the end-of-stream
    /// marker the peer's `recv` sees as `Ok(None)` once drained — real FIN
    /// semantics, matching the iroh impl.
    tx: Option<mpsc::UnboundedSender<Vec<u8>>>,
    rx: mpsc::UnboundedReceiver<Vec<u8>>,
    /// Held only so its drop marks *this whole handle* gone — the peer's
    /// [`Stream::wait_closed`] parks on it. Distinct from `tx` because `finish`
    /// must drop `tx` without counting as "closed".
    _alive: mpsc::UnboundedReceiver<()>,
    /// Never sent on; its `closed()` resolves exactly when the peer drops its
    /// `_alive` half, i.e. drops its whole stream handle.
    peer_alive: mpsc::UnboundedSender<()>,
}

fn duplex() -> (MemStream, MemStream) {
    let (a_tx, a_rx) = mpsc::unbounded_channel();
    let (b_tx, b_rx) = mpsc::unbounded_channel();
    let (a_alive_tx, a_alive_rx) = mpsc::unbounded_channel();
    let (b_alive_tx, b_alive_rx) = mpsc::unbounded_channel();
    (
        MemStream {
            tx: Some(a_tx),
            rx: b_rx,
            _alive: a_alive_rx,
            peer_alive: b_alive_tx,
        },
        MemStream {
            tx: Some(b_tx),
            rx: a_rx,
            _alive: b_alive_rx,
            peer_alive: a_alive_tx,
        },
    )
}

#[async_trait]
impl Stream for MemStream {
    async fn send(&mut self, frame: &[u8]) -> Result<()> {
        self.tx
            .as_ref()
            .ok_or_else(|| anyhow!("stream already finished"))?
            .send(frame.to_vec())
            .map_err(|_| anyhow!("peer stream closed"))
    }
    async fn recv(&mut self) -> Result<Option<Vec<u8>>> {
        // Whole frames over a channel: an end is always at a frame boundary, so
        // mem never surfaces the mid-frame truncation `Err` the iroh impl can.
        Ok(self.rx.recv().await)
    }
    async fn finish(&mut self) -> Result<()> {
        self.tx = None; // dropping the sender delivers end-of-stream after drain
        Ok(())
    }
    async fn wait_closed(&mut self) {
        // Resolves when the peer drops its `_alive` receiver — which happens
        // only when the peer drops its whole stream handle. The ordering half
        // of the accepter contract (park until the dialer is done and drops).
        self.peer_alive.closed().await;
    }
}

/// One end of an in-memory multi-flow connection.
///
/// Models what Runtime tests actually depend on: flows arrive in the order they
/// were opened, a finish and a reset are different endings, datagrams are
/// unreliable in principle and refused when oversized, and closing wakes
/// everyone parked. It does not model loss or reordering — the iroh
/// implementation is the contract where the two diverge, and the cross-
/// implementation tests are what keep the divergence honest.
struct MemConnection {
    peer: PeerId,
    alpn: Vec<u8>,
    /// Flows this end opens, delivered to the far end. QUIC has independent
    /// accept queues for bidirectional and unidirectional streams; modelling
    /// them as one queue made an `accept_bi` consume and reject a uni stream.
    bi_open_tx: mpsc::UnboundedSender<MemFlowHandoff>,
    uni_open_tx: mpsc::UnboundedSender<MemFlowHandoff>,
    /// Flows the far end opened, separated by direction like the shipped QUIC
    /// implementation so both accept futures may be polled concurrently.
    bi_open_rx: TokioMutex<mpsc::UnboundedReceiver<MemFlowHandoff>>,
    uni_open_rx: TokioMutex<mpsc::UnboundedReceiver<MemFlowHandoff>>,
    datagram_tx: mpsc::UnboundedSender<Vec<u8>>,
    datagram_rx: TokioMutex<mpsc::UnboundedReceiver<Vec<u8>>>,
    /// This end's own closed state. A watch rather than a notify because a
    /// parked accept has to see a close that happened before it parked — a
    /// notification that fires into an empty room is how a shutdown deadline
    /// becomes a hang.
    close: watch::Sender<bool>,
    /// The other end's, so either side closing wakes both.
    peer_close: watch::Receiver<bool>,
}

impl MemConnection {
    /// Resolves once either end has closed.
    async fn until_closed(&self) {
        let mut mine = self.close.subscribe();
        let mut theirs = self.peer_close.clone();
        if *mine.borrow() || *theirs.borrow() {
            return;
        }
        tokio::select! {
            _ = mine.wait_for(|closed| *closed) => {}
            _ = theirs.wait_for(|closed| *closed) => {}
        }
    }
}

/// What travels when one end opens a flow: the far end's halves.
struct MemFlowHandoff {
    send: Option<MemSendFlow>,
    recv: MemRecvFlow,
}

/// The datagram ceiling the in-memory transport reports.
///
/// A fixed number, and deliberately one that a caller must still ask for rather
/// than assume — the real limit is path-dependent, so code that reads this
/// constant at send time is code that will keep working when it is not.
const MEM_DATAGRAM_CAPACITY: usize = 1_200;

fn flow_pair() -> (MemSendFlow, MemRecvFlow, MemSendFlow, MemRecvFlow) {
    let (a_tx, a_rx) = mpsc::unbounded_channel();
    let (b_tx, b_rx) = mpsc::unbounded_channel();
    (
        MemSendFlow {
            tx: Some(a_tx),
            pending: None,
        },
        MemRecvFlow {
            rx: b_rx,
            buffered: Vec::new(),
            reset: false,
        },
        MemSendFlow {
            tx: Some(b_tx),
            pending: None,
        },
        MemRecvFlow {
            rx: a_rx,
            buffered: Vec::new(),
            reset: false,
        },
    )
}

fn connection_pair(dialer: PeerId, accepter: PeerId, alpn: Alpn) -> (MemConnection, MemConnection) {
    let (a_bi_open_tx, a_bi_open_rx) = mpsc::unbounded_channel();
    let (b_bi_open_tx, b_bi_open_rx) = mpsc::unbounded_channel();
    let (a_uni_open_tx, a_uni_open_rx) = mpsc::unbounded_channel();
    let (b_uni_open_tx, b_uni_open_rx) = mpsc::unbounded_channel();
    let (a_dg_tx, a_dg_rx) = mpsc::unbounded_channel();
    let (b_dg_tx, b_dg_rx) = mpsc::unbounded_channel();
    let a_close = watch::Sender::new(false);
    let b_close = watch::Sender::new(false);
    let a_close_rx = a_close.subscribe();
    let b_close_rx = b_close.subscribe();
    (
        MemConnection {
            peer: accepter,
            alpn: alpn.to_vec(),
            bi_open_tx: b_bi_open_tx,
            uni_open_tx: b_uni_open_tx,
            bi_open_rx: TokioMutex::new(a_bi_open_rx),
            uni_open_rx: TokioMutex::new(a_uni_open_rx),
            datagram_tx: b_dg_tx,
            datagram_rx: TokioMutex::new(a_dg_rx),
            close: a_close,
            peer_close: b_close_rx,
        },
        MemConnection {
            peer: dialer,
            alpn: alpn.to_vec(),
            bi_open_tx: a_bi_open_tx,
            uni_open_tx: a_uni_open_tx,
            bi_open_rx: TokioMutex::new(b_bi_open_rx),
            uni_open_rx: TokioMutex::new(b_uni_open_rx),
            datagram_tx: a_dg_tx,
            datagram_rx: TokioMutex::new(b_dg_rx),
            close: b_close,
            peer_close: a_close_rx,
        },
    )
}

#[async_trait]
impl super::Connection for MemConnection {
    fn peer(&self) -> PeerId {
        self.peer.clone()
    }

    fn alpn(&self) -> Vec<u8> {
        self.alpn.clone()
    }

    async fn open_bi(&self) -> Result<(Box<dyn SendFlow>, Box<dyn RecvFlow>)> {
        let (mut mine_send, mine_recv, theirs_send, theirs_recv) = flow_pair();
        // Deferred until the first write, because that is when a QUIC stream
        // becomes visible to the peer. Handing it over eagerly would make this
        // transport *laxer* than the wire, and every test above it would then
        // pass against a network that does not behave that way.
        mine_send.pending = Some((
            self.bi_open_tx.clone(),
            Box::new(MemFlowHandoff {
                send: Some(theirs_send),
                recv: theirs_recv,
            }),
        ));
        Ok((Box::new(mine_send), Box::new(mine_recv)))
    }

    async fn accept_bi(&self) -> Result<Option<(Box<dyn SendFlow>, Box<dyn RecvFlow>)>> {
        let mut rx = self.bi_open_rx.lock().await;
        let next = tokio::select! {
            item = rx.recv() => item,
            _ = self.until_closed() => None,
        };
        drop(rx);
        match next {
            Some(handoff) => {
                let send = handoff
                    .send
                    .ok_or_else(|| anyhow!("bidirectional flow lost send half"))?;
                let send: Box<dyn SendFlow> = Box::new(send);
                let recv: Box<dyn RecvFlow> = Box::new(handoff.recv);
                Ok(Some((send, recv)))
            }
            None => Ok(None),
        }
    }

    async fn open_uni(&self) -> Result<Box<dyn SendFlow>> {
        let (mut mine_send, _mine_recv, _theirs_send, theirs_recv) = flow_pair();
        mine_send.pending = Some((
            self.uni_open_tx.clone(),
            Box::new(MemFlowHandoff {
                send: None,
                recv: theirs_recv,
            }),
        ));
        Ok(Box::new(mine_send))
    }

    async fn accept_uni(&self) -> Result<Option<Box<dyn RecvFlow>>> {
        let mut rx = self.uni_open_rx.lock().await;
        let next = tokio::select! {
            item = rx.recv() => item,
            _ = self.until_closed() => None,
        };
        drop(rx);
        match next {
            Some(handoff) => {
                let recv: Box<dyn RecvFlow> = Box::new(handoff.recv);
                Ok(Some(recv))
            }
            None => Ok(None),
        }
    }

    fn send_datagram(&self, payload: &[u8]) -> Result<()> {
        if payload.len() > MEM_DATAGRAM_CAPACITY {
            anyhow::bail!(
                "datagram of {} bytes exceeds the path capacity of {MEM_DATAGRAM_CAPACITY}",
                payload.len()
            );
        }
        self.datagram_tx
            .send(payload.to_vec())
            .map_err(|_| anyhow!("connection is closed"))
    }

    async fn read_datagram(&self) -> Result<Option<Vec<u8>>> {
        let mut rx = self.datagram_rx.lock().await;
        Ok(tokio::select! {
            item = rx.recv() => item,
            _ = self.until_closed() => None,
        })
    }

    fn datagram_capacity(&self) -> Option<usize> {
        Some(MEM_DATAGRAM_CAPACITY)
    }

    fn close(&self, _code: u32, _reason: &[u8]) {
        let _ = self.close.send(true);
    }

    async fn closed(&self) {
        self.until_closed().await;
    }
}

/// What one write put on a flow, or how it ended.
enum MemFlowItem {
    Bytes(Vec<u8>),
    Reset,
}

struct MemSendFlow {
    /// `None` after `finish`: dropping the sender is the clean end the peer
    /// sees, exactly as dropping a framed stream's sender is.
    tx: Option<mpsc::UnboundedSender<MemFlowItem>>,
    /// The peer's halves, not yet handed over. Delivered by the first write,
    /// finish, or reset — see [`Connection::open_bi`](crate::Connection::open_bi).
    pending: Option<(mpsc::UnboundedSender<MemFlowHandoff>, Box<MemFlowHandoff>)>,
}

impl MemSendFlow {
    fn announce(&mut self) -> Result<()> {
        if let Some((opener, handoff)) = self.pending.take() {
            opener
                .send(*handoff)
                .map_err(|_| anyhow!("connection is closed"))?;
        }
        Ok(())
    }
}

#[async_trait]
impl SendFlow for MemSendFlow {
    async fn write_all(&mut self, bytes: &[u8]) -> Result<()> {
        self.announce()?;
        self.tx
            .as_ref()
            .ok_or_else(|| anyhow!("flow already finished"))?
            .send(MemFlowItem::Bytes(bytes.to_vec()))
            .map_err(|_| anyhow!("peer stopped the flow"))
    }

    fn finish(&mut self) -> Result<()> {
        self.announce()?;
        self.tx = None;
        Ok(())
    }

    fn reset(&mut self, _code: u32) {
        let _ = self.announce();
        // A reset has to be distinguishable from a finish at the receiver, so
        // it is an item rather than a drop. Truncation is loud.
        if let Some(tx) = self.tx.take() {
            let _ = tx.send(MemFlowItem::Reset);
        }
    }
}

struct MemRecvFlow {
    rx: mpsc::UnboundedReceiver<MemFlowItem>,
    /// Bytes read from the channel but not yet handed to the caller. A caller
    /// asking for fewer bytes than one write produced must not lose the rest.
    buffered: Vec<u8>,
    reset: bool,
}

#[async_trait]
impl RecvFlow for MemRecvFlow {
    async fn read_chunk(&mut self, max: usize) -> Result<Option<Vec<u8>>> {
        if self.reset {
            anyhow::bail!("flow was reset by the peer");
        }
        if self.buffered.is_empty() {
            match self.rx.recv().await {
                Some(MemFlowItem::Bytes(bytes)) => self.buffered = bytes,
                Some(MemFlowItem::Reset) => {
                    self.reset = true;
                    anyhow::bail!("flow was reset by the peer");
                }
                None => return Ok(None),
            }
        }
        let take = max.min(self.buffered.len());
        let out: Vec<u8> = self.buffered.drain(..take).collect();
        Ok(Some(out))
    }

    fn stop(&mut self, _code: u32) {
        self.rx.close();
    }
}

/// The send half of a joined room: a publisher on the topic's broadcast bus.
struct MemGossipSender {
    me: PeerId,
    bus: broadcast::Sender<TopicMsg>,
}

#[async_trait]
impl GossipSender for MemGossipSender {
    async fn broadcast(&self, bytes: Vec<u8>) -> Result<()> {
        // Delivery to zero subscribers is fine (a solo node broadcasting).
        //
        // No fault check here: gossip is filtered at the RECEIVER. A broadcast
        // goes to a bus rather than to a peer, so the sender cannot know who
        // would have heard it — and per-subscriber filtering is the more
        // faithful model anyway, because a partition means each side
        // independently stops hearing the other rather than the message never
        // being spoken.
        let _ = self.bus.send(TopicMsg::Data(self.me.clone(), bytes));
        Ok(())
    }
}

/// The receive half: a subscriber on the topic's broadcast bus.
struct MemGossipReceiver {
    me: PeerId,
    /// Held so the receiver can ask the switchboard whether it should have
    /// heard this frame — see `MemNet::gossip_reaches`.
    net: MemNet,
    rx: broadcast::Receiver<TopicMsg>,
}

#[async_trait]
impl GossipReceiver for MemGossipReceiver {
    async fn next(&mut self) -> Option<GossipEvent> {
        loop {
            match self.rx.recv().await {
                Ok(TopicMsg::Data(from, bytes)) if from != self.me => {
                    // The policy decides here rather than at the sender: a
                    // broadcast has no destination, so "did this reach me" is a
                    // question only the receiver can answer.
                    if !self.net.gossip_reaches(&from, &self.me) {
                        continue;
                    }
                    return Some(GossipEvent::Received { from, bytes });
                }
                Ok(TopicMsg::Join(p)) if p != self.me => {
                    // A neighbour announcement across a partition is exactly
                    // the thing a partitioned peer must not learn.
                    if !self.net.gossip_reaches(&p, &self.me) {
                        continue;
                    }
                    return Some(GossipEvent::NeighborUp(p));
                }
                Ok(_) => continue, // our own frames
                Err(broadcast::error::RecvError::Lagged(_)) => continue, // lossy by contract
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    }
    async fn joined(&mut self) -> Result<()> {
        Ok(()) // the switchboard is always joined
    }
}

#[async_trait]
impl Transport for MemTransport {
    fn my_id(&self) -> PeerId {
        self.id.clone()
    }

    fn learn(&self, _peer: PeerId, _addrs: &[SocketAddr]) {
        // No-op: the switchboard resolves every peer by id.
    }

    async fn connect(&self, peer: PeerId, alpn: Alpn) -> Result<Box<dyn Stream>> {
        let inbox = self
            .net
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .peers
            .get(&peer)
            .cloned()
            .ok_or_else(|| anyhow!("no such peer on the in-memory network"))?;
        let (mine, theirs) = duplex();
        // A dropped dial returns a live handle that nobody is holding the other
        // end of, which is what a real lost SYN looks like to the dialer: no
        // error, no answer. Returning `Err` here would be the easier code and
        // the wrong model — the caller would learn something the network never
        // told it.
        match self.net.admit(&self.id, &peer) {
            Verdict::Blocked => return Ok(Box::new(mine)),
            Verdict::Once | Verdict::Twice => {}
        }
        inbox
            .send(Incoming {
                from: self.id.clone(),
                alpn: alpn.to_vec(),
                stream: Box::new(theirs),
            })
            .map_err(|_| anyhow!("peer is gone"))?;
        Ok(Box::new(mine))
    }

    async fn accept(&self) -> Option<Incoming> {
        self.incoming.lock().await.recv().await
    }

    async fn connect_session(
        &self,
        peer: PeerId,
        alpn: Alpn,
    ) -> Result<Box<dyn super::Connection>> {
        let inbox = self
            .net
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .sessions
            .get(&peer)
            .cloned()
            .ok_or_else(|| anyhow!("no such peer on the in-memory network"))?;
        let verdict = self.net.admit(&self.id, &peer);
        let (mine, theirs) = connection_pair(self.id.clone(), peer.clone(), alpn);
        if verdict == Verdict::Blocked {
            // As in `connect`: the dialer gets a handle nobody answers, which
            // is what a lost connection attempt looks like from this side.
            return Ok(Box::new(mine));
        }
        inbox
            .send(IncomingConnection {
                from: self.id.clone(),
                alpn: alpn.to_vec(),
                connection: Box::new(theirs),
                opening: Vec::new(),
            })
            .map_err(|_| anyhow!("peer is gone"))?;
        if verdict == Verdict::Twice {
            // A second, independent connection carrying the same opening — what
            // a dialer that retried a request the accepter had already received
            // produces. An accepter that is not idempotent about openings fails
            // here rather than against a real peer.
            let (_extra_mine, extra_theirs) = connection_pair(self.id.clone(), peer, alpn);
            let _ = inbox.send(IncomingConnection {
                from: self.id.clone(),
                alpn: alpn.to_vec(),
                connection: Box::new(extra_theirs),
                opening: Vec::new(),
            });
        }
        Ok(Box::new(mine))
    }

    async fn accept_connection(&self) -> Option<IncomingConnection> {
        self.incoming_sessions.lock().await.recv().await
    }

    fn advertised_addrs(&self) -> Vec<SocketAddr> {
        Vec::new() // the switchboard resolves by id — tickets stay address-free
    }

    async fn subscribe(
        &self,
        topic: Topic,
        _bootstrap: &[PeerId],
    ) -> Result<(Box<dyn GossipSender>, Box<dyn GossipReceiver>)> {
        let bus = self.net.topic_bus(topic);
        let rx = bus.subscribe();
        // Announce ourselves so already-subscribed peers see a NeighborUp.
        let _ = bus.send(TopicMsg::Join(self.id.clone()));
        Ok((
            Box::new(MemGossipSender {
                me: self.id.clone(),
                bus,
            }),
            Box::new(MemGossipReceiver {
                me: self.id.clone(),
                net: self.net.clone(),
                rx,
            }),
        ))
    }

    async fn shutdown(&self) {
        self.net
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .peers
            .remove(&self.id);
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    /// Any ALPN routes the same way here; the daemon's real protocol selectors
    /// are the application's to name, not the transport's.
    const TEST_ALPN: Alpn = b"comms/test/1";

    fn id(seed: u8) -> PeerId {
        mechanics::actor::device_from_seed(&[seed; 32])
    }

    #[tokio::test]
    async fn two_mem_peers_gossip_and_dial() {
        let net = MemNet::new();
        let a = net.peer(id(1));
        let b = net.peer(id(2));
        let topic = Topic([7u8; 32]);

        // B subscribes first, then A — A's Join should reach B as a NeighborUp.
        let (_b_send, mut b_recv) = b.subscribe(topic, &[]).await.unwrap();
        let (a_send, mut a_recv) = a.subscribe(topic, &[]).await.unwrap();
        // joined() resolves immediately — the switchboard is always joined.
        tokio::time::timeout(Duration::from_secs(1), a_recv.joined())
            .await
            .expect("joined must not block on mem")
            .unwrap();
        match b_recv.next().await {
            Some(GossipEvent::NeighborUp(p)) => assert_eq!(p, id(1)),
            other => panic!("expected NeighborUp(a), got {other:?}"),
        }

        // A broadcasts through the split-off sender WHILE its receiver is parked
        // in next() on another task — the concurrency shape the daemon needs
        // (heartbeat broadcasts while recv_loop consumes).
        let a_reader = tokio::spawn(async move { a_recv.next().await });
        a_send.broadcast(b"announce".to_vec()).await.unwrap();
        match b_recv.next().await {
            Some(GossipEvent::Received { from, bytes }) => {
                assert_eq!(from, id(1));
                assert_eq!(bytes, b"announce");
            }
            other => panic!("expected Received, got {other:?}"),
        }
        a_reader.abort(); // A never receives its own frames; stop the parked task.

        // Tickets stay address-free on the switchboard.
        assert!(a.advertised_addrs().is_empty());

        // A dials B directly; B accepts; a frame round-trips both ways.
        let b_accept = tokio::spawn(async move {
            let inc = b.accept().await.expect("incoming");
            assert_eq!(inc.from, id(1));
            assert_eq!(inc.alpn, TEST_ALPN);
            let mut s = inc.stream;
            assert_eq!(s.recv().await.unwrap().as_deref(), Some(&b"ping"[..]));
            s.send(b"pong").await.unwrap();
        });
        let mut s = a.connect(id(2), TEST_ALPN).await.unwrap();
        s.send(b"ping").await.unwrap();
        assert_eq!(s.recv().await.unwrap().as_deref(), Some(&b"pong"[..]));
        b_accept.await.unwrap();
    }

    /// `finish` delivers a real end-of-stream: the peer drains the queued
    /// frames, then sees `Ok(None)` — and a send after `finish` is an error.
    #[tokio::test]
    async fn finish_delivers_end_of_stream_after_drain() {
        let net = MemNet::new();
        let a = net.peer(id(1));
        let b = net.peer(id(2));

        let b_task = tokio::spawn(async move {
            let mut s = b.accept().await.expect("incoming").stream;
            assert_eq!(s.recv().await.unwrap().as_deref(), Some(&b"one"[..]));
            assert_eq!(s.recv().await.unwrap().as_deref(), Some(&b"two"[..]));
            assert_eq!(s.recv().await.unwrap(), None, "clean end after drain");
        });
        let mut s = a.connect(id(2), TEST_ALPN).await.unwrap();
        s.send(b"one").await.unwrap();
        s.send(b"two").await.unwrap();
        s.finish().await.unwrap();
        assert!(s.send(b"late").await.is_err(), "send after finish errors");
        b_task.await.unwrap();
    }

    /// The ordering half of the accepter contract (the part mem CAN model):
    /// `wait_closed` does not resolve while the dialer still holds its stream,
    /// and resolves promptly once the dialer drops it.
    #[tokio::test]
    async fn wait_closed_parks_until_dialer_drops() {
        let net = MemNet::new();
        let a = net.peer(id(1));
        let b = net.peer(id(2));

        let b_task = tokio::spawn(async move {
            let mut s = b.accept().await.expect("incoming").stream;
            s.send(b"payload").await.unwrap();
            s.finish().await.unwrap();
            // Must still be parked: the dialer holds its handle for 200ms.
            let parked = tokio::time::timeout(Duration::from_millis(100), s.wait_closed()).await;
            assert!(
                parked.is_err(),
                "wait_closed resolved while the dialer still held the stream"
            );
            tokio::time::timeout(Duration::from_secs(5), s.wait_closed())
                .await
                .expect("wait_closed must resolve after the dialer drops");
        });

        let mut s = a.connect(id(2), TEST_ALPN).await.unwrap();
        assert_eq!(s.recv().await.unwrap().as_deref(), Some(&b"payload"[..]));
        tokio::time::sleep(Duration::from_millis(200)).await;
        drop(s); // the dialer's "done" signal
        b_task.await.unwrap();
    }

    /// Liveness is switchboard membership: a shutdown peer fails `connect`.
    #[tokio::test]
    async fn connect_fails_after_peer_shutdown() {
        let net = MemNet::new();
        let a = net.peer(id(1));
        let b = net.peer(id(2));
        b.shutdown().await;
        assert!(a.connect(id(2), TEST_ALPN).await.is_err());
    }
}
