//! S7 / G10 — independent adoption conformance.
//!
//! This exercises the orbital lifecycle end to end through the **public**
//! `runtime` API only — the same surface an Issues-free consumer would use. It
//! registers a small test World and drives Space formation, activation, docking,
//! durable submit, committed-snapshot query, Observation, restart durability,
//! the double-lock, per-request authorization, and destructive remove. Nothing
//! here touches crate internals or product types.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

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
use runtime::Error as Failure;
use runtime::{
    plane::Activation, world::AuthorityView, world::Builder, world::Context, world::Effect,
    world::Intent, world::LocalIdentity, world::PrincipalResolution, world::Projection,
    world::Query, world::Rejection, world::World, RemovalConfirmation, Runtime,
};

/// The consumer's writing device; a second device resolves with no grants.
const WRITER_SEED: [u8; 32] = [51u8; 32];
const READER_SEED: [u8; 32] = [52u8; 32];

/// A view whose default `authorize_mutation` builds a structurally-valid
/// receipt — the permissive delegate for the writer-only view's allow path.
struct PermissiveAuthority;

impl AuthorityView for PermissiveAuthority {
    fn resolve(&self, _device: &DeviceId) -> Option<PrincipalResolution> {
        None
    }
}

/// The consumer-supplied mechanics view: authorizes mutations for the writer
/// device only (every known device resolves and may dock).
struct ConsumerAuthority;

impl AuthorityView for ConsumerAuthority {
    fn resolve(&self, _device: &DeviceId) -> Option<PrincipalResolution> {
        Some(PrincipalResolution {
            actor: ActorId::from_incept_hash(&"b".repeat(64)),
            authority_frontier: AuthorityFrontier::from_canonical_bytes(vec![2]),
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
            return Err("device holds no write authority".into());
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

fn writer() -> LocalIdentity {
    Runtime::identity_from_seed(&WRITER_SEED)
}

fn reader() -> LocalIdentity {
    Runtime::identity_from_seed(&READER_SEED)
}

fn test_keys() -> std::sync::Arc<dyn replica::body::BodyKeySource> {
    std::sync::Arc::new(replica::body::StaticBodyKeys::new(
        mechanics::authorization::AuthorizedBodyKey::for_authorized_epoch([1u8; 16], [2u8; 32]),
    ))
}

/// Sign and submit an intent through the frozen public action API.
fn submit_as(
    session: &runtime::Session,
    identity: &LocalIdentity,
    intent: Intent,
) -> Result<runtime::world::CommittedEffect, runtime::world::Failure> {
    session.submit(identity.sign_action(session, runtime::world::RequestId::mint(), intent)?)
}

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn temp_root() -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("lait-adoption-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A tiny key-value World: an intent `key=value` sets a Body; a query for `key`
/// returns its committed value. Deterministic and product-free.
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

    /// A stable Body id derived from the entry key.
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
            effect: value.as_bytes().to_vec(),
            declarations: vec![],
        })
    }
    fn query(&self, ctx: &Context<'_>, query: Query) -> Result<Projection, Rejection> {
        let key = String::from_utf8(query.payload).map_err(|_| Rejection::InvalidRequest)?;
        let value = ctx.read_body(&self.body(&key)).unwrap_or_default();
        Ok(Projection {
            demand: any_demand(),
            schema: SchemaId::parse("entry").unwrap(),
            schema_version: 1,
            bytes: value,
            frontier: ReplicaFrontier::EMPTY,
        })
    }
}

fn kv_runtime(root: &std::path::Path) -> Runtime {
    let world = KvWorld::new();
    let registry = Builder::new().register(Arc::new(world)).build().unwrap();
    Runtime::open(
        root.to_path_buf(),
        registry,
        Arc::new(ConsumerAuthority),
        test_keys(),
    )
}

fn world_id() -> WorldId {
    WorldId::parse("dev.example.kv").unwrap()
}

#[test]
fn a_consumer_drives_the_whole_lifecycle_through_the_public_api() {
    let root = temp_root();
    let rt = kv_runtime(&root);

    // Form a Space and activate its Orbit.
    let orbit = rt.create().unwrap();
    let space = orbit.space_id().clone();
    let station = orbit.open(Activation::default()).unwrap();

    // Observation is advisory and reports the Space present + locked.
    let obs = rt.inspect(&space).unwrap();
    assert!(obs.locked);
    assert_eq!(rt.list().len(), 1);

    // Dock and durably submit two entries.
    let session = station.dock(&world_id(), &writer()).unwrap();
    let c1 = submit_as(
        &session,
        &writer(),
        Intent {
            schema: SchemaId::parse("entry").unwrap(),
            schema_version: 1,
            payload: b"greeting=hello".to_vec(),
        },
    )
    .unwrap();
    assert_eq!(c1.bodies.len(), 1);
    assert_ne!(c1.frontier, ReplicaFrontier::EMPTY);
    let c2 = submit_as(
        &session,
        &writer(),
        Intent {
            schema: SchemaId::parse("entry").unwrap(),
            schema_version: 1,
            payload: b"farewell=bye".to_vec(),
        },
    )
    .unwrap();
    assert_eq!(c2.bodies.len(), 1);
    assert_ne!(c1.frontier, c2.frontier);
    assert_eq!(station.frontier(), c2.frontier);

    // Query reads the committed snapshot.
    let read = |s: &runtime::Session, key: &str| {
        s.query(Query {
            schema: SchemaId::parse("entry").unwrap(),
            schema_version: 1,
            payload: key.as_bytes().to_vec(),
        })
        .unwrap()
        .bytes
    };
    assert_eq!(read(&session, "greeting"), b"hello");
    assert_eq!(read(&session, "farewell"), b"bye");

    // Go dormant (checkpoints), then reactivate: committed entries survive.
    let orbit = station.vacate().unwrap();
    drop(orbit);
    let station = rt
        .acquire(&space)
        .unwrap()
        .open(Activation::default())
        .unwrap();
    let session = station.dock(&world_id(), &writer()).unwrap();
    assert_eq!(read(&session, "greeting"), b"hello");
    assert_eq!(read(&session, "farewell"), b"bye");

    // A double acquisition while the Station is live is refused.
    assert!(matches!(rt.acquire(&space), Err(Failure::ReplicaLocked(_))));

    // Per-request authorization: a read-only principal cannot write.
    let readonly = station.dock(&world_id(), &reader()).unwrap();
    assert_eq!(
        submit_as(
            &readonly,
            &reader(),
            Intent {
                schema: SchemaId::parse("entry").unwrap(),
                schema_version: 1,
                payload: b"x=y".to_vec(),
            }
        ),
        Err(runtime::world::Failure::Rejected(Rejection::Denied))
    );

    // remove destroys the Space.
    let orbit = station.vacate().unwrap();
    orbit
        .remove(RemovalConfirmation::for_space(space.clone()))
        .unwrap();
    assert!(matches!(rt.acquire(&space), Err(Failure::OrbitNotFound(_))));
}

#[test]
fn an_unregistered_world_cannot_be_docked() {
    let root = temp_root();
    let rt = kv_runtime(&root);
    let station = rt.create().unwrap().open(Activation::default()).unwrap();
    let unknown = WorldId::parse("dev.example.other").unwrap();
    assert!(station.dock(&unknown, &writer()).is_err());
}
