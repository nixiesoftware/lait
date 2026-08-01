//! G6 — a real iroh two-node Contact/incorporation, through the public API.
//!
//! Two real iroh endpoints on loopback (`Network::Isolated`: no relay, no
//! discovery — each learns the other's advertised addresses, the production
//! ticket path). Station B runs the public `Station::contact` against Station
//! A over the real wire and durably incorporates A's material.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use mechanics::authorization::AuthorizedBodyKey;
use mechanics::{
    ids::{ActorId, DeviceId, SpaceId},
    station::Key,
};
use replica::body::{BodyId, BodyKey, EncodingId, SchemaId, WorldId};
use replica::body::{MutationModel, Op, Schema};
use replica::frontier::{AuthorityFrontier, ReplicaFrontier};
use runtime::coordinates::{ApproachRoute, CoordinatesAdmission, CoordinatesPayload};
use runtime::plane::contact::CONTACT_ALPN;

#[allow(dead_code)]
fn any_demand() -> Vec<u8> {
    mechanics::authorization::AuthorizationDemand::require(
        mechanics::authorization::PolicyCapability::new("w", "c"),
        mechanics::authorization::Resource::root("w"),
    )
    .encode_canonical()
    .expect("canonical demand")
}
use runtime::{
    coordinates::SignedCoordinates, neighbor::PRESENCE_ALPN, plane::contact::Authority,
    plane::Activation, plane::CommsOptions, world::Builder, world::Context, world::Descriptor,
    world::Effect, world::Intent, world::Limits, world::Projection, world::Query, world::Rejection,
    world::RequestId, world::Version, world::World, Runtime, Station,
};

const FOUNDER_SEED: [u8; 32] = [7u8; 32];
const RECOVERY_SEED: [u8; 32] = [20u8; 32];
const STATION_A_SEED: [u8; 32] = [41u8; 32];
const STATION_B_SEED: [u8; 32] = [42u8; 32];
const WRITER_SEED: [u8; 32] = [43u8; 32];
const SALT: [u8; 16] = [9u8; 16];
const EPOCH: [u8; 16] = [15u8; 16];
const EPOCH_KEY: [u8; 32] = [16u8; 32];

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn temp_root(tag: &str) -> std::path::PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "lait-contact-iroh-{tag}-{}-{n}",
        std::process::id()
    ));
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
        display_name_hint: "Iroh Space".into(),
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

struct KvWorld {
    id: WorldId,
    schemas: Vec<Schema>,
}

impl KvWorld {
    fn new() -> Self {
        Self {
            id: WorldId::parse("dev.example.kv").unwrap(),
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
            frontier: ReplicaFrontier::EMPTY,
        })
    }
}

struct TestAuthority;
impl runtime::world::AuthorityView for TestAuthority {
    fn resolve(&self, _device: &DeviceId) -> Option<runtime::world::PrincipalResolution> {
        Some(runtime::world::PrincipalResolution {
            actor: ActorId::from_incept_hash(&"d".repeat(64)),
            authority_frontier: AuthorityFrontier::from_canonical_bytes(vec![4]),
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
        _records: &[Vec<u8>],
    ) -> Result<replica::convergence::AuthorityBatchReceipt, replica::convergence::Failure> {
        Ok(replica::convergence::AuthorityBatchReceipt {
            space: coordinates().0,
            prior_frontier: replica::frontier::AuthorityFrontier::from_canonical_bytes(vec![]),
            resulting_frontier: AuthorityFrontier::from_canonical_bytes(vec![4]),
            batch_digest: *blake3::hash(&_records.concat()).as_bytes(),
        })
    }
}

fn runtime_at(root: &std::path::Path) -> Runtime {
    let world = KvWorld::new();
    let reg = Descriptor {
        id: world.id(),
        implementation_version: Version(1),
        schemas: world.schemas().to_vec(),
        limits: Limits::default(),
        scope_schemas: Vec::new(),
        signal_schemas: Vec::new(),
    };
    let registry = Builder::new().register(Arc::new(world)).build().unwrap();
    Runtime::open(
        root.to_path_buf(),
        registry,
        Arc::new(TestAuthority),
        Arc::new(replica::body::StaticBodyKeys::new(
            AuthorizedBodyKey::for_authorized_epoch(EPOCH, EPOCH_KEY),
        )),
    )
}

fn comms_options(transport: Arc<dyn comms::Transport>, seed: [u8; 32]) -> CommsOptions {
    CommsOptions {
        transport,
        station_seed: seed,
        authority: Authority {
            source: Arc::new(AnyKnownSigner),
            incorporator: Arc::new(Mutex::new(AcceptingIncorporator)),
            export: Arc::new(Vec::new),
            frontier: Arc::new(|| AuthorityFrontier::from_canonical_bytes(vec![4])),
        },
        gossip: None,
        whole_deadline: Duration::from_secs(30),
        progress_deadline: Duration::from_secs(10),
        route_lease: Duration::from_secs(60),
    }
}

fn submit_kv(station: &Station, entry: &str) {
    let world_id = WorldId::parse("dev.example.kv").unwrap();
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

fn read_kv(station: &Station, key: &str) -> Vec<u8> {
    let world_id = WorldId::parse("dev.example.kv").unwrap();
    let writer = Runtime::identity_from_seed(&WRITER_SEED);
    let session = station.dock(&world_id, &writer).unwrap();
    session
        .query(Query {
            schema: SchemaId::parse("entry").unwrap(),
            schema_version: 1,
            payload: key.as_bytes().to_vec(),
        })
        .unwrap()
        .bytes
}

#[test]
fn a_real_iroh_contact_converges_two_stations() {
    // A long-lived multi-thread runtime hosts the endpoints' background tasks
    // for the duration of the test.
    let rt = tokio::runtime::Runtime::new().unwrap();
    let alpns: &[comms::Alpn] = &[CONTACT_ALPN, PRESENCE_ALPN];
    let (ta, tb) = rt.block_on(async {
        let a = comms::DefaultTransport::new(
            &STATION_A_SEED,
            &comms::policy::Network::Isolated,
            comms::Protocols::framed(alpns),
        )
        .await
        .unwrap();
        let b = comms::DefaultTransport::new(
            &STATION_B_SEED,
            &comms::policy::Network::Isolated,
            comms::Protocols::framed(alpns),
        )
        .await
        .unwrap();
        use comms::Transport;
        let a_addrs = a.advertised_addrs();
        let b_addrs = b.advertised_addrs();
        a.learn(b.my_id(), &b_addrs);
        b.learn(a.my_id(), &a_addrs);
        (a, b)
    });
    let ta: Arc<dyn comms::Transport> = Arc::new(ta);
    let tb: Arc<dyn comms::Transport> = Arc::new(tb);

    let (_space, coords) = coordinates();
    let root_a = temp_root("a");
    let root_b = temp_root("b");
    let rt_a = runtime_at(&root_a);
    let rt_b = runtime_at(&root_b);

    let station_a = rt_a
        .materialize(&coords)
        .unwrap()
        .open(Activation {
            planes: Default::default(),
            content: Default::default(),
            drain_deadline: Duration::from_secs(5),
            comms: Some(comms_options(ta, STATION_A_SEED)),
            observation_capacity: 0,
        })
        .unwrap();
    submit_kv(&station_a, "wire=real-iroh");

    let station_b = rt_b
        .materialize(&coords)
        .unwrap()
        .open(Activation {
            planes: Default::default(),
            content: Default::default(),
            drain_deadline: Duration::from_secs(5),
            comms: Some(comms_options(tb, STATION_B_SEED)),
            observation_capacity: 0,
        })
        .unwrap();

    let a_id = Key::from_device(&mechanics::actor::device_from_seed(&STATION_A_SEED)).unwrap();
    let outcome = station_b.contact(&a_id).unwrap();
    assert!(outcome.bytes_moved > 0);
    assert!(outcome.convergence.accepted >= 1);
    assert_eq!(read_kv(&station_b, "wire"), b"real-iroh");

    let _ = station_a.vacate();
    let _ = station_b.vacate();
    drop(rt);
    let _ = std::fs::remove_dir_all(&root_a);
    let _ = std::fs::remove_dir_all(&root_b);
}
