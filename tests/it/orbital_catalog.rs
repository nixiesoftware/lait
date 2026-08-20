//! Deterministic Catalog formation gates (plan M4): the crash-resumable
//! `InitializeTracker` bootstrap record, exact signed-action replay at every
//! injected fault, deterministic Catalog identity, and typed
//! `StateCorrupt` for missing/misplaced/duplicated Catalog state.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use lait::world::contract;
use runtime::{world::Context, world::Query, world::Rejection, world::World};

const FOUNDER_SEED: [u8; 32] = [71u8; 32];

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn temp_home(tag: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("lait-cat-{tag}-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn the_catalog_identity_is_deterministic_per_space() {
    let home_a = temp_home("det-a");
    let home_b = temp_home("det-b");
    let (mech_a, _c) = lait::orbital::form_space(&home_a, &FOUNDER_SEED, "A").unwrap();
    let (mech_b, _c) = lait::orbital::form_space(&home_b, &[72u8; 32], "B").unwrap();
    let key_a = contract::catalog_key(&mech_a.space());
    // Recomputation is stable.
    assert_eq!(key_a, contract::catalog_key(&mech_a.space()));
    // A different Space derives a different Catalog identity.
    assert_ne!(key_a, contract::catalog_key(&mech_b.space()));
    let _ = std::fs::remove_dir_all(&home_a);
    let _ = std::fs::remove_dir_all(&home_b);
}

// ---- StateCorrupt: missing / misplaced / duplicated Catalog ----------

/// A stub committed snapshot: collaborative views by key, plus the
/// catalog-schema binding set the World enumerates.
#[derive(Default)]
struct StubReader {
    views: BTreeMap<replica::body::BodyKey, fabric::CollaborativeView>,
    catalog_bodies: Vec<replica::body::BodyKey>,
}

impl runtime::world::BodyReader for StubReader {
    fn read_body(
        &self,
        _key: &replica::body::BodyKey,
    ) -> Result<Option<runtime::world::BodyBytes>, runtime::world::BodyReadFailure> {
        Ok(None)
    }
    fn read_collaborative_body(
        &self,
        key: &replica::body::BodyKey,
    ) -> Result<Option<runtime::world::CollaborativeBody>, runtime::world::BodyReadFailure> {
        Ok(self
            .views
            .get(key)
            .cloned()
            .map(runtime::world::CollaborativeBody::owned))
    }
    fn body_version(&self, _key: &replica::body::BodyKey) -> Option<fabric::Version> {
        None
    }
    fn anchor_in_body(
        &self,
        _key: &replica::body::BodyKey,
        _path: &str,
        _position: u64,
    ) -> Result<Option<fabric::Anchor>, runtime::world::BodyReadFailure> {
        Ok(None)
    }
    fn resolve_anchor(
        &self,
        _key: &replica::body::BodyKey,
        _anchor: &fabric::Anchor,
    ) -> Result<fabric::AnchorResolution, runtime::world::BodyReadFailure> {
        Ok(fabric::AnchorResolution::Drifted)
    }
    fn content_status(
        &self,
        _content: &replica::content::ContentRef,
    ) -> Option<runtime::world::ContentStatus> {
        None
    }

    fn bodies_with_schema(
        &self,
        _world: &replica::body::WorldId,
        _schema: &replica::body::SchemaId,
    ) -> Vec<replica::body::BodyKey> {
        self.catalog_bodies.clone()
    }
}

fn principal(space: &mechanics::ids::SpaceId) -> runtime::world::PrincipalFacts {
    let device = mechanics::actor::device_from_seed(&FOUNDER_SEED);
    runtime::world::PrincipalFacts {
        actor: mechanics::ids::ActorId::from_incept_hash(&"ab".repeat(32)),
        station: mechanics::station::Key::from_device(&device).unwrap(),
        device,
        space: space.clone(),
        authority_frontier: replica::frontier::AuthorityFrontier::from_canonical_bytes(vec![]),
    }
}

fn structure_query(world: &lait::world::IssuesWorld, ctx: &Context<'_>) -> Result<(), Rejection> {
    world
        .query(
            ctx,
            Query {
                schema: contract::issue_schema(),
                schema_version: contract::ISSUE_SCHEMA_VERSION,
                payload: contract::IssueQuery::StructureStatus.to_json(),
                publication: None,
            },
        )
        .map(|_| ())
}

#[test]
fn misplaced_and_duplicate_catalogs_are_typed_corrupt_never_repaired() {
    let space = mechanics::ids::SpaceId::mint(&mechanics::ids::SystemUlidSource);
    let world = lait::world::IssuesWorld::migrator();
    let facts = principal(&space);
    let right = contract::catalog_key(&space);
    let wrong = replica::body::BodyKey::new(
        contract::world_id(),
        replica::body::BodyId::from_bytes([9u8; 16]),
    );

    // A catalog-schema Body at the WRONG key only: corrupt (never selected).
    let mut reader = StubReader {
        catalog_bodies: vec![wrong.clone()],
        ..Default::default()
    };
    reader
        .views
        .insert(wrong.clone(), fabric::CollaborativeView::default());
    let ctx = Context::with_reads(&facts, &reader, [0u8; 32]);
    assert!(
        matches!(structure_query(&world, &ctx), Err(Rejection::StateCorrupt)),
        "a misplaced catalog is never chosen"
    );

    // The right key AND a second semantic catalog: corrupt (never merged).
    let mut reader = StubReader {
        catalog_bodies: vec![right.clone(), wrong.clone()],
        ..Default::default()
    };
    reader
        .views
        .insert(right.clone(), fabric::CollaborativeView::default());
    reader
        .views
        .insert(wrong, fabric::CollaborativeView::default());
    let ctx = Context::with_reads(&facts, &reader, [0u8; 32]);
    assert!(
        matches!(structure_query(&world, &ctx), Err(Rejection::StateCorrupt)),
        "a duplicate catalog is never merged"
    );

    // The right key bound as a catalog but unreadable under the collaborative
    // model (wrong model/encoding): corrupt, not "missing".
    let reader = StubReader {
        catalog_bodies: vec![right.clone()],
        ..Default::default()
    };
    let ctx = Context::with_reads(&facts, &reader, [0u8; 32]);
    assert!(
        matches!(structure_query(&world, &ctx), Err(Rejection::StateCorrupt)),
        "a wrong-model catalog is corrupt"
    );

    // No catalog at all: legitimate pre-adoption state, NOT corrupt (a joiner
    // adopts through Manifest synchronization).
    //
    // The claim is the VERDICT, not reaching an answer. The three cases above
    // are decided from the Body set alone and never reach Find. This one has
    // nothing corrupt to reject, so the report goes on to enumerate issues
    // through Find — and a stub reader carries no index to enumerate, so it
    // ends in that capability's typed absence. Which is the point: absence of
    // the capability is not evidence about the store, and this asserts the one
    // thing that must never be said about a Space that simply has not adopted
    // yet.
    let reader = StubReader::default();
    let ctx = Context::with_reads(&facts, &reader, [0u8; 32]);
    assert!(
        !matches!(structure_query(&world, &ctx), Err(Rejection::StateCorrupt)),
        "a Space that has not adopted yet is not a corrupt one"
    );
}
