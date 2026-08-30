//! The carry: IP packets between a person's own devices over lait's own
//! transport.
//!
//! It **borrows** a transport rather than building one, so the tunnel rides
//! the identity's endpoint beside every other plane and never binds a second
//! endpoint to the same key. It **follows the own set** — a `watch` of the
//! profile's device ids — rather than a static peer list, so a device paired
//! after boot is dialed without a restart and a device retired from the
//! kinship log loses its route while a packet is still in flight. And it is
//! **cancellable**: every dial and every flow is an owned task, closed and
//! joined on retirement and on stop, because a dial nobody can stop is a route
//! nobody can revoke.
//!
//! Admission is one pure question, [`admit_own`]: QUIC proved the caller's key,
//! the own set says whether that key is mine. Nothing else — no Space, no
//! capability, no directory — is consulted. A caller outside the set is closed
//! before any frame is read and before a route exists, which under `Public` is
//! the whole difference between a tunnel and a hole.
//!
//! Packets are framed on one raw bidirectional flow per pair as a `u16`
//! big-endian length followed by the packet, the dialer writing a
//! [`NetOpening`] as its first frame. The lower `PeerId` dials; the higher
//! accepts — one connection per pair, deterministically. Encryption and
//! reachability are the transport's (`comms`); routing by tunnel address is the
//! carry's own table and never the transport's.

use std::collections::{BTreeMap, HashMap};
use std::io::{Read, Write};
use std::net::Ipv6Addr;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use comms::{
    Connection, ConnectionQueue, IncomingConnection, PathKind, PeerId, RecvFlow, SendFlow,
    Transport,
};
use mechanics::ids::DeviceId;
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

/// The tunnel protocol. Its own generation, independent of every other plane;
/// the daemon registers it on the identity endpoint and this carry dials it.
pub const NET_ALPN: &[u8] = b"lait/net/1";
/// The opening's protocol version. A different number is closed, so a later
/// incompatible change is a bump here and a compatible one is a feature bit.
pub const PROTOCOL_VERSION: u16 = 1;
/// Close code: the caller is not one of this identity's devices, or spoke a
/// version this carry does not.
pub const REFUSED: u32 = 1;
/// Close code: the pair is no longer in the own set — one side was retired.
/// Written for the peer's benefit and never read back through `comms`
/// (`closed()` says only that a connection is gone); what a retired peer does
/// with it is its own carry's business.
pub const RETIRED: u32 = 2;
/// The largest packet the tunnel will frame. A generous ceiling above any MTU.
const MAX_PACKET: usize = 65_535;
/// The largest opening the accepter will read. Two small integers today; a
/// bound so a stranger who got this far cannot make us buffer a packet's
/// worth of nothing.
const MAX_OPENING: usize = 64;
/// A failed dial waits this long, plus up to [`BACKOFF_SPREAD`], before the
/// next. Jittered so a fleet restarted together does not dial together.
const BACKOFF_FLOOR: Duration = Duration::from_secs(2);
const BACKOFF_SPREAD: Duration = Duration::from_secs(8);

/// The dialer's first frame on a flow: which generation of the carry it
/// speaks. Mirrors the runtime planes' `Open.{protocol_version, features}` so
/// a later packet filter is a feature bit rather than an ALPN bump.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetOpening {
    pub protocol_version: u16,
    pub features: u32,
}

impl Default for NetOpening {
    fn default() -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            features: 0,
        }
    }
}

impl NetOpening {
    pub fn encode(&self) -> Result<Vec<u8>> {
        postcard::to_stdvec(self).context("encode opening")
    }

    /// Decode a peer's opening, refusing any version but ours.
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let opening: Self = postcard::from_bytes(bytes).context("decode opening")?;
        if opening.protocol_version != PROTOCOL_VERSION {
            bail!(
                "peer speaks lait/net protocol {}, this carry speaks {PROTOCOL_VERSION}",
                opening.protocol_version
            );
        }
        Ok(opening)
    }
}

/// Where packets come from and go to: one read per packet, one write per
/// packet, as a TUN opened `IFF_NO_PI` delivers. A `File` is both; so is a
/// pair of channels, which is how the carry is tested without an interface.
pub struct Packets {
    pub read: Box<dyn Read + Send>,
    pub write: Box<dyn Write + Send>,
}

/// How one own device is reached, as of the last thing that happened.
///
/// Three absences kept apart: `NoRoute` is a peer the transport was never
/// taught to reach (Isolated, no hint), `Unreachable` is a dial that was tried
/// and failed, `Retired` is a device the set no longer names. Folding any two
/// together renders a revoked device as a flaky one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reach {
    Connected { via: PathKind },
    Dialing,
    NoRoute,
    Unreachable { since: Instant },
    Retired,
}

/// The interface the carry writes into, decided once at mount. `Off` is the
/// operator's choice; `NotPermitted` and `Unsupported` are the machine's, and
/// are reported as themselves rather than as "off".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Interface {
    Up { name: String, address: Ipv6Addr },
    NotPermitted,
    Unsupported,
    Off,
}

/// The entire admission: the caller's key is one the own set names, and it is
/// a key (an id without one has no tunnel address to route to). `set` is the
/// watch's current value — empty admits nobody, which is the fail-closed
/// answer while the set is unknown.
pub fn admit_own(from: &PeerId, set: &[DeviceId]) -> bool {
    crate::ula_for(from).is_some() && set.contains(from)
}

/// Outbound routing: a peer's tunnel address → a channel into its live flow.
/// A packet for an address with no live flow is dropped, exactly as a packet
/// for an unreachable host would be.
type Routes = Arc<Mutex<HashMap<Ipv6Addr, mpsc::UnboundedSender<Vec<u8>>>>>;
/// The peer table [`Carry::peers`] reads: every device the set has named,
/// with its address and how it is reached.
type Table = Arc<Mutex<BTreeMap<DeviceId, (Ipv6Addr, Reach)>>>;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// A handle on the carry: what the daemon keeps to read the peer table while
/// [`Carry::run`] holds the loop.
#[derive(Clone)]
pub struct Carry {
    interface: Interface,
    table: Table,
}

impl Carry {
    pub fn new(interface: Interface) -> Self {
        Self {
            interface,
            table: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }

    /// How one device is reached, or `None` when this run has no row for it.
    /// A caller asking per device gets a lookup rather than a copy of the
    /// whole table, which is what a view drawing every device would otherwise
    /// pay for once per row.
    pub fn reach_of(&self, peer: &DeviceId) -> Option<Reach> {
        lock(&self.table).get(peer).map(|(_, reach)| *reach)
    }

    /// Every device the set has named in the current run, its tunnel address,
    /// and its reach. Per run: a stop empties the table, so a row never
    /// outlives the links it described.
    pub fn peers(&self) -> Vec<(DeviceId, Ipv6Addr, Reach)> {
        lock(&self.table)
            .iter()
            .map(|(id, (ula, reach))| (id.clone(), *ula, *reach))
            .collect()
    }

    /// Carry packets for the devices `own` names until `stop` is raised or the
    /// transport's queue closes. `queue` is this transport's [`NET_ALPN`]
    /// lane; connections arriving on it are re-admitted here even when a hub
    /// already admitted them, because admission that lives in one place only
    /// is admission that a second composition site forgets.
    ///
    /// The blocking packet reader is **not joined** on stop: a blocking read
    /// has no cancel, so it ends when `packets.read` does. A mount that must
    /// return promptly gives the carry a source that ends with it — a
    /// non-blocking TUN behind a stop pipe — or shuts its runtime down with
    /// `shutdown_background`; awaiting that thread is a stop that never comes.
    pub async fn run(
        &self,
        transport: Arc<dyn Transport>,
        mut queue: ConnectionQueue,
        mut own: watch::Receiver<Vec<DeviceId>>,
        packets: Packets,
        mut stop: watch::Receiver<bool>,
    ) -> Result<()> {
        let routes: Routes = Arc::new(Mutex::new(HashMap::new()));
        let (inbound_tx, inbound_rx) = std::sync::mpsc::channel::<Vec<u8>>();
        let (events_tx, mut events_rx) = mpsc::unbounded_channel::<Event>();
        let wiring = Arc::new(Wiring {
            routes: Arc::clone(&routes),
            table: Arc::clone(&self.table),
            interface: self.interface.clone(),
        });
        let mut running = Running {
            me: transport.my_id(),
            transport,
            wiring,
            inbound_tx,
            events: events_tx,
            links: HashMap::new(),
            generation: 0,
        };

        // Inbound packets from every peer funnel to one blocking writer;
        // outbound packets are read by one blocking reader that routes each
        // by destination. Blocking because a TUN read is, and because a packet
        // loop belongs on a thread that yields to nobody.
        let writer = tokio::task::spawn_blocking(move || write_loop(packets.write, &inbound_rx));
        // Not joined on stop: a blocking read of a TUN has no cancel, and the
        // reader ends when its source does — which for a TUN is the process.
        let _reader = tokio::task::spawn_blocking(move || read_loop(packets.read, &routes));

        let initial = own.borrow_and_update().clone();
        running.reconcile(&initial).await;

        loop {
            tokio::select! {
                () = stopped(&mut stop) => break,
                changed = own.changed() => {
                    if changed.is_err() {
                        break;
                    }
                    let set = own.borrow_and_update().clone();
                    running.reconcile(&set).await;
                }
                incoming = queue.recv() => match incoming {
                    Some(incoming) => {
                        // A device added and dialing at once must not be
                        // refused for arriving before the watch arm's turn.
                        if own.has_changed().unwrap_or(false) {
                            let set = own.borrow_and_update().clone();
                            running.reconcile(&set).await;
                        }
                        let set = own.borrow().clone();
                        running.accept(incoming, &set).await;
                    }
                    None => break,
                },
                event = events_rx.recv() => {
                    if let Some(event) = event {
                        running.on_event(event);
                    }
                }
            }
        }

        running.shutdown().await;
        // The last sender is gone, so the writer drains and returns.
        drop(running);
        let _ = writer.await;
        Ok(())
    }
}

/// Resolves when `stop` is raised, or when whoever held it is gone. Owns the
/// watch's guard so the loop's `select!` never carries one across an await.
async fn stopped(stop: &mut watch::Receiver<bool>) {
    let _ = stop.wait_for(|stopped| *stopped).await;
}

/// What the carry's tasks share with the loop that owns them: the live
/// routes, what each peer's reach reads as, and the interface those routes
/// are raised on.
struct Wiring {
    routes: Routes,
    table: Table,
    interface: Interface,
}

impl Wiring {
    fn set_reach(&self, peer: &DeviceId, reach: Reach) {
        if let Some(row) = lock(&self.table).get_mut(peer) {
            row.1 = reach;
        }
    }

    fn reach(&self, peer: &DeviceId) -> Option<Reach> {
        lock(&self.table).get(peer).map(|row| row.1)
    }

    /// The route table first, the kernel second. `ip` is a whole process, and
    /// forking one on the runtime's own thread would stall every other plane
    /// this daemon carries for as long as it takes — so the kernel half runs
    /// on a blocking thread while the table is already correct.
    async fn raise_route(&self, ula: Ipv6Addr, tx: mpsc::UnboundedSender<Vec<u8>>) {
        lock(&self.routes).insert(ula, tx);
        self.route(ula, true).await;
    }

    async fn lower_route(&self, ula: Ipv6Addr) {
        let held = lock(&self.routes).remove(&ula).is_some();
        if !held {
            return;
        }
        self.route(ula, false).await;
    }

    /// Add or remove one host route on the interface, if there is one. A
    /// failure is a degraded tunnel, never a dead daemon: the flow is already
    /// live either way, and what is missing is the kernel's opinion about
    /// which packets belong to it.
    async fn route(&self, ula: Ipv6Addr, add: bool) {
        let Interface::Up { name, .. } = &self.interface else {
            return;
        };
        let dev = name.clone();
        let changed = tokio::task::spawn_blocking(move || {
            if add {
                crate::tun::add_route(&dev, ula)
            } else {
                crate::tun::del_route(&dev, ula)
            }
        })
        .await;
        let verb = if add { "added" } else { "removed" };
        match changed {
            Ok(Ok(())) => {}
            Ok(Err(error)) => tracing::warn!(%ula, dev = %name, %error, "route not {verb}"),
            Err(error) => tracing::warn!(%ula, dev = %name, %error, "route change did not run"),
        }
    }
}

/// What a task reports back to the loop. `generation` names the attempt it
/// belongs to, so a report from a link the loop has already retired or
/// replaced is recognised as stale rather than acted on.
enum Event {
    Dialed {
        peer: DeviceId,
        generation: u64,
        connection: Box<dyn Connection>,
        send: Box<dyn SendFlow>,
        recv: Box<dyn RecvFlow>,
    },
    DialFailed {
        peer: DeviceId,
        generation: u64,
    },
    Ended {
        peer: DeviceId,
        generation: u64,
    },
}

/// What is happening for one peer right now.
enum Slot {
    /// This side dials (its id is the lower); the task retries with backoff.
    Dialing(JoinHandle<()>),
    /// The peer dials (its id is the lower); nothing to do until it arrives.
    Expecting,
    /// One connection, served by one task. The connection is held here so
    /// retirement can close it with a code the peer can act on.
    Live {
        connection: Arc<dyn Connection>,
        task: JoinHandle<()>,
    },
}

struct Link {
    ula: Ipv6Addr,
    generation: u64,
    slot: Slot,
}

struct Running {
    me: PeerId,
    transport: Arc<dyn Transport>,
    wiring: Arc<Wiring>,
    inbound_tx: std::sync::mpsc::Sender<Vec<u8>>,
    events: mpsc::UnboundedSender<Event>,
    links: HashMap<DeviceId, Link>,
    generation: u64,
}

impl Running {
    fn next_generation(&mut self) -> u64 {
        self.generation = self.generation.wrapping_add(1);
        self.generation
    }

    /// The reach of a peer nothing has been tried against yet. Under Isolated
    /// the transport resolves nobody it was not taught, so the honest answer
    /// is "no route" rather than "dialing".
    fn initial_reach(&self) -> Reach {
        if self.transport.is_isolated() {
            Reach::NoRoute
        } else {
            Reach::Dialing
        }
    }

    /// Bring the links in line with the set: retire what it dropped, add what
    /// it gained. `me ∉ set` means this device was retired — every link
    /// closes and every route drops, because a retired device carrying for
    /// its former siblings is the revocation that did not happen.
    async fn reconcile(&mut self, set: &[DeviceId]) {
        let desired: Vec<DeviceId> = if set.contains(&self.me) {
            set.iter().filter(|id| **id != self.me).cloned().collect()
        } else {
            Vec::new()
        };
        let removed: Vec<DeviceId> = self
            .links
            .keys()
            .filter(|peer| !desired.contains(peer))
            .cloned()
            .collect();
        for peer in removed {
            self.retire(&peer).await;
        }
        for peer in desired {
            if !self.links.contains_key(&peer) {
                self.add(peer);
            }
        }
    }

    fn add(&mut self, peer: DeviceId) {
        let Some(ula) = crate::ula_for(&peer) else {
            tracing::warn!(peer = %peer.short(), "own device has no endpoint key; not carried");
            return;
        };
        // Under Local the policy default (the relays) is what resolves a bare
        // id; under Isolated only a hint handed in through `Transport::learn`
        // by whoever learned it (the pairing ceremony) will.
        self.transport.learn(peer.clone(), &[]);
        let generation = self.next_generation();
        lock(&self.wiring.table).insert(peer.clone(), (ula, self.initial_reach()));
        let slot = if self.me < peer {
            Slot::Dialing(self.spawn_dial(peer.clone(), generation, Duration::ZERO))
        } else {
            Slot::Expecting
        };
        self.links.insert(
            peer,
            Link {
                ula,
                generation,
                slot,
            },
        );
    }

    /// Close, cancel, join, then drop the route — in that order. Joining
    /// before the route drop is what makes "dropped" true: an aborted task
    /// finishes the poll it is in, and that poll may be the one raising the
    /// route.
    async fn retire(&mut self, peer: &DeviceId) {
        let Some(link) = self.links.remove(peer) else {
            return;
        };
        Self::close_slot(link.slot, RETIRED, b"retired").await;
        self.wiring.lower_route(link.ula).await;
        self.wiring.set_reach(peer, Reach::Retired);
        tracing::info!(peer = %peer.short(), "retired from the carry");
    }

    async fn close_slot(slot: Slot, code: u32, reason: &[u8]) {
        match slot {
            Slot::Dialing(task) => {
                task.abort();
                let _ = task.await;
            }
            Slot::Expecting => {}
            Slot::Live { connection, task } => {
                connection.close(code, reason);
                task.abort();
                let _ = task.await;
            }
        }
    }

    /// Close every link and empty the table: a row that said `Connected`
    /// after its connection was closed would be a stale answer a later run
    /// inherits.
    async fn shutdown(&mut self) {
        let peers: Vec<DeviceId> = self.links.keys().cloned().collect();
        for peer in peers {
            if let Some(link) = self.links.remove(&peer) {
                Self::close_slot(link.slot, 0, b"stopping").await;
                self.wiring.lower_route(link.ula).await;
            }
        }
        lock(&self.wiring.table).clear();
    }

    fn spawn_dial(&self, peer: DeviceId, generation: u64, first_delay: Duration) -> JoinHandle<()> {
        let transport = Arc::clone(&self.transport);
        let events = self.events.clone();
        tokio::spawn(dial(transport, peer, generation, first_delay, events))
    }

    fn spawn_serve(
        &self,
        peer: DeviceId,
        generation: u64,
        ula: Ipv6Addr,
        via: PathKind,
        send: Box<dyn SendFlow>,
        recv: Box<dyn RecvFlow>,
    ) -> JoinHandle<()> {
        let wiring = Arc::clone(&self.wiring);
        let inbound = self.inbound_tx.clone();
        let events = self.events.clone();
        tokio::spawn(async move {
            serve(&wiring, &peer, ula, via, send, recv, &inbound).await;
            let _ = events.send(Event::Ended { peer, generation });
        })
    }

    fn spawn_accept(
        &self,
        peer: DeviceId,
        generation: u64,
        ula: Ipv6Addr,
        connection: Arc<dyn Connection>,
    ) -> JoinHandle<()> {
        let wiring = Arc::clone(&self.wiring);
        let inbound = self.inbound_tx.clone();
        let events = self.events.clone();
        tokio::spawn(async move {
            if let Err(error) = accept(&wiring, &peer, ula, &*connection, &inbound).await {
                tracing::warn!(peer = %peer.short(), error = %format!("{error:#}"), "flow refused");
                connection.close(REFUSED, b"opening");
            }
            let _ = events.send(Event::Ended { peer, generation });
        })
    }

    /// A connection on the net lane. Admission runs here before any frame is
    /// read, whatever a hub did upstream; a refused caller is closed and
    /// never gains a row, a task, or a route.
    async fn accept(&mut self, incoming: IncomingConnection, set: &[DeviceId]) {
        let IncomingConnection {
            from,
            alpn,
            connection,
            ..
        } = incoming;
        let admitted = admit_own(&from, set) && from != self.me && alpn == NET_ALPN;
        let link = if admitted {
            self.links.get(&from)
        } else {
            None
        };
        let Some(link) = link else {
            tracing::debug!(from = %from.short(), alpn = %String::from_utf8_lossy(&alpn), "refused: not one of this identity's devices");
            connection.close(REFUSED, b"not one of this identity's devices");
            return;
        };
        let ula = link.ula;
        // A second connection from a peer replaces the first: the newer one is
        // the one the peer believes in, and a dial of ours it beat to it is
        // one connection too many.
        if let Some(link) = self.links.remove(&from) {
            Self::close_slot(link.slot, 0, b"replaced").await;
            self.wiring.lower_route(link.ula).await;
        }
        let generation = self.next_generation();
        let connection: Arc<dyn Connection> = Arc::from(connection);
        let task = self.spawn_accept(from.clone(), generation, ula, Arc::clone(&connection));
        self.links.insert(
            from,
            Link {
                ula,
                generation,
                slot: Slot::Live { connection, task },
            },
        );
    }

    fn on_event(&mut self, event: Event) {
        match event {
            Event::Dialed {
                peer,
                generation,
                connection,
                send,
                recv,
            } => {
                let ula = match self.links.get(&peer) {
                    Some(link) if link.generation == generation => link.ula,
                    _ => {
                        connection.close(RETIRED, b"retired");
                        return;
                    }
                };
                let connection: Arc<dyn Connection> = Arc::from(connection);
                let via = connection.quality().via;
                let task = self.spawn_serve(peer.clone(), generation, ula, via, send, recv);
                if let Some(link) = self.links.get_mut(&peer) {
                    link.slot = Slot::Live { connection, task };
                }
            }
            Event::DialFailed { peer, generation } => {
                if self.links.get(&peer).map(|link| link.generation) != Some(generation) {
                    return;
                }
                let reach = if self.transport.is_isolated() {
                    Reach::NoRoute
                } else {
                    match self.wiring.reach(&peer) {
                        Some(Reach::Unreachable { since }) => Reach::Unreachable { since },
                        _ => Reach::Unreachable {
                            since: Instant::now(),
                        },
                    }
                };
                self.wiring.set_reach(&peer, reach);
            }
            Event::Ended { peer, generation } => {
                if self.links.get(&peer).map(|link| link.generation) != Some(generation) {
                    return;
                }
                let next = self.next_generation();
                let slot = if self.me < peer {
                    // A backoff before the redial, not only after a failure:
                    // a peer that accepts and then closes us would otherwise
                    // be redialed in a tight loop.
                    Slot::Dialing(self.spawn_dial(peer.clone(), next, backoff()))
                } else {
                    Slot::Expecting
                };
                if let Some(link) = self.links.get_mut(&peer) {
                    link.generation = next;
                    link.slot = slot;
                }
                self.wiring.set_reach(&peer, self.initial_reach());
            }
        }
    }
}

/// A number in `[0, 1)` for jitter, degrading to the midpoint rather than
/// failing — a dial that refused to run because entropy was briefly
/// unavailable would be a peer that stays unreachable over a number that only
/// needs to be roughly spread.
fn draw() -> f64 {
    let mut bytes = [0u8; 4];
    if getrandom::fill(&mut bytes).is_err() {
        return 0.5;
    }
    f64::from(u32::from_le_bytes(bytes)) / f64::from(u32::MAX)
}

fn backoff() -> Duration {
    BACKOFF_FLOOR.saturating_add(BACKOFF_SPREAD.mul_f64(draw()))
}

/// Dial one peer until it answers, reporting each failure and, at last, the
/// flow. Ends by delivering the connection or by being aborted — never on its
/// own — so "retired" is the loop's decision and not a race with a retry.
async fn dial(
    transport: Arc<dyn Transport>,
    peer: DeviceId,
    generation: u64,
    first_delay: Duration,
    events: mpsc::UnboundedSender<Event>,
) {
    let mut delay = first_delay;
    loop {
        if !delay.is_zero() {
            tokio::time::sleep(delay).await;
        }
        match connect(&*transport, &peer).await {
            Ok((connection, send, recv)) => {
                let _ = events.send(Event::Dialed {
                    peer,
                    generation,
                    connection,
                    send,
                    recv,
                });
                return;
            }
            Err(error) => {
                tracing::debug!(peer = %peer.short(), error = %format!("{error:#}"), "dial failed");
                let _ = events.send(Event::DialFailed {
                    peer: peer.clone(),
                    generation,
                });
                delay = backoff();
            }
        }
    }
}

async fn connect(
    transport: &dyn Transport,
    peer: &DeviceId,
) -> Result<(Box<dyn Connection>, Box<dyn SendFlow>, Box<dyn RecvFlow>)> {
    let connection = transport
        .connect_session(peer.clone(), NET_ALPN)
        .await
        .context("connect")?;
    let (mut send, recv) = connection.open_bi().await.context("open flow")?;
    write_frame(&mut *send, &NetOpening::default().encode()?)
        .await
        .context("write opening")?;
    Ok((connection, send, recv))
}

/// The accepting side of one admitted connection: take the peer's flow, read
/// and check its opening, then serve. No route exists until the opening has
/// been read whole and its version agreed.
async fn accept(
    wiring: &Wiring,
    peer: &DeviceId,
    ula: Ipv6Addr,
    connection: &dyn Connection,
    inbound: &std::sync::mpsc::Sender<Vec<u8>>,
) -> Result<()> {
    let Some((send, mut recv)) = connection.accept_bi().await.context("accept flow")? else {
        bail!("closed before opening a flow");
    };
    let opening = read_frame(&mut *recv, MAX_OPENING)
        .await
        .context("read opening")?;
    NetOpening::decode(&opening)?;
    let via = connection.quality().via;
    serve(wiring, peer, ula, via, send, recv, inbound).await;
    Ok(())
}

/// Split one flow pair into a writer (drains this peer's route channel) and a
/// reader (hands inbound packets to the interface), and run until either ends.
async fn serve(
    wiring: &Wiring,
    peer: &DeviceId,
    ula: Ipv6Addr,
    via: PathKind,
    mut send: Box<dyn SendFlow>,
    mut recv: Box<dyn RecvFlow>,
    inbound: &std::sync::mpsc::Sender<Vec<u8>>,
) {
    let (tx, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();
    wiring.raise_route(ula, tx).await;
    wiring.set_reach(peer, Reach::Connected { via });
    tracing::info!(peer = %peer.short(), %ula, ?via, "carrying");

    let writer = async move {
        while let Some(packet) = rx.recv().await {
            if write_frame(&mut *send, &packet).await.is_err() {
                break;
            }
        }
    };
    let reader = async move {
        loop {
            let Ok(packet) = read_frame(&mut *recv, MAX_PACKET).await else {
                break;
            };
            if inbound.send(packet).is_err() {
                break;
            }
        }
    };
    tokio::select! {
        () = writer => {}
        () = reader => {}
    }
    wiring.lower_route(ula).await;
    tracing::info!(peer = %peer.short(), %ula, "flow ended");
}

async fn write_frame(send: &mut dyn SendFlow, bytes: &[u8]) -> Result<()> {
    let Ok(len) = u16::try_from(bytes.len()) else {
        // Silently dropping would be a hole in the framing; a frame we cannot
        // describe in two bytes is one we never start writing.
        bail!("frame of {} bytes exceeds the u16 framing", bytes.len());
    };
    send.write_all(&len.to_be_bytes()).await?;
    send.write_all(bytes).await
}

async fn read_frame(recv: &mut dyn RecvFlow, max: usize) -> Result<Vec<u8>> {
    let header = recv.read_exact(2).await?;
    let len = usize::from(u16::from_be_bytes([
        header.first().copied().unwrap_or_default(),
        header.get(1).copied().unwrap_or_default(),
    ]));
    if len == 0 || len > max {
        bail!("frame of {len} bytes is outside 1..={max}");
    }
    recv.read_exact(len).await
}

/// Blocking: read packets from the interface and hand each to the peer that
/// owns its destination address. A packet for an unknown destination is
/// dropped. Ends when the source does.
fn read_loop(mut read: Box<dyn Read + Send>, routes: &Routes) {
    let mut buf = vec![0u8; MAX_PACKET];
    loop {
        let n = match read.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => {
                tracing::warn!(%error, "packet read ended");
                break;
            }
        };
        let Some(packet) = buf.get(..n) else {
            break;
        };
        if let Some(dest) = crate::ipv6_destination(packet) {
            if let Some(tx) = lock(routes).get(&dest) {
                let _ = tx.send(packet.to_vec());
            }
        }
    }
}

/// Blocking: write inbound packets to the interface. A failed write loses
/// that packet, as a host would, and never the carry — degradation is not a
/// reason for the daemon not to exist.
fn write_loop(mut write: Box<dyn Write + Send>, inbound: &std::sync::mpsc::Receiver<Vec<u8>>) {
    while let Ok(packet) = inbound.recv() {
        if let Err(error) = write.write_all(&packet) {
            tracing::warn!(%error, "packet not written");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use comms::mem::MemNet;
    use std::sync::mpsc::{Receiver, Sender};

    fn device(seed: u8) -> DeviceId {
        mechanics::actor::device_from_seed(&[seed; 32])
    }

    /// One packet per `read`, as `IFF_NO_PI` delivers; EOF once the test
    /// drops its sender.
    struct Source(Receiver<Vec<u8>>);

    impl Read for Source {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            match self.0.recv() {
                Ok(packet) => {
                    let n = packet.len().min(buf.len());
                    buf[..n].copy_from_slice(&packet[..n]);
                    Ok(n)
                }
                Err(_) => Ok(0),
            }
        }
    }

    /// One packet per `write_all`.
    struct Sink(Sender<Vec<u8>>);

    impl Write for Sink {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            let _ = self.0.send(buf.to_vec());
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    struct Node {
        id: DeviceId,
        carry: Carry,
        run: JoinHandle<Result<()>>,
        /// What this node's interface hands the carry (its outbound packets).
        interface_in: Sender<Vec<u8>>,
        /// What the carry writes to this node's interface (its inbound packets).
        interface_out: Receiver<Vec<u8>>,
    }

    fn node(
        net: &MemNet,
        id: DeviceId,
        own: &watch::Receiver<Vec<DeviceId>>,
        stop: &watch::Receiver<bool>,
    ) -> Node {
        let transport = net.peer(id.clone());
        let queue = transport
            .take_session_queue(NET_ALPN)
            .expect("the net lane is handed over once");
        let (interface_in, read) = std::sync::mpsc::channel();
        let (write, interface_out) = std::sync::mpsc::channel();
        let carry = Carry::new(Interface::Off);
        let run = {
            let carry = carry.clone();
            let own = own.clone();
            let stop = stop.clone();
            tokio::spawn(async move {
                carry
                    .run(
                        Arc::new(transport),
                        queue,
                        own,
                        Packets {
                            read: Box::new(Source(read)),
                            write: Box::new(Sink(write)),
                        },
                        stop,
                    )
                    .await
            })
        };
        Node {
            id,
            carry,
            run,
            interface_in,
            interface_out,
        }
    }

    fn packet_to(dest: Ipv6Addr, payload: u8) -> Vec<u8> {
        let mut packet = vec![payload; 48];
        packet[0] = 0x60;
        packet[24..40].copy_from_slice(&dest.octets());
        packet
    }

    fn reach_of(carry: &Carry, peer: &DeviceId) -> Option<Reach> {
        carry
            .peers()
            .into_iter()
            .find(|(id, _, _)| id == peer)
            .map(|(_, _, reach)| reach)
    }

    async fn wait_for_reach(carry: &Carry, peer: &DeviceId, wanted: impl Fn(Reach) -> bool + Send) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if reach_of(carry, peer).is_some_and(&wanted) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "never reached the wanted state for {}: {:?}",
                peer.short(),
                carry.peers()
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    fn delivered(node: &Node, within: Duration) -> Option<Vec<u8>> {
        tokio::task::block_in_place(|| node.interface_out.recv_timeout(within).ok())
    }

    #[test]
    fn admission_is_the_own_set_and_nothing_else() {
        let (a, b, stranger) = (device(1), device(2), device(3));
        let set = vec![a.clone(), b.clone()];
        assert!(admit_own(&a, &set));
        assert!(admit_own(&b, &set));
        assert!(
            !admit_own(&stranger, &set),
            "a key outside the set is refused"
        );
        assert!(!admit_own(&a, &[]), "an empty set admits nobody");
        let not_a_key = DeviceId::from_key_string("not-a-key".into());
        assert!(
            !admit_own(&not_a_key, std::slice::from_ref(&not_a_key)),
            "an id without an endpoint key has no address to route to"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_packet_crosses_between_two_own_devices_in_memory() {
        let net = MemNet::new().with_planes();
        let (own_tx, own) = watch::channel(vec![device(1), device(2)]);
        let (stop_tx, stop) = watch::channel(false);
        let a = node(&net, device(1), &own, &stop);
        let b = node(&net, device(2), &own, &stop);

        wait_for_reach(&a.carry, &b.id, |r| matches!(r, Reach::Connected { .. })).await;
        wait_for_reach(&b.carry, &a.id, |r| matches!(r, Reach::Connected { .. })).await;

        let b_ula = crate::ula_for(&b.id).unwrap();
        a.interface_in.send(packet_to(b_ula, 0xAB)).unwrap();
        let got = delivered(&b, Duration::from_secs(5));
        assert_eq!(got, Some(packet_to(b_ula, 0xAB)));

        stop_tx.send(true).unwrap();
        a.run.await.unwrap().unwrap();
        b.run.await.unwrap().unwrap();
        drop(own_tx);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_stranger_on_the_net_plane_is_closed_before_a_route_exists() {
        let net = MemNet::new().with_planes();
        let (_own_tx, own) = watch::channel(vec![device(1), device(2)]);
        let (stop_tx, stop) = watch::channel(false);
        let a = node(&net, device(1), &own, &stop);
        let stranger = net.peer(device(3));

        // A flow is invisible to the peer until its first write, so a
        // stranger that opens one and says nothing is closed only by a carry
        // that refuses at admission — one that admitted after reading the
        // opening would park in `accept_bi` and miss the deadline.
        let silent = stranger
            .connect_session(a.id.clone(), NET_ALPN)
            .await
            .unwrap();
        let _flow = silent.open_bi().await.unwrap();
        tokio::time::timeout(Duration::from_secs(5), silent.closed())
            .await
            .expect("a silent stranger is closed before any frame is read");

        // A well-formed opening from the wrong key must not help either.
        let talking = stranger
            .connect_session(a.id.clone(), NET_ALPN)
            .await
            .unwrap();
        let (mut send, _recv) = talking.open_bi().await.unwrap();
        let _ = write_frame(&mut *send, &NetOpening::default().encode().unwrap()).await;
        tokio::time::timeout(Duration::from_secs(5), talking.closed())
            .await
            .expect("a talking stranger is closed, not served");
        assert!(
            a.carry.peers().iter().all(|(id, _, _)| *id != device(3)),
            "a stranger never gains a row: {:?}",
            a.carry.peers()
        );

        stop_tx.send(true).unwrap();
        a.run.await.unwrap().unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn retiring_a_device_drops_its_live_route() {
        let net = MemNet::new().with_planes();
        // Two watches: retirement is A's decision, and B must go on
        // believing in the pair so that only A's close can end the link.
        let (a_own_tx, a_own) = watch::channel(vec![device(1), device(2)]);
        let (_b_own_tx, b_own) = watch::channel(vec![device(1), device(2)]);
        let (stop_tx, stop) = watch::channel(false);
        let a = node(&net, device(1), &a_own, &stop);
        let b = node(&net, device(2), &b_own, &stop);
        wait_for_reach(&a.carry, &b.id, |r| matches!(r, Reach::Connected { .. })).await;
        wait_for_reach(&b.carry, &a.id, |r| matches!(r, Reach::Connected { .. })).await;

        a_own_tx.send(vec![device(1)]).unwrap();
        wait_for_reach(&a.carry, &b.id, |r| r == Reach::Retired).await;
        // B's set still names A; only A closing the connection moves B off
        // `Connected` — and A refuses whatever B tries next.
        wait_for_reach(&b.carry, &a.id, |r| !matches!(r, Reach::Connected { .. })).await;
        assert_ne!(reach_of(&b.carry, &a.id), Some(Reach::Retired));

        let b_ula = crate::ula_for(&b.id).unwrap();
        a.interface_in.send(packet_to(b_ula, 0xCD)).unwrap();
        assert_eq!(
            delivered(&b, Duration::from_millis(300)),
            None,
            "a packet for a retired device is dropped, not carried"
        );

        stop_tx.send(true).unwrap();
        a.run.await.unwrap().unwrap();
        b.run.await.unwrap().unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn retiring_a_device_mid_dial_aborts_the_dial_and_a_stop_returns_promptly() {
        let net = MemNet::new().with_planes();
        // The dialer is the lower id; the peer it dials is not on the network,
        // so every attempt fails and the dial task lives in its backoff.
        let mut ids = [device(1), device(9)];
        ids.sort();
        let (low, high) = (ids[0].clone(), ids[1].clone());
        let (own_tx, own) = watch::channel(vec![low.clone(), high.clone()]);
        let (stop_tx, stop) = watch::channel(false);
        let a = node(&net, low, &own, &stop);
        wait_for_reach(&a.carry, &high, |r| matches!(r, Reach::Unreachable { .. })).await;

        own_tx.send(vec![a.id.clone()]).unwrap();
        wait_for_reach(&a.carry, &high, |r| r == Reach::Retired).await;

        stop_tx.send(true).unwrap();
        // A dial that was not aborted and joined would hold the stop for the
        // rest of its 2–10 s backoff.
        tokio::time::timeout(Duration::from_secs(1), a.run)
            .await
            .expect("stop returns without waiting out a backoff")
            .unwrap()
            .unwrap();
        assert!(
            a.carry.peers().is_empty(),
            "a stop empties the table: {:?}",
            a.carry.peers()
        );
    }
}
