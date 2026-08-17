//! C3 / G7 — the bounded Observation stream, through the public API.
//!
//! Frozen v1 semantics: exclusive `(epoch, sequence)` cursors; `observe(None)`
//! or a cursor from another epoch yields exactly one reset record then live
//! delivery; an in-window cursor replays retained records; an overrun yields
//! one reset and discards the gap; publications happen once per durable
//! commit (never before durability, never for a refused request or an
//! idempotent replay); sequences are monotonic within an activation epoch;
//! dormancy ends streams with a typed `StationDormant`.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use mechanics::authorization::AuthorizedBodyKey;
use mechanics::ids::{ActorId, DeviceId};
use replica::body::{BodyId, BodyKey, EncodingId, SchemaId, WorldId};
use replica::body::{MutationModel, Op, Schema};
use replica::frontier::{AuthorityFrontier, ReplicaFrontier};

#[allow(dead_code)]
fn any_demand() -> Vec<u8> {
    mechanics::authorization::AuthorizationDemand::require(
        mechanics::authorization::PolicyCapability::new("w", "c"),
        mechanics::authorization::Resource::root("w"),
    )
    .encode_canonical()
    .expect("canonical demand")
}
use runtime::session::{Failure as SessionFailure, Interruption};
use runtime::{
    plane::Activation, world::Builder, world::Context, world::Effect, world::Intent,
    world::LocalIdentity, world::ObservationCursor, world::Projection, world::Query,
    world::Rejection, world::RequestId, world::World, Runtime, Session, Station,
};

const WRITER_SEED: [u8; 32] = [55u8; 32];
const READER_SEED: [u8; 32] = [56u8; 32];

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn temp_root() -> std::path::PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("lait-obs-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
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
            bytes: ctx.read_body(&self.body(&key)).unwrap_or_default(),
            frontier: ReplicaFrontier::EMPTY,
            publication: None,
        })
    }
}

struct WriterOnly;

/// A view whose default `authorize_mutation` builds a structurally-valid
/// receipt — the permissive delegate for the writer-only view's allow path.
struct PermissiveAuthority;

impl runtime::world::AuthorityView for PermissiveAuthority {
    fn resolve(&self, _device: &DeviceId) -> Option<runtime::world::PrincipalResolution> {
        None
    }
}

impl runtime::world::AuthorityView for WriterOnly {
    fn resolve(&self, _device: &DeviceId) -> Option<runtime::world::PrincipalResolution> {
        Some(runtime::world::PrincipalResolution {
            actor: ActorId::from_incept_hash(&"e".repeat(64)),
            authority_frontier: AuthorityFrontier::from_canonical_bytes(vec![5]),
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn authorize_mutation(
        &self,
        space: &mechanics::ids::SpaceId,
        world: &WorldId,
        actor: &ActorId,
        device: &DeviceId,
        authority_frontier: &AuthorityFrontier,
        parent_manifest_root: [u8; 32],
        implementation_id: [u8; 32],
        intent_digest: [u8; 32],
        demand: &[u8],
        operations_digest: [u8; 32],
        core_digest: [u8; 32],
    ) -> Result<Vec<u8>, mechanics::authorization::Refusal> {
        // The coarse per-device write gate lives in the view, as the orbital
        // composition's demand evaluation does — never in the World callback.
        let writer = mechanics::actor::device_from_seed(&WRITER_SEED);
        if device != &writer {
            return Err(mechanics::authorization::Refusal::Denied(
                mechanics::authorization::DenialReason::DemandUnsatisfied,
            ));
        }
        PermissiveAuthority.authorize_mutation(
            space,
            world,
            actor,
            device,
            authority_frontier,
            parent_manifest_root,
            implementation_id,
            intent_digest,
            demand,
            operations_digest,
            core_digest,
        )
    }
}

fn runtime_at(root: &std::path::Path) -> Runtime {
    let world = KvWorld::new();
    let registry = Builder::new().register(Arc::new(world)).build().unwrap();
    Runtime::open(
        root.to_path_buf(),
        registry,
        Arc::new(WriterOnly),
        Arc::new(replica::body::StaticBodyKeys::new(
            AuthorizedBodyKey::for_authorized_epoch([17u8; 16], [18u8; 32]),
        )),
    )
}

fn station_with_capacity(root: &std::path::Path, capacity: usize) -> Station {
    runtime_at(root)
        .create()
        .unwrap()
        .open(Activation {
            planes: Default::default(),
            content: Default::default(),
            find: Default::default(),
            drain_deadline: Duration::from_secs(5),
            comms: None,
            observation_capacity: capacity,
        })
        .unwrap()
}

fn dock(station: &Station) -> (Session, LocalIdentity) {
    let world_id = WorldId::parse("dev.example.kv").unwrap();
    let writer = Runtime::identity_from_seed(&WRITER_SEED);
    let session = station.dock(&world_id, &writer).unwrap();
    (session, writer)
}

fn action(
    session: &Session,
    identity: &LocalIdentity,
    request: RequestId,
    entry: &str,
) -> runtime::world::SignedWorldAction {
    identity
        .sign_action(
            session,
            request,
            Intent {
                schema: SchemaId::parse("entry").unwrap(),
                schema_version: 1,
                payload: entry.as_bytes().to_vec(),
            },
        )
        .unwrap()
}

#[test]
fn first_use_resets_then_each_durable_commit_publishes_exactly_once() {
    let root = temp_root();
    let station = station_with_capacity(&root, 0);
    let (session, writer) = dock(&station);

    let mut stream = session.observe(None);
    let first = stream.try_next().unwrap().unwrap();
    assert!(first.reset, "first use rebaselines");
    assert!(stream.try_next().unwrap().is_none(), "exactly one reset");

    // A durable commit publishes exactly one record with its scopes.
    session
        .submit(action(&session, &writer, RequestId::mint(), "a=1"))
        .unwrap();
    let record = stream.try_next().unwrap().unwrap();
    assert!(!record.reset);
    assert_eq!(record.bodies.len(), 1);
    assert!(record.sequence > first.sequence, "monotonic");
    assert!(stream.try_next().unwrap().is_none(), "published ONCE");

    // A refused request publishes nothing.
    let reader = Runtime::identity_from_seed(&READER_SEED);
    let world_id = WorldId::parse("dev.example.kv").unwrap();
    let denied_session = station.dock(&world_id, &reader).unwrap();
    let denied = denied_session.submit(action(&denied_session, &reader, RequestId::mint(), "x=y"));
    assert_eq!(
        denied,
        Err(SessionFailure::Rejected(Rejection::Denied(
            crate::world::DeniedCause::DemandUnsatisfied
        )))
    );
    assert!(stream.try_next().unwrap().is_none());

    // An idempotent replay publishes nothing either.
    let request = RequestId::from_bytes([9u8; 16]);
    let signed = action(&session, &writer, request, "b=2");
    session.submit(signed.clone()).unwrap();
    let _ = stream.try_next().unwrap().unwrap();
    session.submit(signed).unwrap();
    assert!(
        stream.try_next().unwrap().is_none(),
        "a replay commits nothing and publishes nothing"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn an_in_window_cursor_replays_then_follows_live() {
    let root = temp_root();
    let station = station_with_capacity(&root, 0);
    let (session, writer) = dock(&station);
    session
        .submit(action(&session, &writer, RequestId::mint(), "a=1"))
        .unwrap();
    session
        .submit(action(&session, &writer, RequestId::mint(), "b=2"))
        .unwrap();

    // A fresh stream resets at the current sequence; its cursor then REPLAYS
    // nothing (exclusive) until new commits arrive.
    let mut stream = session.observe(None);
    let reset = stream.try_next().unwrap().unwrap();
    assert!(reset.reset);
    assert!(stream.try_next().unwrap().is_none());

    // A cursor from sequence 0 of THIS epoch replays both retained records.
    let mut replay = session.observe(Some(ObservationCursor {
        epoch: session.epoch(),
        sequence: 0,
    }));
    let r1 = replay.try_next().unwrap().unwrap();
    let r2 = replay.try_next().unwrap().unwrap();
    assert!(!r1.reset && !r2.reset);
    assert!(r1.sequence < r2.sequence);
    assert!(replay.try_next().unwrap().is_none());

    // …then follows live delivery.
    session
        .submit(action(&session, &writer, RequestId::mint(), "c=3"))
        .unwrap();
    let live = replay
        .next_timeout(Duration::from_secs(5))
        .unwrap()
        .unwrap();
    assert_eq!(live.sequence, r2.sequence + 1);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn an_overrun_yields_one_reset_and_discards_the_gap() {
    let root = temp_root();
    // Capacity 1: only the newest record is retained.
    let station = station_with_capacity(&root, 1);
    let (session, writer) = dock(&station);
    for i in 0..3 {
        session
            .submit(action(
                &session,
                &writer,
                RequestId::mint(),
                &format!("k{i}=v"),
            ))
            .unwrap();
    }
    // A cursor pointing into the discarded gap gets exactly one reset, then
    // the retained tail.
    let mut stream = session.observe(Some(ObservationCursor {
        epoch: session.epoch(),
        sequence: 0,
    }));
    let reset = stream.try_next().unwrap().unwrap();
    assert!(reset.reset, "overrun rebaselines");
    assert!(
        stream.try_next().unwrap().is_none(),
        "the gap is discarded, not replayed"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn restart_and_cross_epoch_cursors_reset() {
    let root = temp_root();
    let rt = runtime_at(&root);
    let station = rt.create().unwrap().open(Activation::offline()).unwrap();
    let space = station.space_id().clone();
    let (session, writer) = dock(&station);
    session
        .submit(action(&session, &writer, RequestId::mint(), "a=1"))
        .unwrap();
    let old_epoch = session.epoch();

    // Crash after durability, before any consumer observed: recovery is reset
    // + re-query, never a durable outbox.
    let orbit = station.vacate().unwrap();
    drop(orbit);
    let station = rt
        .acquire(&space)
        .unwrap()
        .open(Activation::offline())
        .unwrap();
    let (session, _) = dock(&station);
    let mut stream = session.observe(Some(ObservationCursor {
        epoch: old_epoch,
        sequence: 1,
    }));
    let record = stream.try_next().unwrap().unwrap();
    assert!(record.reset, "a cursor from another epoch resets");
    // The committed state is re-queried, not replayed.
    let projection = session
        .query(Query {
            schema: SchemaId::parse("entry").unwrap(),
            schema_version: 1,
            payload: b"a".to_vec(),
            publication: None,
        })
        .unwrap();
    assert_eq!(projection.bytes, b"1");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn dormancy_terminates_streams_typed_and_concurrent_sessions_both_receive() {
    let root = temp_root();
    let station = station_with_capacity(&root, 0);
    let (s1, writer) = dock(&station);
    let (s2, _) = dock(&station);
    let mut stream1 = s1.observe(None);
    let mut stream2 = s2.observe(None);
    let _ = stream1.try_next().unwrap();
    let _ = stream2.try_next().unwrap();

    s1.submit(action(&s1, &writer, RequestId::mint(), "a=1"))
        .unwrap();
    assert!(stream1.try_next().unwrap().unwrap().sequence >= 1);
    assert!(stream2.try_next().unwrap().unwrap().sequence >= 1);

    let _ = station.vacate().unwrap();
    assert_eq!(
        stream1.next_timeout(Duration::from_secs(1)),
        Err(Interruption::StationDormant)
    );
    assert_eq!(stream2.try_next(), Err(Interruption::StationDormant));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_live_station_can_hold_content_and_reclaims_a_dead_run_at_activation() {
    // The content plane was unreachable: `Residency::open` and
    // `ContentHost::new` had no production caller, so everything plan 13 built
    // could be exercised only from a test that constructed them by hand.
    // Activation opens one cache per Station, and one is the operative number —
    // two caches over one directory would sweep each other's staging.
    use runtime::content_host::{ContentAction, ContentPolicy};

    let root = temp_root();
    let rt = runtime_at(&root);
    let station = rt.create().unwrap().open(Activation::offline()).unwrap();

    // Leave behind exactly what a killed run leaves: a staging slot and an
    // operation lease that no live transfer holds.
    let host = station.content();
    let cache = host.cache();
    let dead = [0xEEu8; 16];
    cache.append_staged(&dead, 0, 0, b"half a chunk").unwrap();
    let orphan = journal::object_content_hash(b"orphan");
    cache.install(&orphan, b"orphan", b"proof").unwrap();
    cache.hold_operation(dead, orphan).unwrap();
    assert!(cache.staged_bytes() > 0);
    assert!(cache.is_held(&orphan).unwrap());
    let space_id = station.space_id().clone();
    station.vacate().expect("dormant");

    // Reactivating is what says the operation is over.
    let station = rt
        .acquire(&space_id)
        .unwrap()
        .open(Activation::offline())
        .unwrap();
    let host = station.content();
    let cache = host.cache();
    assert_eq!(cache.staged_bytes(), 0, "a dead run's staging is reclaimed");
    assert!(
        !cache.is_held(&orphan).unwrap(),
        "and its operation lease is released"
    );

    // And the plane works: ingest through the real host, read it back.
    struct Keys;
    impl runtime::content_host::ContentKeys for Keys {
        fn sealing_key(&self) -> Option<AuthorizedBodyKey> {
            Some(AuthorizedBodyKey::for_authorized_epoch(
                [3u8; 16], [4u8; 32],
            ))
        }
        fn opening_key(&self, _epoch: &[u8; 16]) -> Option<AuthorizedBodyKey> {
            Some(AuthorizedBodyKey::for_authorized_epoch(
                [3u8; 16], [4u8; 32],
            ))
        }
    }
    let space = station.space_id().clone();
    let allow = |_: ContentAction| Ok(());
    let policy = ContentPolicy {
        space: &space,
        keys: Arc::new(Keys),
        authorize: &allow,
        max_content_len: u64::MAX,
    };
    let signer = replica::transaction::SeedSigner(&WRITER_SEED);
    let ctx = replica::transaction::CommitContext {
        space: &space,
        signer: &signer,
        authority_frontier: AuthorityFrontier::from_canonical_bytes(vec![9]),
    };
    let plaintext = vec![7u8; 5_000];
    let content = station
        .content()
        .ingest(
            &policy,
            [1u8; 16],
            &mut std::io::Cursor::new(plaintext.clone()),
            &ctx,
        )
        .expect("ingest through a live Station");
    assert_eq!(
        station
            .content()
            .read_range(&policy, &content, 0, plaintext.len())
            .unwrap(),
        plaintext
    );
    let _ = std::fs::remove_dir_all(&root);
}
