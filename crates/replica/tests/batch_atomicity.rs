//! Batch atomicity through the real durable Replica path.
//!
//! Fabric's own tests cover the in-memory half. What this file adds is the
//! part that cannot be faked: that a failed batch leaves nothing behind in the
//! store either, that a *later* successful commit does not seal the damage in,
//! and that a reopen agrees with what the live Replica said.
//!
//! The regression these guard is one bug with three faces. The bounded
//! rollback saved a *position* inside each touched Body's document; a
//! `Tombstone` destroys the document a position indexes, so the position had
//! nothing to restore and the Body was simply gone.

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
    let dir =
        std::env::temp_dir().join(format!("lait-batch-atomicity-{tag}-{}", std::process::id()));
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

fn try_commit(
    r: &mut Replica,
    n: u8,
    bindings: &[(BodyKey, BodyBinding)],
    ops: &[(BodyKey, BodyOp)],
) -> Result<(), replica::ReplicaCommitError> {
    let space = space();
    let signer = SeedSigner(&WRITER_SEED);
    let ctx = CommitContext {
        space: &space,
        signer: &signer,
        authority_frontier: AuthorityFrontier::from_canonical_bytes(vec![9]),
    };
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
        &[n; 16],
        &[7u8; 32],
        b"effect".to_vec(),
        vec![],
        "op",
        ops,
        bindings,
        &[],
    )
    .map(|_| ())
}

fn text_of(r: &Replica, key: &BodyKey) -> String {
    match r.read_collaborative(key) {
        Ok(v) => v.texts.get("body").cloned().unwrap_or_default(),
        Err(e) => format!("<err {e:?}>"),
    }
}

#[test]
fn a_failed_batch_that_tombstoned_a_body_leaves_it_intact_on_disk() {
    let dir = temp_store("tombstone");
    let mut r = Replica::open_journaled(&dir, keys()).unwrap();
    r.set_supported(supported());

    try_commit(
        &mut r,
        1,
        &[(body(1), collab_binding())],
        &[(
            body(1),
            BodyOp::TextSplice {
                path: "body".into(),
                index: 0,
                delete: 0,
                insert: "important content".into(),
            },
        )],
    )
    .expect("seed commit");
    let sealed = r.frontier();

    // A batch that tombstones the seeded Body and then fails on another one.
    try_commit(
        &mut r,
        2,
        &[(body(1), collab_binding()), (body(2), collab_binding())],
        &[
            (body(1), BodyOp::Tombstone),
            (
                body(2),
                BodyOp::TextSplice {
                    path: "body".into(),
                    index: 999_999,
                    delete: 0,
                    insert: "boom".into(),
                },
            ),
        ],
    )
    .expect_err("batch must fail");

    assert_eq!(r.body_keys().len(), 1);
    assert_eq!(text_of(&r, &body(1)), "important content");
    assert!(r.binding(&body(1)).is_some());
    assert_eq!(r.frontier(), sealed, "a failed batch advances no frontier");

    // The dangerous sequence: an ordinary successful edit right after, which
    // is what would seal a corrupted Body into the store for good.
    try_commit(
        &mut r,
        3,
        &[(body(1), collab_binding())],
        &[(
            body(1),
            BodyOp::TextSplice {
                path: "body".into(),
                index: 0,
                delete: 0,
                insert: "still here: ".into(),
            },
        )],
    )
    .expect("follow-on commit");
    assert_eq!(text_of(&r, &body(1)), "still here: important content");

    drop(r);
    let reopened = Replica::open_journaled(&dir, keys()).unwrap();
    assert_eq!(reopened.body_keys().len(), 1);
    assert_eq!(
        text_of(&reopened, &body(1)),
        "still here: important content",
        "the store must agree with what the live Replica reported"
    );
}

#[test]
fn a_failed_batch_survives_a_reopen_with_no_follow_on_edit() {
    // The same failure with nothing to mask it: the only commit after the
    // failed batch touches an unrelated Body, so what reopens is whatever the
    // rollback actually left.
    let dir = temp_store("tombstone-reopen");
    let mut r = Replica::open_journaled(&dir, keys()).unwrap();
    r.set_supported(supported());
    try_commit(
        &mut r,
        1,
        &[(body(1), collab_binding())],
        &[(
            body(1),
            BodyOp::TextSplice {
                path: "body".into(),
                index: 0,
                delete: 0,
                insert: "important content".into(),
            },
        )],
    )
    .expect("seed commit");
    try_commit(
        &mut r,
        2,
        &[(body(1), collab_binding()), (body(2), collab_binding())],
        &[
            (body(1), BodyOp::Tombstone),
            (
                body(2),
                BodyOp::TextSplice {
                    path: "body".into(),
                    index: 999_999,
                    delete: 0,
                    insert: "boom".into(),
                },
            ),
        ],
    )
    .expect_err("batch must fail");
    try_commit(
        &mut r,
        3,
        &[(body(3), collab_binding())],
        &[(
            body(3),
            BodyOp::TextSplice {
                path: "body".into(),
                index: 0,
                delete: 0,
                insert: "unrelated".into(),
            },
        )],
    )
    .expect("unrelated commit");
    assert_eq!(text_of(&r, &body(1)), "important content");

    drop(r);
    let reopened = Replica::open_journaled(&dir, keys()).unwrap();
    assert_eq!(reopened.body_keys().len(), 2);
    assert_eq!(text_of(&reopened, &body(1)), "important content");
    assert_eq!(text_of(&reopened, &body(3)), "unrelated");
}
