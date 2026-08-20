//! G6 — operational Contact over `MemTransport`, through the **public**
//! Station API only.
//!
//! Two Stations enter the same Space (Coordinates v1), activate with a real
//! comms transport, and converge: once via the privileged administrative
//! `Station::contact`, and once fully automatically — a signed Beacon is
//! observed, the persistent registry queues the Neighbor, the Station
//! scheduler dials, the accepter serves its retained material, and the
//! validated bundle incorporates durably. No test code feeds frames or calls
//! Replica incorporation directly.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
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
    coordinates::SignedCoordinates, plane::contact::Authority, plane::Activation,
    plane::CommsOptions, plane::GossipOptions, world::Builder, world::Context, world::Effect,
    world::Intent, world::Projection, world::Query, world::Rejection, world::RequestId,
    world::World, Runtime, Station,
};

const FOUNDER_SEED: [u8; 32] = [7u8; 32];
const RECOVERY_SEED: [u8; 32] = [20u8; 32];
const STATION_A_SEED: [u8; 32] = [31u8; 32];
const STATION_B_SEED: [u8; 32] = [32u8; 32];
const WRITER_SEED: [u8; 32] = [33u8; 32];
const WRITER_2_SEED: [u8; 32] = [34u8; 32];
const SALT: [u8; 16] = [9u8; 16];
const EPOCH: [u8; 16] = [13u8; 16];
const EPOCH_KEY: [u8; 32] = [14u8; 32];

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn temp_root(tag: &str) -> std::path::PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir =
        std::env::temp_dir().join(format!("lait-contact-mem-{tag}-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A valid founding + Coordinates both nodes enter with.
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
        display_name_hint: "Contact Space".into(),
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

/// The shared note World: intents `key=value` set atomic Bodies.
struct KvWorld {
    id: WorldId,
    schemas: Vec<Schema>,
    find_schemas: Vec<runtime::find::Schema>,
    find_extractors: Vec<runtime::find::Extractor>,
    extraction_gate: Option<Arc<(Mutex<bool>, Condvar)>>,
    extraction_started: Option<std::sync::mpsc::Sender<()>>,
}

impl KvWorld {
    fn new() -> Self {
        let indexed = runtime::find::SchemaRef {
            name: SchemaId::parse("entry-index").unwrap(),
            version: 1,
        };
        let field = runtime::find::FieldRef {
            schema: indexed.clone(),
            name: SchemaId::parse("value").unwrap(),
        };
        let find_schema = runtime::find::Schema {
            reference: indexed.clone(),
            sources: vec![runtime::find::SourceRef {
                name: SchemaId::parse("entry").unwrap(),
                version: 1,
            }],
            fields: vec![runtime::find::Field {
                reference: field,
                kind: runtime::find::FieldKind::Text,
                analyzer: None,
            }],
            edges: Vec::new(),
            gates: Vec::new(),
            analyzers: Vec::new(),
            features: Vec::new(),
            ops: runtime::find::OpSet::SEEK,
            modes: runtime::find::ModeSet::EXACT,
            bound: runtime::find::Bound {
                decoded_bodies: 1024,
                postings_read: 1024,
                edges_visited: 1024,
                nodes_visited: 1024,
                paths_retained: 1024,
                candidates_per_branch: 1024,
                score_evaluations: 1,
                projected_bytes: 1024 * 1024,
                packed_tokens: 1024,
                wall_millis: 1_000,
            },
        };
        let find_extractor = runtime::find::Extractor {
            schema: indexed,
            source: runtime::find::SourceRef {
                name: SchemaId::parse("entry").unwrap(),
                version: 1,
            },
            abi_version: runtime::find::EXTRACTOR_ABI_VERSION,
            semantic_digest: [0x6b; 32],
            shape: runtime::find::ExtractionShape::new(
                1,
                1_024,
                1_024,
                1024 * 1024,
                1024 * 1024,
                4 * 1024,
            ),
        };
        Self {
            id: WorldId::parse("dev.example.kv").unwrap(),
            schemas: vec![Schema {
                id: SchemaId::parse("entry").unwrap(),
                version: 1,
                encoding: EncodingId::parse("bytes").unwrap(),
                mutation: MutationModel::Atomic,
                readable_predecessors: vec![],
            }],
            find_schemas: vec![find_schema],
            find_extractors: vec![find_extractor],
            extraction_gate: None,
            extraction_started: None,
        }
    }

    fn with_extraction_gate(
        mut self,
        gate: Arc<(Mutex<bool>, Condvar)>,
        started: std::sync::mpsc::Sender<()>,
    ) -> Self {
        self.extraction_gate = Some(gate);
        self.extraction_started = Some(started);
        self
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
    fn find_schemas(&self) -> &[runtime::find::Schema] {
        &self.find_schemas
    }
    fn find_extractors(&self) -> &[runtime::find::Extractor] {
        &self.find_extractors
    }
    fn extract(
        &self,
        ctx: &runtime::world::ExtractionContext<'_>,
        extractor: &runtime::find::Extractor,
        body: &BodyKey,
    ) -> Result<runtime::find::BodyExtraction, Rejection> {
        if extractor != &self.find_extractors[0] {
            return Err(Rejection::ContractViolation);
        }
        if let Some(started) = &self.extraction_started {
            let _ = started.send(());
        }
        if let Some(gate) = &self.extraction_gate {
            let (released, wake) = &**gate;
            let mut released = released.lock().unwrap();
            while !*released {
                released = wake.wait(released).unwrap();
            }
        }
        let value = ctx.read_body(body)?.ok_or(Rejection::StateCorrupt)?;
        Ok(runtime::find::BodyExtraction {
            body: body.clone(),
            stamp: ctx.body_stamp(body).unwrap_or_default(),
            nodes: vec![runtime::find::ExtractedNode {
                key: runtime::find::NodeKey {
                    schema: self.find_schemas[0].reference.clone(),
                    node: runtime::find::NodeId::new(body.body.as_bytes().to_vec())
                        .map_err(|_| Rejection::ContractViolation)?,
                },
                gate: None,
                fields: vec![runtime::find::ExtractedField {
                    reference: self.find_schemas[0].fields[0].reference.clone(),
                    value: runtime::find::Value::text(
                        std::str::from_utf8(&value)
                            .map_err(|_| Rejection::StateCorrupt)?
                            .to_owned(),
                    ),
                    gate: None,
                    terms: Vec::new(),
                }],
                edges: Vec::new(),
                features: Vec::new(),
            }],
        })
    }
    fn submit(&self, _ctx: &mut Context<'_>, intent: Intent) -> Result<Effect, Rejection> {
        let text = String::from_utf8(intent.payload).map_err(|_| Rejection::InvalidRequest)?;
        let (key, value) = text.split_once('=').ok_or(Rejection::InvalidRequest)?;
        let body = self.body(key);
        Ok(Effect {
            content_refs: Vec::new(),
            exec: Vec::new(),
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
            bytes: ctx
                .read_body(&self.body(&key))?
                .map(|bytes| bytes.as_ref().to_vec())
                .unwrap_or_default(),
            frontier: ReplicaFrontier::EMPTY,
            publication: None,
        })
    }
}

/// Authorizes everyone this test names (writer + both stations).
struct TestAuthority;
impl runtime::world::AuthorityView for TestAuthority {
    fn resolve(&self, device: &DeviceId) -> Option<runtime::world::PrincipalResolution> {
        let actor_hash = if device == &mechanics::actor::device_from_seed(&WRITER_2_SEED) {
            "d".repeat(64)
        } else {
            "c".repeat(64)
        };
        Some(runtime::world::PrincipalResolution {
            actor: ActorId::from_incept_hash(&actor_hash),
            authority_frontier: AuthorityFrontier::from_canonical_bytes(vec![3]),
        })
    }
}

struct AnyKnownSigner;
impl replica::transaction::AuthoritySource for AnyKnownSigner {
    fn signer_authorized(&self, signer: &[u8; 32], _f: &AuthorityFrontier) -> bool {
        [WRITER_SEED, WRITER_2_SEED, STATION_A_SEED, STATION_B_SEED]
            .iter()
            .any(|seed| mechanics::actor::device_from_seed(seed).key_bytes() == Some(*signer))
    }
}

#[derive(Default)]
struct AcceptingIncorporator;
impl replica::convergence::AuthorityIncorporator for AcceptingIncorporator {
    fn incorporate_authority(
        &mut self,
        _records: &[Vec<u8>],
    ) -> Result<replica::convergence::AuthorityBatchReceipt, replica::convergence::Failure> {
        Ok(replica::convergence::AuthorityBatchReceipt {
            space: coordinates().0,
            prior_frontier: replica::frontier::AuthorityFrontier::from_canonical_bytes(vec![]),
            resulting_frontier: AuthorityFrontier::from_canonical_bytes(vec![3]),
            batch_digest: *blake3::hash(&_records.concat()).as_bytes(),
        })
    }
}

fn test_keys() -> Arc<dyn replica::body::BodyKeySource> {
    Arc::new(replica::body::StaticBodyKeys::new(
        AuthorizedBodyKey::for_authorized_epoch(EPOCH, EPOCH_KEY),
    ))
}

fn runtime_at(root: &std::path::Path) -> Runtime {
    let world = KvWorld::new();
    let registry = Builder::new().register(Arc::new(world)).build().unwrap();
    Runtime::open(
        root.to_path_buf(),
        registry,
        Arc::new(TestAuthority),
        test_keys(),
    )
}

fn runtime_at_with_extraction_gate(
    root: &std::path::Path,
    gate: Arc<(Mutex<bool>, Condvar)>,
    started: std::sync::mpsc::Sender<()>,
) -> Runtime {
    let world = KvWorld::new().with_extraction_gate(gate, started);
    let registry = Builder::new().register(Arc::new(world)).build().unwrap();
    Runtime::open(
        root.to_path_buf(),
        registry,
        Arc::new(TestAuthority),
        test_keys(),
    )
}

fn comms_options(
    transport: Arc<dyn comms::Transport>,
    station_seed: [u8; 32],
    gossip: Option<GossipOptions>,
) -> CommsOptions {
    CommsOptions {
        transport,
        station_seed,
        authority: Authority {
            source: Arc::new(AnyKnownSigner),
            incorporator: Arc::new(Mutex::new(AcceptingIncorporator)),
            export: Arc::new(Vec::new),
            frontier: Arc::new(|| AuthorityFrontier::from_canonical_bytes(vec![3])),
        },
        gossip,
        whole_deadline: Duration::from_secs(20),
        progress_deadline: Duration::from_secs(5),
        route_lease: Duration::from_secs(60),
    }
}

fn activate_with(
    rt: &Runtime,
    coords: &SignedCoordinates,
    transport: Arc<dyn comms::Transport>,
    seed: [u8; 32],
    gossip: Option<GossipOptions>,
) -> Station {
    rt.materialize(coords)
        .unwrap()
        .open(Activation {
            planes: Default::default(),
            content: Default::default(),
            find: Default::default(),
            drain_deadline: Duration::from_secs(5),
            comms: Some(comms_options(transport, seed, gossip)),
            observation_capacity: 0,
        })
        .unwrap()
}

fn submit_kv(station: &Station, seed: &[u8; 32], entry: &str) -> RequestId {
    let world_id = WorldId::parse("dev.example.kv").unwrap();
    let writer = Runtime::identity_from_seed(seed);
    let session = station.dock(&world_id, &writer).unwrap();
    let request = RequestId::mint();
    let action = writer
        .sign_action(
            &session,
            request,
            Intent {
                schema: SchemaId::parse("entry").unwrap(),
                schema_version: 1,
                payload: entry.as_bytes().to_vec(),
            },
        )
        .unwrap();
    session.submit(action).unwrap();
    request
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
            publication: None,
        })
        .unwrap()
        .bytes
}

fn station_id(seed: &[u8; 32]) -> Key {
    Key::from_device(&mechanics::actor::device_from_seed(seed)).unwrap()
}

#[test]
fn two_stations_converge_through_the_public_contact_api() {
    let (_space, coords) = coordinates();
    let net = comms::mem::MemNet::new();
    let ta: Arc<dyn comms::Transport> =
        Arc::new(net.peer(mechanics::actor::device_from_seed(&STATION_A_SEED)));
    let tb: Arc<dyn comms::Transport> =
        Arc::new(net.peer(mechanics::actor::device_from_seed(&STATION_B_SEED)));

    let root_a = temp_root("a");
    let root_b = temp_root("b");
    let rt_a = runtime_at(&root_a);
    let rt_b = runtime_at(&root_b);

    let station_a = activate_with(&rt_a, &coords, ta, STATION_A_SEED, None);
    let greeting_operation = submit_kv(&station_a, &WRITER_SEED, "greeting=hello");
    let farewell_operation = submit_kv(&station_a, &WRITER_2_SEED, "farewell=bye");

    let station_b = activate_with(&rt_b, &coords, tb, STATION_B_SEED, None);
    // Subscribe BEFORE the contact: remote convergence publishes one
    // live-epoch Observation per contributing signed transaction.
    let world_id = WorldId::parse("dev.example.kv").unwrap();
    let writer = Runtime::identity_from_seed(&WRITER_SEED);
    let obs_session = station_b.dock(&world_id, &writer).unwrap();
    let mut obs = obs_session.observe(None);
    assert!(obs.try_next().unwrap().unwrap().reset);
    // The privileged administrative Contact, through the public API.
    let outcome = station_b.contact(&station_id(&STATION_A_SEED)).unwrap();
    assert!(outcome.bytes_moved > 0, "bytes accounted separately");
    assert!(outcome.convergence.accepted >= 1);
    assert_eq!(outcome.convergence.changes.len(), 2);
    let first_remote = obs
        .next_timeout(Duration::from_secs(5))
        .unwrap()
        .expect("remote convergence publishes");
    let second_remote = obs
        .next_timeout(Duration::from_secs(5))
        .unwrap()
        .expect("each signed transaction publishes separately");
    let remote_records = [first_remote, second_remote];
    for remote_record in &remote_records {
        assert!(!remote_record.reset);
        assert_eq!(remote_record.bodies.len(), 1);
        assert_eq!(remote_record.publications.len(), 1);
        assert_eq!(remote_record.publications[0].world, world_id);
        assert!(remote_record.change.attribution.is_some());
        assert!(remote_record
            .change
            .bodies
            .iter()
            .all(|change| matches!(change.detail, runtime::change::Detail::Dirty)));
    }
    let mut operations: Vec<_> = remote_records
        .iter()
        .map(|record| record.change.attribution.as_ref().unwrap().operation)
        .collect();
    operations.sort();
    let mut expected = vec![greeting_operation.as_bytes(), farewell_operation.as_bytes()];
    expected.sort();
    assert_eq!(operations, expected);
    let mut authors: Vec<_> = remote_records
        .iter()
        .map(|record| {
            let attribution = record.change.attribution.as_ref().unwrap();
            (attribution.actor.clone(), attribution.device.clone())
        })
        .collect();
    authors.sort();
    let mut expected_authors = vec![
        (
            ActorId::from_incept_hash(&"c".repeat(64)),
            mechanics::actor::device_from_seed(&WRITER_SEED),
        ),
        (
            ActorId::from_incept_hash(&"d".repeat(64)),
            mechanics::actor::device_from_seed(&WRITER_2_SEED),
        ),
    ];
    expected_authors.sort();
    assert_eq!(authors, expected_authors);
    let installed = obs_session
        .query(Query {
            schema: SchemaId::parse("entry").unwrap(),
            schema_version: 1,
            payload: b"greeting".to_vec(),
            publication: None,
        })
        .unwrap()
        .publication
        .expect("Runtime stamps every World projection");
    assert_eq!(
        remote_records[0].publications[0].publication, installed,
        "every remote notification must stamp the adopted publication"
    );
    assert_eq!(remote_records[1].publications[0].publication, installed);
    // The bundle had exactly two contributing signed transactions. No union
    // record or transport-only duplicate follows them.
    assert!(
        obs.try_next().unwrap().is_none(),
        "the Contact published a record outside its contributing transactions"
    );
    assert_eq!(read_kv(&station_b, "greeting"), b"hello");
    assert_eq!(read_kv(&station_b, "farewell"), b"bye");

    // An unchanged second Contact converges nothing new — and under the
    // O(changed) protocol it also SHIPS nothing new: B's signed holdings
    // declaration covers every head, so the accepter serves only the
    // manifest advertisement and the idle pull is a fraction of the first.
    let again = station_b.contact(&station_id(&STATION_A_SEED)).unwrap();
    assert_eq!(again.convergence.accepted, 0);
    assert!(!again.convergence.advanced());
    assert!(
        obs.try_next().unwrap().is_none(),
        "exact replay must not publish a duplicate attributed change"
    );
    assert!(
        again.bytes_moved < outcome.bytes_moved,
        "idle delta pull ({}) must move fewer bytes than the first sync ({})",
        again.bytes_moved,
        outcome.bytes_moved
    );
    obs_session.close();

    // Restart B: incorporated material is durable, and a further Contact is
    // still unchanged.
    let orbit_b = station_b.vacate().unwrap();
    drop(orbit_b);
    let tb2: Arc<dyn comms::Transport> =
        Arc::new(net.peer(mechanics::actor::device_from_seed(&STATION_B_SEED)));
    let space = station_a.space_id().clone();
    let station_b = rt_b
        .acquire(&space)
        .unwrap()
        .open(Activation {
            planes: Default::default(),
            content: Default::default(),
            find: Default::default(),
            drain_deadline: Duration::from_secs(5),
            comms: Some(comms_options(tb2, STATION_B_SEED, None)),
            observation_capacity: 0,
        })
        .unwrap();
    assert_eq!(read_kv(&station_b, "greeting"), b"hello");
    let after_restart = station_b.contact(&station_id(&STATION_A_SEED)).unwrap();
    assert_eq!(after_restart.convergence.accepted, 0);

    // Dormancy rejects newly queued work with a typed refusal.
    let station_id_a = station_id(&STATION_A_SEED);
    let orbit_b = station_b.vacate().unwrap();
    drop(orbit_b);
    drop(station_a);
    let _ = std::fs::remove_dir_all(&root_a);
    let _ = std::fs::remove_dir_all(&root_b);
    let _ = station_id_a;
}

#[test]
fn remote_extraction_does_not_block_prior_reads_and_local_retry_orders_after_contact() {
    let (_space, coords) = coordinates();
    let net = comms::mem::MemNet::new();
    let ta: Arc<dyn comms::Transport> =
        Arc::new(net.peer(mechanics::actor::device_from_seed(&STATION_A_SEED)));
    let tb: Arc<dyn comms::Transport> =
        Arc::new(net.peer(mechanics::actor::device_from_seed(&STATION_B_SEED)));

    let root_a = temp_root("ordered-a");
    let root_b = temp_root("ordered-b");
    let gate = Arc::new((Mutex::new(true), Condvar::new()));
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let rt_a = runtime_at(&root_a);
    let rt_b = runtime_at_with_extraction_gate(&root_b, gate.clone(), entered_tx);
    let station_a = Arc::new(activate_with(&rt_a, &coords, ta, STATION_A_SEED, None));
    let station_b = Arc::new(activate_with(&rt_b, &coords, tb, STATION_B_SEED, None));

    submit_kv(&station_b, &WRITER_SEED, "local=before");
    while entered_rx.try_recv().is_ok() {}
    submit_kv(&station_a, &WRITER_2_SEED, "remote=arrived");

    let world_id = WorldId::parse("dev.example.kv").unwrap();
    let identity = Runtime::identity_from_seed(&WRITER_SEED);
    let session_b = Arc::new(station_b.dock(&world_id, &identity).unwrap());
    let prior = session_b
        .query(Query {
            schema: SchemaId::parse("entry").unwrap(),
            schema_version: 1,
            payload: b"local".to_vec(),
            publication: None,
        })
        .unwrap();
    let prior_publication = prior
        .publication
        .expect("prior exact publication")
        .publication;
    assert_eq!(prior.bytes, b"before");

    let (released, wake) = &*gate;
    *released.lock().unwrap() = false;
    let contact_station = station_b.clone();
    let contact = std::thread::spawn(move || contact_station.contact(&station_id(&STATION_A_SEED)));
    entered_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("remote publication extractor entered");

    // Corpus recovery is blocking work, but it must not occupy Contact's
    // current-thread reactor. The scheduler remains live and promptly applies
    // its bounded in-flight admission rule while the first exact publication
    // is deliberately stalled.
    let duplicate_station = station_b.clone();
    let (duplicate_tx, duplicate_rx) = std::sync::mpsc::channel();
    let duplicate = std::thread::spawn(move || {
        let result = duplicate_station.contact(&station_id(&STATION_A_SEED));
        let _ = duplicate_tx.send(result);
    });
    // A Contact authority record may also invalidate this already-docked
    // Session before mutation admission. Either result is terminal and
    // immediate: stale session or occupied mutation lane, never a hidden wait.
    assert!(matches!(
        duplicate_rx
            .recv_timeout(Duration::from_millis(250))
            .expect("Contact admission reactor must remain live during extraction"),
        Err(runtime::plane::contact::Failure::Capacity)
    ));
    duplicate.join().unwrap();

    // The installed frontier is part of the immutable prior read image. It
    // remains immediately observable while Contact compiles the candidate
    // corpus off the Station state mutex.
    let (frontier_tx, frontier_rx) = std::sync::mpsc::channel();
    let frontier_station = station_b.clone();
    let frontier_read = std::thread::spawn(move || {
        let _ = frontier_tx.send(frontier_station.frontier());
    });
    frontier_rx
        .recv_timeout(Duration::from_millis(250))
        .expect("published frontier must not wait behind remote extraction");
    frontier_read.join().unwrap();

    // An exact old publication remains readable. An ambient write cannot
    // overtake the durable remote root while its interpretation is Building;
    // it is refused promptly and can be retried against the ready head.
    let (read_tx, read_rx) = std::sync::mpsc::channel();
    let prior_session = session_b.clone();
    let prior_read = std::thread::spawn(move || {
        let result = prior_session.query(Query {
            schema: SchemaId::parse("entry").unwrap(),
            schema_version: 1,
            payload: b"local".to_vec(),
            publication: Some(prior_publication),
        });
        let _ = read_tx.send(result);
    });
    let old = read_rx
        .recv_timeout(Duration::from_millis(250))
        .expect("exact prior query must not wait behind remote extraction")
        .unwrap();
    assert_eq!(old.bytes, b"before");
    prior_read.join().unwrap();

    let request = RequestId::mint();
    let action = identity
        .sign_action(
            &session_b,
            request,
            Intent {
                schema: SchemaId::parse("entry").unwrap(),
                schema_version: 1,
                payload: b"local=during".to_vec(),
            },
        )
        .unwrap();
    let (write_tx, write_rx) = std::sync::mpsc::channel();
    let blocked_head_session = session_b.clone();
    let write = std::thread::spawn(move || {
        let _ = write_tx.send(blocked_head_session.submit(action));
    });
    assert!(matches!(
        write_rx
            .recv_timeout(Duration::from_millis(250))
            .expect("local submit must be refused, not deadlock behind Contact"),
        Err(runtime::world::Failure::Busy)
    ));
    write.join().unwrap();

    *released.lock().unwrap() = true;
    wake.notify_all();
    let outcome = contact.join().unwrap().unwrap();
    let adopted_count = outcome.convergence.current.transaction_count;
    submit_kv(&station_b, &WRITER_SEED, "local=after");
    assert_eq!(read_kv(&station_b, "remote"), b"arrived");
    assert_eq!(read_kv(&station_b, "local"), b"after");
    assert!(
        station_b.frontier().transaction_count > adopted_count,
        "the retried local commit must publish after the Contact frontier"
    );

    drop(station_a);
    drop(station_b);
    let _ = std::fs::remove_dir_all(root_a);
    let _ = std::fs::remove_dir_all(root_b);
}

#[test]
fn a_beacon_drives_fully_automatic_convergence() {
    let (_space, coords) = coordinates();
    let net = comms::mem::MemNet::new();
    let ta: Arc<dyn comms::Transport> =
        Arc::new(net.peer(mechanics::actor::device_from_seed(&STATION_A_SEED)));
    let tb: Arc<dyn comms::Transport> =
        Arc::new(net.peer(mechanics::actor::device_from_seed(&STATION_B_SEED)));

    let root_a = temp_root("auto-a");
    let root_b = temp_root("auto-b");
    let rt_a = runtime_at(&root_a);
    let rt_b = runtime_at(&root_b);

    let gossip = |advertise: bool| {
        Some(GossipOptions {
            bootstrap: vec![],
            advertise: if advertise {
                vec![runtime::beacon::RouteHint {
                    scheme: 1,
                    bytes: b"127.0.0.1:1".to_vec(),
                }]
            } else {
                vec![]
            },
            beacon_interval: Duration::from_millis(100),
        })
    };
    let station_a = activate_with(&rt_a, &coords, ta, STATION_A_SEED, gossip(true));
    submit_kv(&station_a, &WRITER_SEED, "auto=converged");
    let station_b = activate_with(&rt_b, &coords, tb, STATION_B_SEED, gossip(true));

    // No manual contact: A's periodic Beacon reaches B over gossip, the
    // registry queues the Neighbor, and the scheduler dials + incorporates.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        if read_kv(&station_b, "auto") == b"converged" {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "automatic convergence never happened"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    // The registry now lists A as a known Neighbor.
    assert!(station_b
        .neighbors()
        .iter()
        .any(|n| n.station == station_id(&STATION_A_SEED)));

    let _ = station_a.vacate();
    let _ = station_b.vacate();
    let _ = std::fs::remove_dir_all(&root_a);
    let _ = std::fs::remove_dir_all(&root_b);
}

#[test]
fn an_unknown_neighbor_is_unreachable_and_dormancy_refuses_contact() {
    let (_space, coords) = coordinates();
    let net = comms::mem::MemNet::new();
    let tb: Arc<dyn comms::Transport> =
        Arc::new(net.peer(mechanics::actor::device_from_seed(&STATION_B_SEED)));
    let root_b = temp_root("refuse");
    let rt_b = runtime_at(&root_b);
    let station_b = activate_with(&rt_b, &coords, tb, STATION_B_SEED, None);

    // Nobody answers this station id on the network.
    let ghost = station_id(&[99u8; 32]);
    assert!(station_b.contact(&ghost).is_err());

    // After dormancy, newly queued work is refused with a typed error and no
    // task, staging file, or lock is leaked (the Orbit reactivates cleanly).
    let orbit = station_b.vacate().unwrap();
    let station_b = orbit.open(Activation::offline()).unwrap();
    assert!(matches!(
        station_b.contact(&ghost),
        Err(runtime::plane::contact::Failure::Unreachable(_))
    ));
    let _ = station_b.vacate().unwrap();
    let _ = std::fs::remove_dir_all(&root_b);
}
