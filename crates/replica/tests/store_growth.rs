//! What the store keeps is what it holds — not what it has ever done.
//!
//! The index redesign made a commit cost what it changed. It did not, on its
//! own, make *storage* cost what is live: index nodes handed to the journal as
//! ordinary `added` objects became permanent entries in the required set, so
//! every commit's superseded spine survived every commit after it. Measured
//! before the fix: 80 commits editing one Body left 323 required objects, 237
//! of them index nodes no live root reached.
//!
//! Storage is the axis that has no natural ceiling, so it needs the test.

use std::path::PathBuf;
use std::sync::Arc;

use mechanics::crypto::AuthorizedBodyKey;
use mechanics::ids::SpaceId;
use replica::frontier::AuthorityFrontier;
use replica::{
    BodyBinding, BodyId, BodyKey, BodyOp, CommitAuthorization, CommitContext, EncodingId, Replica,
    SchemaId, SeedSigner, StaticBodyKeys, SupportedSchemas, WorldId, MUTATION_COLLABORATIVE,
};

const WRITER_SEED: [u8; 32] = [62u8; 32];
const EPOCH: [u8; 16] = [3u8; 16];
const EPOCH_KEY: [u8; 32] = [4u8; 32];

fn temp_store(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("lait-store-growth-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}
fn space() -> SpaceId {
    SpaceId::from_digest([31u8; 16])
}
fn keys() -> Arc<StaticBodyKeys> {
    Arc::new(StaticBodyKeys::new(
        AuthorizedBodyKey::for_authorized_epoch(EPOCH, EPOCH_KEY),
    ))
}
fn world() -> WorldId {
    WorldId::parse("com.example.notes").unwrap()
}
fn test_auth() -> replica::StaticAuthorizer {
    replica::StaticAuthorizer {
        world: world(),
        implementation_id: [0u8; 32],
    }
}
fn test_demand() -> Vec<u8> {
    use mechanics::demand::{AuthorizationDemand, PolicyCapability, PolicyResource};
    AuthorizationDemand::require(
        PolicyCapability::new("com.example.notes", "write"),
        PolicyResource::space("com.example.notes"),
    )
    .encode_canonical()
    .expect("canonical demand")
}
fn body(n: u8) -> BodyKey {
    BodyKey::new(world(), BodyId::from_bytes([n; 16]))
}
fn collab_binding() -> BodyBinding {
    BodyBinding {
        schema: SchemaId::parse("note").unwrap(),
        schema_version: 1,
        encoding: EncodingId::parse("collab").unwrap(),
        mutation_model: MUTATION_COLLABORATIVE,
    }
}
fn supported() -> SupportedSchemas {
    let mut s = SupportedSchemas::new();
    s.declare(
        world(),
        SchemaId::parse("note").unwrap(),
        1,
        EncodingId::parse("collab").unwrap(),
        MUTATION_COLLABORATIVE,
    );
    s
}
fn device() -> mechanics::ids::DeviceId {
    mechanics::crypto::device_from_seed(&WRITER_SEED)
}

fn edit(r: &mut Replica, n: u16, key: &BodyKey) {
    let space = space();
    let signer = SeedSigner(&WRITER_SEED);
    let ctx = CommitContext {
        space: &space,
        signer: &signer,
        authority_frontier: AuthorityFrontier::from_canonical_bytes(vec![9]),
    };
    let mut request = [0u8; 16];
    request[..2].copy_from_slice(&n.to_be_bytes());
    r.commit_action(
        &ctx,
        &CommitAuthorization {
            actor: "actor",
            parent_manifest_root: [0u8; 32],
            demand: test_demand(),
            intent_digest: [7u8; 32],
            authorizer: &test_auth(),
        },
        &world(),
        &device(),
        &request,
        &[7u8; 32],
        b"effect".to_vec(),
        vec![],
        "op",
        &[(
            key.clone(),
            BodyOp::TextSplice {
                path: "body".into(),
                index: 0,
                delete: 0,
                insert: "x".into(),
            },
        )],
        &[(key.clone(), collab_binding())],
        &[],
    )
    .expect("commit");
}

fn object_count(dir: &PathBuf) -> usize {
    std::fs::read_dir(dir.join("objects")).unwrap().count()
}

#[test]
fn editing_one_body_does_not_grow_the_required_set() {
    // The required set is the promise the store can never withdraw, so it is
    // the number that must track live state. It needs no sweep to be correct:
    // an index node is kept by reachability, so superseding one removes it
    // from the set the moment the new root lands.
    let dir = temp_store("required");
    let mut r = Replica::open_journaled(&dir, keys()).unwrap();
    r.set_supported(supported());
    for n in 0..20u16 {
        edit(&mut r, n, &body(1));
    }
    let after_twenty = r.required_object_count().expect("durable");
    for n in 20..80u16 {
        edit(&mut r, n, &body(1));
    }
    let after_eighty = r.required_object_count().expect("durable");

    // One required object per commit is the signed Body transaction, and that
    // is the authenticated chain rather than overhead — it is what a peer
    // validating a historical parent has to be able to read. Four per commit is
    // the index spine, and the spine is not history.
    let per_commit = (after_eighty as f64 - after_twenty as f64) / 60.0;
    assert!(
        per_commit < 1.5,
        "the required set grows {per_commit:.2} per commit \
         ({after_twenty} after 20, {after_eighty} after 80)"
    );

    // A reopen must agree. A restart that reclaims something is a session that
    // was holding it for no reason.
    drop(r);
    let reopened = Replica::open_journaled(&dir, keys()).unwrap();
    assert_eq!(
        reopened.required_object_count().expect("durable"),
        after_eighty
    );
}

#[test]
fn collecting_returns_the_store_to_live_state() {
    // Between sweeps a session accumulates the spines its commits superseded;
    // what matters is that collecting reclaims them, and that a restart then
    // finds nothing left to do.
    let dir = temp_store("collect");
    let mut r = Replica::open_journaled(&dir, keys()).unwrap();
    r.set_supported(supported());
    for n in 0..20u16 {
        edit(&mut r, n, &body(1));
    }
    r.collect_unreachable_objects().expect("collect");
    let after_twenty = object_count(&dir);

    for n in 20..80u16 {
        edit(&mut r, n, &body(1));
    }
    r.collect_unreachable_objects().expect("collect");
    let after_eighty = object_count(&dir);

    // Some growth is legitimate: the Body's own collaborative history
    // accumulates until a checkpoint reclaims it. What must not survive is the
    // index spine, at three to four nodes per commit.
    let per_commit = (after_eighty as f64 - after_twenty as f64) / 60.0;
    assert!(
        per_commit < 2.5,
        "storage grows {per_commit:.2} objects per commit          ({after_twenty} after 20, {after_eighty} after 80)"
    );

    drop(r);
    let reopened = Replica::open_journaled(&dir, keys()).unwrap();
    assert_eq!(
        object_count(&dir),
        after_eighty,
        "an in-session collect leaves nothing for the restart to find"
    );
    drop(reopened);
}
