//! ADVERSARIAL: does the `peer_holds_news` re-arm in `note_reciprocable`
//! actually have the fixpoint its comment claims?
//!
//! The claim (crates/runtime/src/neighbors.rs:495) is:
//!
//! > Once we pull, its heads are a subset of ours and it stops arming — the
//! > predicate has a fixpoint and converged peers fall quiet on their own.
//!
//! That is only true when everything the peer declares can become an
//! *interpreted* head here. `Replica::head_commitments` (replica.rs:3422)
//! deliberately skips opaque records, so material this Station retains but
//! cannot interpret is declared by the peer forever and never appears in
//! `ours`. `peer_holds_news` is then permanently true.
//!
//! Two Stations each holding a World the other does not deploy is the ordinary
//! way to get there — the exact topology `independent_world.rs` already calls
//! "opaque retention". Each side declares heads the other can only retain
//! opaquely, so each inbound Contact re-arms an outbound one, forever, at the
//! 25 ms scheduler tick with `record_success` resetting `next_attempt_ms` to
//! `now` (neighbors.rs:581). Nothing converges; the dials never stop.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use mechanics::authorization::AuthorizedBodyKey;
use mechanics::ids::{ActorId, DeviceId, SpaceId};
use replica::body::{BodyId, BodyKey, EncodingId, MutationModel, Op, Schema, SchemaId, WorldId};
use replica::frontier::AuthorityFrontier;
use runtime::coordinates::{ApproachRoute, CoordinatesAdmission, CoordinatesPayload};
use runtime::{
    coordinates::SignedCoordinates, plane::contact::Authority, plane::Activation,
    plane::CommsOptions, plane::GossipOptions, world::Builder, world::Context, world::Effect,
    world::Intent, world::Projection, world::Query, world::Rejection, world::RequestId,
    world::World, Runtime, Station,
};

const FOUNDER_SEED: [u8; 32] = [7u8; 32];
const RECOVERY_SEED: [u8; 32] = [20u8; 32];
const STATION_A_SEED: [u8; 32] = [41u8; 32];
const STATION_B_SEED: [u8; 32] = [42u8; 32];
const WRITER_SEED: [u8; 32] = [43u8; 32];
const SALT: [u8; 16] = [9u8; 16];
const EPOCH: [u8; 16] = [13u8; 16];
const EPOCH_KEY: [u8; 32] = [14u8; 32];

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn temp_root(tag: &str) -> std::path::PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("lait-dialloop-{tag}-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn coordinates() -> (SpaceId, SignedCoordinates) {
    let rc = mechanics::space::recovery_commit(&mechanics::space::recovery_pub_of(&RECOVERY_SEED))
        .unwrap();
    let device = mechanics::space::recovery_pub_of(&FOUNDER_SEED);
    let ws = mechanics::space::derive_space_id(&device, &SALT, &rc);
    let (incept, _actor) =
        mechanics::actor::incept_single(&FOUNDER_SEED, &ws, [1u8; 16], [2u8; 16], None);
    let payload = CoordinatesPayload {
        space: <[u8; 29]>::try_from(ws.as_str().as_bytes()).unwrap(),
        salt: SALT,
        recovery_root: rc,
        founder_inception: postcard::to_stdvec(&incept).unwrap(),
        display_name_hint: "Dial loop".into(),
        approach_station: mechanics::actor::device_from_seed(&STATION_A_SEED)
            .key_bytes()
            .unwrap(),
        approach_nick_hint: "a".into(),
        approach_routes: vec![ApproachRoute::DirectIpv4 {
            ip: [127, 0, 0, 1],
            port: 4242,
        }],
        admission: CoordinatesAdmission::None,
    };
    (ws, SignedCoordinates::sign(payload, &STATION_A_SEED))
}

fn any_demand() -> Vec<u8> {
    mechanics::authorization::AuthorizationDemand::require(
        mechanics::authorization::PolicyCapability::new("w", "c"),
        mechanics::authorization::Resource::root("w"),
    )
    .encode_canonical()
    .expect("canonical demand")
}

/// A trivial key/value World, parameterised by id so two Stations can deploy
/// two DIFFERENT Worlds and each retain the other's material opaquely.
struct KvWorld {
    id: WorldId,
    schemas: Vec<Schema>,
}

impl KvWorld {
    fn new(id: &str) -> Self {
        Self {
            id: WorldId::parse(id).unwrap(),
            schemas: vec![Schema {
                id: SchemaId::parse("entry").unwrap(),
                version: 1,
                encoding: EncodingId::parse("bytes").unwrap(),
                mutation: MutationModel::Atomic,
                readable_predecessors: vec![],
            }],
        }
    }
    fn body(&self, key: &str) -> BodyKey {
        let mut raw = [0u8; 16];
        let k = key.as_bytes();
        raw[..k.len().min(16)].copy_from_slice(&k[..k.len().min(16)]);
        BodyKey::new(self.id.clone(), BodyId::from_bytes(raw))
    }
}

impl World for KvWorld {
    fn id(&self) -> WorldId {
        self.id.clone()
    }
    fn schemas(&self) -> &[Schema] {
        &self.schemas
    }
    fn submit(&self, _ctx: &mut Context<'_>, intent: Intent) -> Result<Effect, Rejection> {
        let text = String::from_utf8(intent.payload).map_err(|_| Rejection::InvalidRequest)?;
        let (key, value) = text.split_once('=').ok_or(Rejection::InvalidRequest)?;
        let body = self.body(key);
        Ok(Effect {
            content_refs: Vec::new(),
            demand: any_demand(),
            operations: vec![(
                body.clone(),
                Op::ReplaceAtomic {
                    value: value.as_bytes().to_vec(),
                },
            )],
            bodies: vec![body],
            effect: vec![],
            declarations: vec![],
        })
    }
    fn query(&self, ctx: &Context<'_>, query: Query) -> Result<Projection, Rejection> {
        let key = String::from_utf8(query.payload).map_err(|_| Rejection::InvalidRequest)?;
        Ok(Projection {
            demand: any_demand(),
            schema: SchemaId::parse("entry").unwrap(),
            schema_version: 1,
            bytes: ctx.read_body(&self.body(&key)).unwrap_or_default(),
            frontier: replica::frontier::ReplicaFrontier::EMPTY,
        })
    }
}

struct TestAuthority;
impl runtime::world::AuthorityView for TestAuthority {
    fn resolve(&self, _device: &DeviceId) -> Option<runtime::world::PrincipalResolution> {
        Some(runtime::world::PrincipalResolution {
            actor: ActorId::from_incept_hash(&"c".repeat(64)),
            authority_frontier: AuthorityFrontier::from_canonical_bytes(vec![3]),
        })
    }
}

struct AnyKnownSigner;
impl replica::transaction::AuthoritySource for AnyKnownSigner {
    fn signer_authorized(&self, signer: &[u8; 32], _f: &AuthorityFrontier) -> bool {
        [WRITER_SEED, STATION_A_SEED, STATION_B_SEED]
            .iter()
            .any(|seed| mechanics::actor::device_from_seed(seed).key_bytes() == Some(*signer))
    }
}

struct AcceptingIncorporator;
impl replica::convergence::AuthorityIncorporator for AcceptingIncorporator {
    fn incorporate_authority(
        &mut self,
        records: &[Vec<u8>],
    ) -> Result<replica::convergence::AuthorityBatchReceipt, replica::convergence::Failure> {
        Ok(replica::convergence::AuthorityBatchReceipt {
            space: coordinates().0,
            prior_frontier: AuthorityFrontier::from_canonical_bytes(vec![]),
            resulting_frontier: AuthorityFrontier::from_canonical_bytes(vec![3]),
            batch_digest: *blake3::hash(&records.concat()).as_bytes(),
        })
    }
}

fn test_keys() -> Arc<dyn replica::body::BodyKeySource> {
    Arc::new(replica::body::StaticBodyKeys::new(
        AuthorizedBodyKey::for_authorized_epoch(EPOCH, EPOCH_KEY),
    ))
}

/// A transport that counts outbound Contact dials and Beacon broadcasts, and
/// otherwise delegates. The two counters separate the two things that can queue
/// a Contact: the gossip Beacon (`observe_beacon`) and the reciprocal arm
/// (`note_reciprocable`).
struct CountingTransport {
    inner: Arc<dyn comms::Transport>,
    contact_dials: Arc<AtomicUsize>,
    beacons: Arc<AtomicUsize>,
}

struct CountingSender {
    inner: Box<dyn comms::GossipSender>,
    beacons: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl comms::GossipSender for CountingSender {
    async fn broadcast(&self, bytes: Vec<u8>) -> anyhow::Result<()> {
        self.beacons.fetch_add(1, Ordering::SeqCst);
        self.inner.broadcast(bytes).await
    }
}

#[async_trait::async_trait]
impl comms::Transport for CountingTransport {
    fn my_id(&self) -> comms::PeerId {
        self.inner.my_id()
    }
    fn learn(&self, peer: comms::PeerId, addrs: &[std::net::SocketAddr]) {
        self.inner.learn(peer, addrs)
    }
    async fn connect(
        &self,
        peer: comms::PeerId,
        alpn: comms::Alpn,
    ) -> anyhow::Result<Box<dyn comms::Stream>> {
        if alpn == runtime::plane::contact::CONTACT_ALPN {
            self.contact_dials.fetch_add(1, Ordering::SeqCst);
        }
        self.inner.connect(peer, alpn).await
    }
    async fn accept(&self) -> Option<comms::Incoming> {
        self.inner.accept().await
    }
    fn advertised_addrs(&self) -> Vec<std::net::SocketAddr> {
        self.inner.advertised_addrs()
    }
    async fn subscribe(
        &self,
        topic: comms::Topic,
        bootstrap: &[comms::PeerId],
    ) -> anyhow::Result<(Box<dyn comms::GossipSender>, Box<dyn comms::GossipReceiver>)> {
        let (sender, receiver) = self.inner.subscribe(topic, bootstrap).await?;
        Ok((
            Box::new(CountingSender {
                inner: sender,
                beacons: self.beacons.clone(),
            }),
            receiver,
        ))
    }
    async fn shutdown(&self) {
        self.inner.shutdown().await
    }
}

fn comms_options(transport: Arc<dyn comms::Transport>, station_seed: [u8; 32]) -> CommsOptions {
    CommsOptions {
        transport,
        station_seed,
        authority: Authority {
            source: Arc::new(AnyKnownSigner),
            incorporator: Arc::new(Mutex::new(AcceptingIncorporator)),
            export: Arc::new(Vec::new),
            frontier: Arc::new(|| AuthorityFrontier::from_canonical_bytes(vec![3])),
        },
        // A beacon floor an hour out: after the activation beacon and the
        // edge-triggered emissions that convergence causes, gossip goes quiet.
        // Any dial after that is the reciprocal arm and nothing else.
        gossip: Some(GossipOptions {
            bootstrap: vec![],
            advertise: vec![],
            beacon_interval: Duration::from_secs(3600),
        }),
        whole_deadline: Duration::from_secs(20),
        progress_deadline: Duration::from_secs(5),
        route_lease: Duration::from_secs(600),
    }
}

fn activate(
    rt: &Runtime,
    coords: &SignedCoordinates,
    transport: Arc<dyn comms::Transport>,
    seed: [u8; 32],
) -> Station {
    rt.materialize(coords)
        .unwrap()
        .open(Activation {
            planes: Default::default(),
            content: Default::default(),
            drain_deadline: Duration::from_secs(5),
            comms: Some(comms_options(transport, seed)),
            observation_capacity: 0,
        })
        .unwrap()
}

fn submit_kv(station: &Station, world: &str, entry: &str) {
    let world_id = WorldId::parse(world).unwrap();
    let writer = Runtime::identity_from_seed(&WRITER_SEED);
    let session = station.dock(&world_id, &writer).unwrap();
    let action = writer
        .sign_action(
            &session,
            RequestId::mint(),
            Intent {
                schema: SchemaId::parse("entry").unwrap(),
                schema_version: 1,
                payload: entry.as_bytes().to_vec(),
            },
        )
        .unwrap();
    session.submit(action).unwrap();
}

/// `worlds` decides whether the pair can interpret each other's material.
/// Returns (dials by A, dials by B) over a quiet observation window that starts
/// only after both sides have stopped changing.
fn dials_after_convergence(tag: &str, shared_worlds: bool) -> (usize, usize, usize, usize) {
    let (_space, coords) = coordinates();
    let net = comms::mem::MemNet::new();
    let count_a = Arc::new(AtomicUsize::new(0));
    let count_b = Arc::new(AtomicUsize::new(0));
    let beacons_a = Arc::new(AtomicUsize::new(0));
    let beacons_b = Arc::new(AtomicUsize::new(0));
    let ta: Arc<dyn comms::Transport> = Arc::new(CountingTransport {
        inner: Arc::new(net.peer(mechanics::actor::device_from_seed(&STATION_A_SEED))),
        contact_dials: count_a.clone(),
        beacons: beacons_a.clone(),
    });
    let tb: Arc<dyn comms::Transport> = Arc::new(CountingTransport {
        inner: Arc::new(net.peer(mechanics::actor::device_from_seed(&STATION_B_SEED))),
        contact_dials: count_b.clone(),
        beacons: beacons_b.clone(),
    });

    let catalog = |own: &str, other: Option<&str>| {
        let mut b = Builder::new().register(Arc::new(KvWorld::new(own)));
        if let Some(other) = other {
            b = b.register(Arc::new(KvWorld::new(other)));
        }
        b.build().unwrap()
    };
    let (w1, w2) = ("dev.example.one", "dev.example.two");
    let root_a = temp_root(&format!("{tag}-a"));
    let root_b = temp_root(&format!("{tag}-b"));
    let rt_a = Runtime::open(
        root_a.clone(),
        catalog(w1, shared_worlds.then_some(w2)),
        Arc::new(TestAuthority),
        test_keys(),
    );
    let rt_b = Runtime::open(
        root_b.clone(),
        catalog(w2, shared_worlds.then_some(w1)),
        Arc::new(TestAuthority),
        test_keys(),
    );

    let station_a = activate(&rt_a, &coords, ta, STATION_A_SEED);
    let station_b = activate(&rt_b, &coords, tb, STATION_B_SEED);
    submit_kv(&station_a, w1, "a=one");
    submit_kv(&station_b, w2, "b=two");

    // Let discovery + convergence run. Ten seconds is far past the point at
    // which two Stations on an in-memory network with one Body each have
    // nothing left to say to one another.
    std::thread::sleep(Duration::from_secs(10));
    let before = (
        count_a.load(Ordering::SeqCst),
        count_b.load(Ordering::SeqCst),
        beacons_a.load(Ordering::SeqCst),
        beacons_b.load(Ordering::SeqCst),
    );
    // The observation window: a quiet system dials zero times here.
    std::thread::sleep(Duration::from_secs(3));
    let after = (
        count_a.load(Ordering::SeqCst),
        count_b.load(Ordering::SeqCst),
        beacons_a.load(Ordering::SeqCst),
        beacons_b.load(Ordering::SeqCst),
    );

    let _ = station_a.vacate();
    let _ = station_b.vacate();
    let _ = std::fs::remove_dir_all(&root_a);
    let _ = std::fs::remove_dir_all(&root_b);
    (
        after.0 - before.0,
        after.1 - before.1,
        after.2 - before.2,
        after.3 - before.3,
    )
}

#[test]
fn two_converged_stations_stop_dialling_each_other() {
    // Control: both Stations deploy both Worlds, so everything either declares
    // becomes an interpreted head on the other. `peer_holds_news` goes false
    // and stays false — this is the fixpoint the comment describes.
    let (control_a, control_b, cba, cbb) = dials_after_convergence("ctl", true);
    println!(
        "CONTROL (both worlds deployed): {control_a} + {control_b} dials, \
         {cba} + {cbb} beacons / 3 s"
    );

    // The break: each Station deploys only its own World, so each retains the
    // other's material OPAQUELY. `head_commitments` never lists an opaque head,
    // so `held \ ours` is non-empty on both sides forever. Every inbound
    // Contact re-arms an outbound one and nothing ever converges the gap.
    let (loop_a, loop_b, lba, lbb) = dials_after_convergence("opaque", false);
    println!(
        "OPAQUE  (disjoint worlds):      {loop_a} + {loop_b} dials, \
         {lba} + {lbb} beacons / 3 s"
    );
    // Beacon traffic is the same in both arms, so the difference is not the
    // gossip path marking anyone pending — it is the reciprocal arm.
    assert!(
        loop_a + loop_b <= (control_a + control_b) * 2,
        "BROKEN: `peer_holds_news` has no fixpoint when the peer declares \
         heads this Station can only retain opaquely — {loop_a} + {loop_b} \
         Contact dials in a 3 s window, 10 s after the last thing either \
         Station had to say (control managed {control_a} + {control_b})"
    );
}
