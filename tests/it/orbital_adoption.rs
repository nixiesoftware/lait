//! The product composes the orbital lifecycle through its mechanics-only
//! adoption seam (`lait::orbital`): store-root convention + caller-supplied
//! registry + caller-supplied mechanics authority view. The World used here is
//! an **independent example World** (a tiny counter journal) — deliberately not
//! product semantics: per O13/O23 the product ships no first-party World, and
//! the Issues adapter arrives with the daemon integration as an adapter over
//! the existing product behavior.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use lait::orbital::{open_orbital_runtime, orbital_store_root, WorldPackages};
use mechanics::ids::{ActorId, DeviceId};
use runtime::{
    plane::Activation, world::AuthorityView, world::Builder, world::Context, world::Effect,
    world::Intent, world::PrincipalResolution, world::Projection, world::Query, world::Rejection,
    world::World, Runtime,
};

use ::replica::body::{BodyId, BodyKey, EncodingId, SchemaId, WorldId};
use ::replica::body::{MutationModel, Op, Schema};
use ::replica::frontier::{AuthorityFrontier, ReplicaFrontier};

#[allow(dead_code)]
fn any_demand() -> Vec<u8> {
    mechanics::authorization::AuthorizationDemand::require(
        mechanics::authorization::PolicyCapability::new("w", "c"),
        mechanics::authorization::Resource::root("w"),
    )
    .encode_canonical()
    .expect("canonical demand")
}

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn temp_home() -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("lait-orbital-adopt-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

const WRITER_SEED: [u8; 32] = [61u8; 32];

/// The example deployment's mechanics view: the writer device gets Write.
struct ExampleAuthority;

impl AuthorityView for ExampleAuthority {
    fn resolve(&self, device: &DeviceId) -> Option<PrincipalResolution> {
        let writer = mechanics::actor::device_from_seed(&WRITER_SEED);
        (device == &writer).then(|| PrincipalResolution {
            actor: ActorId::from_incept_hash(&"c".repeat(64)),
            authority_frontier: AuthorityFrontier::from_canonical_bytes(vec![3]),
        })
    }
}

/// An independent example World: a single tally Body; an intent increments it
/// by the payload's byte length, a query returns the tally as decimal ASCII.
struct TallyWorld {
    id: WorldId,
    body_id: BodyId,
    schemas: Vec<Schema>,
}

impl TallyWorld {
    fn new() -> Self {
        Self::named("dev.example.tally", 9)
    }

    fn named(id: &str, body_marker: u8) -> Self {
        Self {
            id: WorldId::parse(id).unwrap(),
            body_id: BodyId::from_bytes([body_marker; 16]),
            schemas: vec![Schema {
                id: SchemaId::parse("tally").unwrap(),
                version: 1,
                encoding: EncodingId::parse("ascii.decimal").unwrap(),
                mutation: MutationModel::Atomic,
                readable_predecessors: vec![],
            }],
        }
    }
    fn body(&self) -> BodyKey {
        BodyKey::new(self.id.clone(), self.body_id.clone())
    }
    fn current(&self, ctx: &Context<'_>) -> Result<u64, Rejection> {
        let Some(bytes) = ctx.read_body(&self.body())? else {
            return Ok(0);
        };
        let value = std::str::from_utf8(&bytes).map_err(|_| Rejection::StateCorrupt)?;
        value.parse().map_err(|_| Rejection::StateCorrupt)
    }
}

impl World for TallyWorld {
    fn id(&self) -> WorldId {
        self.id.clone()
    }
    fn schemas(&self) -> &[Schema] {
        &self.schemas
    }
    fn submit(&self, ctx: &mut Context<'_>, intent: Intent) -> Result<Effect, Rejection> {
        let next = self.current(ctx)? + intent.payload.len() as u64;
        let key = self.body();
        Ok(Effect {
            content_refs: Vec::new(),
            exec: Vec::new(),
            demand: any_demand(),
            operations: vec![(
                key.clone(),
                Op::ReplaceAtomic {
                    value: next.to_string().into_bytes(),
                },
            )],
            bodies: vec![key],
            effect: next.to_string().into_bytes(),
            declarations: vec![],
        })
    }
    fn query(&self, ctx: &Context<'_>, _query: Query) -> Result<Projection, Rejection> {
        Ok(Projection {
            demand: any_demand(),
            schema: SchemaId::parse("tally").unwrap(),
            schema_version: 1,
            bytes: self.current(ctx)?.to_string().into_bytes(),
            frontier: ReplicaFrontier::EMPTY, // overwritten by Runtime
            publication: None,
        })
    }
}

/// Sign and submit an intent through the frozen public action API.
fn submit_as(
    session: &runtime::Session,
    identity: &runtime::world::LocalIdentity,
    intent: Intent,
) -> Result<runtime::world::CommittedEffect, runtime::world::Failure> {
    session.submit(identity.sign_action(session, runtime::world::RequestId::mint(), intent)?)
}

#[test]
fn the_product_composes_the_orbital_runtime_for_an_independent_world() {
    let home = temp_home();
    let world = TallyWorld::new();
    let world_id = world.id();
    let registry = Builder::new().register(Arc::new(world)).build().unwrap();

    // The product's composition seam: store-root convention + supplied parts.
    let keys = Arc::new(replica::body::StaticBodyKeys::new(
        mechanics::authorization::AuthorizedBodyKey::for_authorized_epoch([1u8; 16], [2u8; 32]),
    ));
    let rt = open_orbital_runtime(&home, registry, Arc::new(ExampleAuthority), keys).unwrap();
    assert!(orbital_store_root(&home).ends_with("orbital"));

    let writer = Runtime::identity_from_seed(&WRITER_SEED);
    let orbit = rt.create().unwrap();
    let space = orbit.space_id().clone();
    let station = orbit.open(Activation::default()).unwrap();
    let session = station.dock(&world_id, &writer).unwrap();

    // Two increments: 5 then 3 bytes.
    submit_as(
        &session,
        &writer,
        Intent {
            schema: SchemaId::parse("tally").unwrap(),
            schema_version: 1,
            payload: b"hello".to_vec(),
        },
    )
    .unwrap();
    let second = submit_as(
        &session,
        &writer,
        Intent {
            schema: SchemaId::parse("tally").unwrap(),
            schema_version: 1,
            payload: b"add".to_vec(),
        },
    )
    .unwrap();
    assert_eq!(second.effect, b"8");
    assert_eq!(second.bodies.len(), 1);

    // The store lives under the product's orbital root.
    assert!(orbital_store_root(&home).join(space.as_str()).is_dir());

    // Restart durability through the product seam.
    let orbit = station.vacate().unwrap();
    drop(orbit);
    let station = rt
        .acquire(&space)
        .unwrap()
        .open(Activation::default())
        .unwrap();
    let session = station.dock(&world_id, &writer).unwrap();
    let proj = session
        .query(Query {
            schema: SchemaId::parse("tally").unwrap(),
            schema_version: 1,
            payload: vec![],
            publication: None,
        })
        .unwrap();
    assert_eq!(proj.bytes, b"8");
    // Runtime stamped the real committed frontier onto the projection.
    assert_eq!(proj.frontier, second.frontier);
}

#[test]
fn one_station_hosts_and_routes_two_worlds_independently() {
    let home = temp_home();
    let files = TallyWorld::named("dev.example.files", 7);
    let notes = TallyWorld::named("dev.example.notes", 8);
    let files_id = files.id();
    let notes_id = notes.id();
    let (registry, worlds) = WorldPackages::new()
        .register(Arc::new(files), [7; 32])
        .register(Arc::new(notes), [8; 32])
        .build()
        .unwrap();

    let keys = Arc::new(replica::body::StaticBodyKeys::new(
        mechanics::authorization::AuthorizedBodyKey::for_authorized_epoch([1u8; 16], [2u8; 32]),
    ));
    let rt = open_orbital_runtime(&home, registry, Arc::new(ExampleAuthority), keys).unwrap();
    let writer = Runtime::identity_from_seed(&WRITER_SEED);
    let station = rt.create().unwrap().open(Activation::default()).unwrap();

    worlds.ensure_primary(&station, &files_id, &writer).unwrap();
    worlds.ensure_primary(&station, &notes_id, &writer).unwrap();

    let intent = |payload: &[u8]| Intent {
        schema: SchemaId::parse("tally").unwrap(),
        schema_version: 1,
        payload: payload.to_vec(),
    };
    let files_effect = worlds
        .with_primary(&files_id, |session| {
            submit_as(session, &writer, intent(b"file"))
        })
        .expect("files Session")
        .unwrap();
    let notes_effect = worlds
        .with_primary(&notes_id, |session| {
            submit_as(session, &writer, intent(b"notes"))
        })
        .expect("notes Session")
        .unwrap();

    assert_eq!(files_effect.effect, b"4");
    assert_eq!(notes_effect.effect, b"5");
    let query = Query {
        schema: SchemaId::parse("tally").unwrap(),
        schema_version: 1,
        payload: vec![],
        publication: None,
    };
    assert_eq!(
        worlds
            .with_primary(&files_id, |session| session.query(query.clone()))
            .expect("files Session")
            .unwrap()
            .bytes,
        b"4"
    );
    assert_eq!(
        worlds
            .with_primary(&notes_id, |session| session.query(query))
            .expect("notes Session")
            .unwrap()
            .bytes,
        b"5"
    );
}

#[test]
fn a_legacy_home_is_refused_with_recreation_guidance_and_never_overwritten() {
    let home = temp_home();
    // A pre-orbital store signature.
    std::fs::create_dir_all(home.join("repo")).unwrap();
    std::fs::write(home.join("repo").join("genesis.json"), b"{}").unwrap();
    let registry = Builder::new().build().unwrap();
    let keys = Arc::new(replica::body::StaticBodyKeys::new(
        mechanics::authorization::AuthorizedBodyKey::for_authorized_epoch([1u8; 16], [2u8; 32]),
    ));
    let err = match open_orbital_runtime(&home, registry, Arc::new(ExampleAuthority), keys) {
        Err(err) => err,
        Ok(_) => panic!("a legacy home must be refused"),
    };
    assert!(err.guidance.contains("clean break"));
    assert!(err.to_string().contains("unsupported store version"));
    // Nothing orbital was created beside the legacy home.
    assert!(
        !orbital_store_root(&home).exists(),
        "no fresh Orbit beside a detected old home"
    );
}
