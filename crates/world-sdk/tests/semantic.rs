use std::path::{Path, PathBuf};

use mechanics::ids::ActorId;
use mechanics::station::Key;
use replica::body::{BodyId, BodyKey, SchemaId, WorldId};
use runtime::world::{
    BodyBytes, BodyReadFailure, BodyReader, Context, PrincipalFacts, Query, World,
};
use world_runner::Provenance;
use world_runner::{Instance, Release};

fn fixture_binary() -> PathBuf {
    let suffix = if cfg!(windows) { ".exe" } else { "" };
    let test_binary = std::env::current_exe().expect("running semantic test binary");
    let profile_dir = test_binary
        .parent()
        .and_then(Path::parent)
        .expect("Cargo test binary under <target>/<profile>/deps");
    profile_dir.join(format!("world-semantic-fixture{suffix}"))
}

struct Reads {
    body: BodyKey,
}

impl BodyReader for Reads {
    fn read_body(&self, key: &BodyKey) -> Result<Option<BodyBytes>, BodyReadFailure> {
        Ok((key == &self.body).then(|| BodyBytes::owned(b"through-the-host".to_vec())))
    }

    fn read_collaborative_body(
        &self,
        _key: &BodyKey,
    ) -> Result<Option<runtime::world::CollaborativeBody>, BodyReadFailure> {
        Ok(None)
    }

    fn bodies_with_schema(&self, _world: &WorldId, _schema: &SchemaId) -> Vec<BodyKey> {
        vec![self.body.clone()]
    }

    fn body_version(&self, _key: &BodyKey) -> Option<fabric::Version> {
        None
    }

    fn anchor_in_body(
        &self,
        _key: &BodyKey,
        _path: &str,
        _position: u64,
    ) -> Result<Option<fabric::Anchor>, BodyReadFailure> {
        Ok(None)
    }

    fn resolve_anchor(
        &self,
        _key: &BodyKey,
        _anchor: &fabric::Anchor,
    ) -> Result<fabric::AnchorResolution, BodyReadFailure> {
        Ok(fabric::AnchorResolution::Drifted)
    }

    fn content_status(
        &self,
        _content: &replica::content::ContentRef,
    ) -> Option<runtime::world::ContentStatus> {
        None
    }
}

fn principal() -> PrincipalFacts {
    let device = mechanics::actor::device_from_seed(&[0x71; 32]);
    PrincipalFacts {
        actor: ActorId::from_incept_hash(&"72".repeat(32)),
        station: Key::from_device(&device).expect("Station key"),
        device,
        space: mechanics::ids::SpaceId::from_digest([0x73; 16]),
        authority_frontier: replica::frontier::AuthorityFrontier::from_canonical_bytes(Vec::new()),
    }
}

#[test]
fn semantic_queries_execute_in_the_child_but_read_only_through_the_host() {
    let source = fixture_binary();
    assert!(
        source.is_file(),
        "build semantic fixture at {}",
        source.display()
    );
    let release_root = tempfile::tempdir().expect("release root");
    let name = source.file_name().expect("fixture filename");
    std::fs::copy(&source, release_root.path().join(name)).expect("stage fixture");
    let release = Release::under(
        release_root.path(),
        "com.lait.semantic-fixture",
        "1.0.0",
        Provenance::Sealed([0x74; 32]),
        Path::new(name),
        Vec::new(),
        None::<&Path>,
    )
    .expect("release");
    let instance = Instance::launch(release).expect("runner launches");
    let remote = world_sdk::RemoteWorld::connect(instance).expect("semantic bridge connects");
    assert_eq!(remote.reviewed_implementation(), [0x61; 32]);

    let world = WorldId::parse("com.lait.semantic-fixture").expect("World id");
    let reads = Reads {
        body: BodyKey::new(world, BodyId::from_bytes([0x51; 16])),
    };
    let principal = principal();
    let context = Context::with_reads(&principal, &reads, [0x75; 32]);
    let projection = remote.query(
        &context,
        Query {
            schema: SchemaId::parse("record").expect("schema"),
            schema_version: 1,
            payload: Vec::new(),
            publication: None,
        },
    );
    assert!(
        projection.is_ok(),
        "query crosses the process boundary: {:?}; transport: {:?}",
        projection,
        remote.last_failure()
    );
    let projection = projection.expect("checked above");
    assert_eq!(projection.bytes, b"through-the-host");
}
