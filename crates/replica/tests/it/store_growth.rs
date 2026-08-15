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

use std::path::{Path, PathBuf};
use std::sync::Arc;

use mechanics::authorization::AuthorizedBodyKey;
use mechanics::ids::SpaceId;
use replica::body::{BodyBinding, Op, StaticBodyKeys, SupportedSchemas, MUTATION_COLLABORATIVE};
use replica::body::{BodyId, BodyKey, EncodingId, SchemaId, WorldId};
use replica::frontier::AuthorityFrontier;
use replica::transaction::{CommitAuthorization, CommitContext, SeedSigner};
use replica::Replica;

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
fn test_auth() -> replica::transaction::StaticAuthorizer {
    replica::transaction::StaticAuthorizer {
        world: world(),
        implementation_id: [0u8; 32],
    }
}
fn test_demand() -> Vec<u8> {
    use mechanics::authorization::{AuthorizationDemand, PolicyCapability, Resource};
    AuthorizationDemand::require(
        PolicyCapability::new("com.example.notes", "write"),
        Resource::root("com.example.notes"),
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
    mechanics::actor::device_from_seed(&WRITER_SEED)
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
            Op::TextSplice {
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

fn object_count(dir: &Path) -> usize {
    std::fs::read_dir(dir.join("objects")).unwrap().count()
}

#[test]
fn editing_one_body_grows_only_by_the_history_it_retains() {
    // The required set is the promise the store can never withdraw, so it is
    // the number that must track live state. It needs no sweep to be correct:
    // an index node is kept by reachability, so superseding one removes it
    // from the set the moment the new root lands.
    let dir = temp_store("required");
    let mut r = Replica::open(&dir, keys()).unwrap();
    r.set_supported(supported());
    for n in 0..20u16 {
        edit(&mut r, n, &body(1));
    }
    let after_twenty = r.required_object_count().expect("durable");
    for n in 20..80u16 {
        edit(&mut r, n, &body(1));
    }
    let after_eighty = r.required_object_count().expect("durable");

    // One required object per commit is the signed Body transaction, and one
    // is the changed-Body generation delta that makes an exact historical read
    // survive restart and sweep. Both are retained history. The index spine is
    // not history and must not add another three to four objects per commit.
    let per_commit = (after_eighty as f64 - after_twenty as f64) / 60.0;
    assert!(
        per_commit < 2.5,
        "the required set grows {per_commit:.2} per commit \
         ({after_twenty} after 20, {after_eighty} after 80)"
    );

    // A reopen must agree. A restart that reclaims something is a session that
    // was holding it for no reason.
    drop(r);
    let reopened = Replica::open(&dir, keys()).unwrap();
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
    let mut r = Replica::open(&dir, keys()).unwrap();
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
    let reopened = Replica::open(&dir, keys()).unwrap();
    assert_eq!(
        object_count(&dir),
        after_eighty,
        "an in-session collect leaves nothing for the restart to find"
    );
    drop(reopened);
}

// ---- What the store can say about itself ----
//
// A storage surface asks three things: how much, how many, and when was any of
// it last checked. The first two are reads over state the store already has;
// the third has to be recorded by whatever does the checking. All three have
// the same rule — a figure nobody measured is reported absent, never as a zero
// that makes the surface look populated.

/// The store knows how many Bodies it holds without being asked to list them,
/// and a restart holds what it held.
#[test]
fn the_store_counts_the_bodies_it_holds_without_enumerating_them() {
    let dir = temp_store("body-count");
    let mut r = Replica::open(&dir, keys()).unwrap();
    r.set_supported(supported());
    assert_eq!(r.body_count(), 0, "a store with no commits holds no Bodies");

    for n in 1..=3u8 {
        edit(&mut r, u16::from(n), &body(n));
    }
    assert_eq!(r.body_count(), 3);
    assert_eq!(
        r.body_count(),
        u64::try_from(r.body_keys().len()).unwrap(),
        "the cheap count and the enumeration must not be able to disagree"
    );

    drop(r);
    let reopened = Replica::open(&dir, keys()).unwrap();
    assert_eq!(reopened.body_count(), 3, "a restart holds what it held");
}

/// A Replica that was never opened from a store has never been verified, and
/// says so rather than reporting the epoch.
#[test]
fn a_replica_that_never_touched_a_store_reports_no_verification_at_all() {
    // The distinction the whole `Option` exists for: this is not "verified at
    // time zero", it is "nobody has ever checked". A surface that rendered the
    // former would be stating an observation that never happened.
    assert_eq!(Replica::loro().verified_at_ms(), None);
}

/// Opening a store from disk *is* the verification pass, so every open stamps
/// the moment it completed — including the first, over an empty store.
#[test]
fn opening_a_store_records_the_verification_that_opening_performed() {
    let dir = temp_store("verified-at");

    // The first open has no commit point to read, but the journal still
    // validated the required set it found. A store that has been checked and
    // holds nothing is not the same as one nobody has checked.
    let formed = {
        let _clock = mechanics::wallclock::Frozen::at_millis(1_700_000_000_000);
        let mut r = Replica::open(&dir, keys()).unwrap();
        r.set_supported(supported());
        edit(&mut r, 0, &body(1));
        r.verified_at_ms()
    };
    assert_eq!(formed, Some(1_700_000_000_000));

    // A later open re-reads every required object, re-derives its content
    // address and re-verifies every signed transaction, so the stamp moves to
    // when *that* pass ran. It is not the store's birthday.
    let reopened = {
        let _clock = mechanics::wallclock::Frozen::at_millis(1_700_000_060_000);
        Replica::open(&dir, keys()).unwrap().verified_at_ms()
    };
    assert_eq!(reopened, Some(1_700_000_060_000));
}
