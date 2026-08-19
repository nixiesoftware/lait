//! Multi-writer convergence — the constituent-head model.
//!
//! A Body written concurrently by two authorized devices has no single
//! envelope containing its merged state: each replica's index carries the SET
//! of author-signed heads whose engine merge is the state, the manifest
//! advertises one entry per head, transfers move every head, restart re-merges
//! every head, and a later local commit collapses the set to one (its sealed
//! envelope is the full merged snapshot). Only original author-signed material
//! ever crosses a wire — a replica never re-signs what it merged, so a
//! read-only member can relay merged state it could never have authored.
//!
//! This pins the defect the 32-actor reference corpus surfaced: a replica that
//! incorporated concurrent catalog writes re-served a stale single-author
//! envelope (or nothing), so peers either rejected the root whole or silently
//! never converged, and a restart lost the merge.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use mechanics::authorization::AuthorizedBodyKey;
use mechanics::ids::SpaceId;
use replica::body::{BodyBinding, Op, StaticBodyKeys, SupportedSchemas, MUTATION_COLLABORATIVE};
use replica::body::{BodyId, BodyKey, SchemaId, WorldId};
use replica::convergence::{AuthorityBatchReceipt, AuthorityIncorporator, StagedContactMaterial};
use replica::frontier::AuthorityFrontier;
use replica::transaction::{ActionOutcome, CommitAuthorization, CommitContext, SeedSigner};
use replica::Replica;

const SEED_A: [u8; 32] = [81u8; 32];
const SEED_B: [u8; 32] = [82u8; 32];
const SEED_C: [u8; 32] = [83u8; 32];
const EPOCH: [u8; 16] = [21u8; 16];
const EPOCH_KEY: [u8; 32] = [22u8; 32];

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn temp_store(tag: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("lait-heads-{tag}-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn space() -> SpaceId {
    SpaceId::from_digest([43u8; 16])
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

fn shared_body() -> BodyKey {
    BodyKey::new(world(), BodyId::from_bytes([9u8; 16]))
}

fn binding() -> BodyBinding {
    BodyBinding {
        schema: SchemaId::parse("note").unwrap(),
        schema_version: 1,
        encoding: replica::body::EncodingId::parse("collab").unwrap(),
        mutation_model: MUTATION_COLLABORATIVE,
    }
}

fn supported() -> SupportedSchemas {
    let mut s = SupportedSchemas::new();
    s.declare(
        world(),
        SchemaId::parse("note").unwrap(),
        1,
        replica::body::EncodingId::parse("collab").unwrap(),
        MUTATION_COLLABORATIVE,
    );
    s
}

/// Any of the three test devices is an authorized signer.
struct AnyWriter;
impl replica::transaction::AuthoritySource for AnyWriter {
    fn signer_authorized(&self, signer: &[u8; 32], _f: &AuthorityFrontier) -> bool {
        [SEED_A, SEED_B, SEED_C]
            .iter()
            .any(|seed| mechanics::actor::device_from_seed(seed).key_bytes() == Some(*signer))
    }
}

#[derive(Default)]
struct AcceptingIncorporator;
impl AuthorityIncorporator for AcceptingIncorporator {
    fn incorporate_authority(
        &mut self,
        records: &[Vec<u8>],
    ) -> Result<AuthorityBatchReceipt, replica::convergence::Failure> {
        Ok(replica::convergence::AuthorityBatchReceipt {
            space: space(),
            prior_frontier: AuthorityFrontier::from_canonical_bytes(vec![]),
            resulting_frontier: authority_frontier(),
            batch_digest: *blake3::hash(&records.concat()).as_bytes(),
        })
    }
}

fn authority_frontier() -> AuthorityFrontier {
    AuthorityFrontier::from_canonical_bytes(vec![17])
}

fn ctx_for(seed: &'static [u8; 32]) -> (SpaceId, SeedSigner<'static>) {
    (space(), SeedSigner(seed))
}

fn keyed_replica() -> Replica {
    let mut r = Replica::loro().with_keys(keys());
    r.set_supported(supported());
    r
}

fn durable_replica(tag: &str) -> (Replica, PathBuf) {
    let root = temp_store(tag);
    let mut r = Replica::open(&root, keys()).unwrap();
    r.set_supported(supported());
    (r, root)
}

fn reopen(root: &PathBuf) -> Replica {
    let mut r = Replica::open(root, keys()).unwrap();
    r.set_supported(supported());
    r
}

/// Commit `RegisterSet(path=value)` on the shared body, signed by `seed`.
fn commit_register(
    r: &mut Replica,
    seed: &'static [u8; 32],
    request: [u8; 16],
    path: &str,
    value: &str,
) -> Result<ActionOutcome, replica::transaction::commit::Failure> {
    let (space, signer) = ctx_for(seed);
    let ctx = CommitContext {
        space: &space,
        signer: &signer,
        authority_frontier: authority_frontier(),
    };
    r.commit_action(
        &ctx,
        &CommitAuthorization {
            actor: "act_0000000000000000000000000000000000000000000000000000000000000000",
            parent_manifest_root: [0u8; 32],
            demand: test_demand(),
            intent_digest: [7u8; 32],
            authorizer: &test_auth(),
        },
        &world(),
        &mechanics::actor::device_from_seed(seed),
        &request,
        &[7u8; 32],
        vec![],
        vec![],
        "note",
        &[(
            shared_body(),
            Op::RegisterSet {
                path: path.into(),
                value: value.as_bytes().to_vec(),
            },
        )],
        &[(shared_body(), binding())],
        &[],
    )
}

/// Stage a replica's export as untrusted Contact material, omitting heads the
/// peer declared it holds (the O(changed) delta path; empty = full transfer).
fn stage_excluding(
    r: &Replica,
    seed: &'static [u8; 32],
    held: &std::collections::BTreeSet<(BodyKey, [u8; 32])>,
) -> StagedContactMaterial {
    let (space, signer) = ctx_for(seed);
    let ctx = CommitContext {
        space: &space,
        signer: &signer,
        authority_frontier: authority_frontier(),
    };
    let material = r.export_material_excluding(held).unwrap();
    let (root, pages) = r.export_manifest(&ctx).unwrap();
    let mut authority_records = Vec::new();
    let mut bodies = Vec::new();
    for (tx, payloads) in &material {
        authority_records.push(tx.encode());
        for (key, envelope) in payloads {
            bodies.push((tx.id(), key.clone(), envelope.clone()));
        }
    }
    StagedContactMaterial {
        authority_records,
        manifest_root_bytes: root,
        manifest_nodes: pages,
        bodies,
    }
}

/// Stage a replica's full export as untrusted Contact material.
fn stage(r: &Replica, seed: &'static [u8; 32]) -> StagedContactMaterial {
    let (space, signer) = ctx_for(seed);
    let ctx = CommitContext {
        space: &space,
        signer: &signer,
        authority_frontier: authority_frontier(),
    };
    let material = r.export_material().unwrap();
    let (root, pages) = r.export_manifest(&ctx).unwrap();
    let mut authority_records = Vec::new();
    let mut bodies = Vec::new();
    for (tx, payloads) in &material {
        authority_records.push(tx.encode());
        for (key, envelope) in payloads {
            bodies.push((tx.id(), key.clone(), envelope.clone()));
        }
    }
    StagedContactMaterial {
        authority_records,
        manifest_root_bytes: root,
        manifest_nodes: pages,
        bodies,
    }
}

/// Pull `from`'s full staging into `into` (validate + incorporate).
fn pull(
    into: &mut Replica,
    into_seed: &'static [u8; 32],
    from: &Replica,
    from_seed: &'static [u8; 32],
) -> replica::convergence::ConvergenceOutcome {
    let staged = stage(from, from_seed);
    let (space, signer) = ctx_for(into_seed);
    let ctx = CommitContext {
        space: &space,
        signer: &signer,
        authority_frontier: authority_frontier(),
    };
    let mut incorporator = AcceptingIncorporator;
    let bundle = into
        .validate_contact(&staged, &AnyWriter, &mut incorporator)
        .unwrap();
    into.incorporate_bundle(&ctx, bundle, &AnyWriter).unwrap()
}

fn register_of(r: &Replica, path: &str) -> Option<String> {
    r.read_collaborative(&shared_body()).ok().and_then(|v| {
        v.registers
            .get(path)
            .map(|b| String::from_utf8_lossy(b).into_owned())
    })
}

#[test]
fn concurrent_writers_converge_and_reserve_the_union() {
    // A and B write concurrently (each from the empty base).
    let mut a = keyed_replica();
    let mut b = keyed_replica();
    commit_register(&mut a, &SEED_A, [1u8; 16], "froma", "alpha").unwrap();
    commit_register(&mut b, &SEED_B, [2u8; 16], "fromb", "beta").unwrap();

    // A pulls B: the union is readable, and BOTH heads are advertised.
    let outcome = pull(&mut a, &SEED_A, &b, &SEED_B);
    assert!(outcome.advanced(), "concurrent head incorporates");
    assert_eq!(register_of(&a, "froma").as_deref(), Some("alpha"));
    assert_eq!(register_of(&a, "fromb").as_deref(), Some("beta"));

    // B pulls A: symmetric union.
    pull(&mut b, &SEED_B, &a, &SEED_A);
    assert_eq!(register_of(&b, "froma").as_deref(), Some("alpha"));
    assert_eq!(register_of(&b, "fromb").as_deref(), Some("beta"));

    // TRANSITIVE relay: C (a third party that authored nothing) pulls ONLY A
    // and still receives both authors' heads — merged state relays through an
    // intermediary without that intermediary re-signing anything.
    let mut c = keyed_replica();
    pull(&mut c, &SEED_C, &a, &SEED_A);
    assert_eq!(register_of(&c, "froma").as_deref(), Some("alpha"));
    assert_eq!(register_of(&c, "fromb").as_deref(), Some("beta"));

    // Idempotent re-pull: nothing new, nothing rejected.
    let again = pull(&mut c, &SEED_C, &a, &SEED_A);
    assert!(!again.advanced(), "re-pulling known heads changes nothing");
    assert_eq!(again.rejected, 0);
}

#[test]
fn merged_heads_survive_a_durable_restart() {
    let (mut a, root) = durable_replica("restart");
    let mut b = keyed_replica();
    commit_register(&mut a, &SEED_A, [3u8; 16], "froma", "alpha").unwrap();
    commit_register(&mut b, &SEED_B, [4u8; 16], "fromb", "beta").unwrap();
    pull(&mut a, &SEED_A, &b, &SEED_B);
    assert_eq!(register_of(&a, "fromb").as_deref(), Some("beta"));
    drop(a);

    // Reopen: the union is rebuilt from the persisted head set, and the
    // reopened replica can still serve BOTH heads to a third party.
    let a = reopen(&root);
    assert_eq!(register_of(&a, "froma").as_deref(), Some("alpha"));
    assert_eq!(register_of(&a, "fromb").as_deref(), Some("beta"));
    let mut c = keyed_replica();
    pull(&mut c, &SEED_C, &a, &SEED_A);
    assert_eq!(register_of(&c, "froma").as_deref(), Some("alpha"));
    assert_eq!(register_of(&c, "fromb").as_deref(), Some("beta"));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_local_commit_collapses_the_head_set() {
    let mut a = keyed_replica();
    let mut b = keyed_replica();
    commit_register(&mut a, &SEED_A, [5u8; 16], "froma", "alpha").unwrap();
    commit_register(&mut b, &SEED_B, [6u8; 16], "fromb", "beta").unwrap();
    pull(&mut a, &SEED_A, &b, &SEED_B);

    // A's next local commit seals the FULL merged snapshot: one head again.
    commit_register(&mut a, &SEED_A, [7u8; 16], "sealed", "yes").unwrap();

    // A fresh C pulling only A gets everything from that single-author head.
    let mut c = keyed_replica();
    pull(&mut c, &SEED_C, &a, &SEED_A);
    assert_eq!(register_of(&c, "froma").as_deref(), Some("alpha"));
    assert_eq!(register_of(&c, "fromb").as_deref(), Some("beta"));
    assert_eq!(register_of(&c, "sealed").as_deref(), Some("yes"));
}

/// Validate + incorporate an explicit staging into `into` (the delta-path
/// twin of `pull`, which always stages the full export).
fn pull_staged(
    into: &mut Replica,
    into_seed: &'static [u8; 32],
    staged: &StagedContactMaterial,
) -> Result<replica::convergence::ConvergenceOutcome, replica::transaction::commit::Failure> {
    let (space, signer) = ctx_for(into_seed);
    let ctx = CommitContext {
        space: &space,
        signer: &signer,
        authority_frontier: authority_frontier(),
    };
    let mut incorporator = AcceptingIncorporator;
    let bundle = into.validate_contact(staged, &AnyWriter, &mut incorporator)?;
    into.incorporate_bundle(&ctx, bundle, &AnyWriter)
}

fn holdings(r: &Replica) -> std::collections::BTreeSet<(BodyKey, [u8; 32])> {
    r.head_commitments().into_iter().collect()
}

fn artifact_pack_count(pack: &[u8]) -> usize {
    assert_eq!(pack.first(), Some(&1), "artifact pack version");
    usize::from(u16::from_be_bytes([pack[1], pack[2]]))
}

fn second_body() -> BodyKey {
    BodyKey::new(world(), BodyId::from_bytes([10u8; 16]))
}

#[test]
fn a_delta_pull_ships_only_missing_heads_and_converges() {
    // B fully syncs A, then A advances (a new write on the shared body AND a
    // brand-new body). B's next pull declares its holdings: the staging must
    // carry ONLY the new material, and adoption must land the identical state
    // a full transfer would.
    let mut a = keyed_replica();
    let mut b = keyed_replica();
    commit_register(&mut a, &SEED_A, [11u8; 16], "froma", "alpha").unwrap();
    pull(&mut b, &SEED_B, &a, &SEED_A);
    assert_eq!(register_of(&b, "froma").as_deref(), Some("alpha"));

    let (space, signer) = ctx_for(&SEED_A);
    let ctx = CommitContext {
        space: &space,
        signer: &signer,
        authority_frontier: authority_frontier(),
    };
    a.commit_action(
        &ctx,
        &CommitAuthorization {
            actor: "act_0000000000000000000000000000000000000000000000000000000000000000",
            parent_manifest_root: [0u8; 32],
            demand: test_demand(),
            intent_digest: [7u8; 32],
            authorizer: &test_auth(),
        },
        &world(),
        &mechanics::actor::device_from_seed(&SEED_A),
        &[13u8; 16],
        &[7u8; 32],
        vec![],
        vec![],
        "note",
        &[(
            second_body(),
            Op::RegisterSet {
                path: "fresh".into(),
                value: b"new-body".to_vec(),
            },
        )],
        &[(second_body(), binding())],
        &[],
    )
    .unwrap();

    let full = stage(&a, &SEED_A);
    let delta = stage_excluding(&a, &SEED_A, &holdings(&b));
    assert!(
        delta.bodies.len() < full.bodies.len(),
        "delta ({}) must ship fewer heads than full ({})",
        delta.bodies.len(),
        full.bodies.len()
    );
    // Exactly ONE new head: the fresh body. The shared body's head commitment
    // is unchanged and B declared it, so it ships nothing.
    assert_eq!(delta.bodies.len(), 1);
    assert_eq!(full.bodies.len(), 2);

    let outcome = pull_staged(&mut b, &SEED_B, &delta).unwrap();
    assert!(outcome.advanced(), "the delta adopts");
    assert_eq!(register_of(&b, "froma").as_deref(), Some("alpha"));
    let fresh = b
        .read_collaborative(&second_body())
        .ok()
        .and_then(|v| v.registers.get("fresh").cloned());
    assert_eq!(fresh.as_deref(), Some(b"new-body".as_slice()));

    // Steady state: a re-pull with up-to-date holdings ships NOTHING and
    // adopts nothing new — the O(changed) idle pump.
    let idle = stage_excluding(&a, &SEED_A, &holdings(&b));
    assert!(idle.bodies.is_empty(), "idle delta ships no bodies");
    let outcome = pull_staged(&mut b, &SEED_B, &idle).unwrap();
    assert!(!outcome.advanced(), "an empty delta changes nothing");
}

#[test]
fn a_hot_body_pull_ships_only_the_new_artifact_and_converges() {
    let mut a = keyed_replica();
    let mut b = keyed_replica();
    commit_register(&mut a, &SEED_A, [61u8; 16], "first", "alpha").unwrap();
    pull(&mut b, &SEED_B, &a, &SEED_A);
    let held = holdings(&b);

    commit_register(&mut a, &SEED_A, [62u8; 16], "second", "beta").unwrap();
    let full = stage(&a, &SEED_A);
    let delta = stage_excluding(&a, &SEED_A, &held);

    assert_eq!(full.bodies.len(), 1);
    assert_eq!(delta.bodies.len(), 1);
    let full_count = artifact_pack_count(&full.bodies[0].2);
    let delta_count = artifact_pack_count(&delta.bodies[0].2);
    eprintln!(
        "hot-peer artifact delivery: full={full_count} artifacts/{} bytes, incremental={delta_count} artifact/{} bytes",
        full.bodies[0].2.len(),
        delta.bodies[0].2.len()
    );
    assert!(
        full_count >= 2,
        "the signed closure retains checkpoint/tail history"
    );
    assert_eq!(
        delta_count, 1,
        "a hot peer receives only the new content-addressed artifact"
    );
    assert!(
        delta.bodies[0].2.len() < full.bodies[0].2.len(),
        "the delivery pack must shrink with receiver holdings"
    );

    let outcome = pull_staged(&mut b, &SEED_B, &delta).unwrap();
    assert!(outcome.advanced());
    assert_eq!(register_of(&b, "first").as_deref(), Some("alpha"));
    assert_eq!(register_of(&b, "second").as_deref(), Some("beta"));
    assert_eq!(holdings(&a), holdings(&b));
}

/// An upgrade out of opaque merges every author's retained material.
///
/// Opaque material is retained per author and never merged, so the envelope
/// that triggers an upgrade contains only ITS author's state. The `was_opaque`
/// fast-forward — "the incoming envelope CONTAINS our state, so its head
/// REPLACES the set" — is a false premise exactly here, and it used to replace
/// the head set with one author's work and delete the rest: `raw_material` is
/// dropped the instant a record turns interpreted, and the refcount pass then
/// releases the objects for sweeping. The material was recoverable only if the
/// other author happened to contact this Station again.
#[test]
fn upgrading_an_opaque_body_merges_every_retained_head() {
    let mut a = keyed_replica();
    let mut c = keyed_replica();
    commit_register(&mut a, &SEED_A, [31u8; 16], "froma", "alpha").unwrap();
    commit_register(&mut c, &SEED_C, [32u8; 16], "fromc", "gamma").unwrap();

    // B has the key epoch but not the schema: both authors' material is
    // retained opaquely, byte-identically, as two heads of one Body.
    let mut b = Replica::loro().with_keys(keys());
    pull(&mut b, &SEED_B, &a, &SEED_A);
    pull(&mut b, &SEED_B, &c, &SEED_C);
    assert!(
        b.head_commitments().is_empty(),
        "opaque heads are deliberately not declared"
    );
    assert!(
        register_of(&b, "froma").is_none() && register_of(&b, "fromc").is_none(),
        "an opaque Body reads as absent"
    );
    assert_eq!(stage(&b, &SEED_B).bodies.len(), 2, "two heads retained");

    // The schema arrives. B re-receives A's material — the upgrade path. C's
    // retained head is replayed alongside it and the engine merges both.
    b.set_supported(supported());
    pull(&mut b, &SEED_B, &a, &SEED_A);

    assert_eq!(register_of(&b, "froma").as_deref(), Some("alpha"));
    assert_eq!(
        register_of(&b, "fromc").as_deref(),
        Some("gamma"),
        "C never re-contacted; its material was already here and must survive"
    );
    assert_eq!(
        b.head_commitments().len(),
        2,
        "the upgraded record advertises both heads"
    );

    // Idempotent, and a later Contact with C changes nothing.
    pull(&mut b, &SEED_B, &c, &SEED_C);
    assert_eq!(register_of(&b, "froma").as_deref(), Some("alpha"));
    assert_eq!(register_of(&b, "fromc").as_deref(), Some("gamma"));
    assert_eq!(b.head_commitments().len(), 2);
}

/// The declaration is a SNAPSHOT, and the initiator keeps writing under it.
///
/// `contact_driver::initiate` reads `head_commitments` once (contact_driver.rs:743)
/// and only takes the Replica lock again to validate (contact_driver.rs:583), so
/// every local commit, and every one of the other three Contacts the driver may
/// have in flight (`MAX_CONTACTS_IN_FLIGHT = 4`), lands in between. A local commit
/// collapses a Body's head set to one new head (replica.rs:1942), so the
/// commitment this replica declared is simply gone by the time rule 7 looks for
/// it — while the honest server, believing the declaration, omitted exactly that
/// material (replica.rs:3307) and advertised a manifest that still names it.
///
/// Nobody lied. The whole root is refused anyway, and the refusal takes the
/// unrelated new Body in the same bundle with it.
#[test]
fn an_honest_local_write_during_a_delta_pull_rejects_the_whole_root() {
    let mut a = keyed_replica();
    let mut b = keyed_replica();
    commit_register(&mut a, &SEED_A, [21u8; 16], "froma", "alpha").unwrap();
    pull(&mut b, &SEED_B, &a, &SEED_A);

    // 1. B dials A and declares what it holds — truthfully, right now.
    let declared = holdings(&b);
    assert_eq!(declared.len(), 1);

    // 2. A advances: a brand-new Body, the news this Contact exists to carry.
    let (space, signer) = ctx_for(&SEED_A);
    let ctx = CommitContext {
        space: &space,
        signer: &signer,
        authority_frontier: authority_frontier(),
    };
    a.commit_action(
        &ctx,
        &CommitAuthorization {
            actor: "act_0000000000000000000000000000000000000000000000000000000000000000",
            parent_manifest_root: [0u8; 32],
            demand: test_demand(),
            intent_digest: [7u8; 32],
            authorizer: &test_auth(),
        },
        &world(),
        &mechanics::actor::device_from_seed(&SEED_A),
        &[22u8; 16],
        &[7u8; 32],
        vec![],
        vec![],
        "note",
        &[(
            second_body(),
            Op::RegisterSet {
                path: "fresh".into(),
                value: b"new-body".to_vec(),
            },
        )],
        &[(second_body(), binding())],
        &[],
    )
    .unwrap();

    // 3. The window. B's own user edits the shared Body while the transfer is
    //    on the wire. The head set collapses: the commitment declared in (1) is
    //    no longer anywhere in B's record.
    commit_register(&mut b, &SEED_B, [23u8; 16], "fromb", "beta").unwrap();
    assert!(
        holdings(&b).is_disjoint(&declared),
        "the local commit replaced the head B declared"
    );

    // 4. A serves honestly against the declaration it was given.
    let delta = stage_excluding(&a, &SEED_A, &declared);
    assert_eq!(
        delta.bodies.len(),
        1,
        "only the new Body ships; the shared Body was declared held"
    );

    // 5. Rule 7 (replica.rs:2929) finds the advertised head neither received
    //    nor locally reconstructable, and refuses the root whole.
    let err = pull_staged(&mut b, &SEED_B, &delta).unwrap_err();
    assert_eq!(
        err,
        replica::transaction::commit::Failure::Illegitimate(
            replica::transaction::commit::Invalid::IncompleteMaterial
        ),
    );
    let fresh = b
        .read_collaborative(&second_body())
        .ok()
        .and_then(|v| v.registers.get("fresh").cloned());
    assert!(
        fresh.is_none(),
        "the unrelated new Body is collateral: nothing in the bundle is adopted"
    );

    // The next Contact recovers — B now declares its post-commit head, A no
    // longer omits anything, and the same root adopts.
    let retry = stage_excluding(&a, &SEED_A, &holdings(&b));
    pull_staged(&mut b, &SEED_B, &retry).unwrap();
    let fresh = b
        .read_collaborative(&second_body())
        .ok()
        .and_then(|v| v.registers.get("fresh").cloned());
    assert_eq!(fresh.as_deref(), Some(b"new-body".as_slice()));
}

#[test]
fn a_false_holdings_declaration_starves_only_the_claimant() {
    // B claims to hold A's head without having it. The server honestly omits
    // it; B's own root-completeness validation then rejects the WHOLE root
    // ("neither held nor transferred") and B adopts nothing — a lying (or
    // stale) declaration cannot corrupt state, it can only stall the liar.
    let mut a = keyed_replica();
    let mut b = keyed_replica();
    commit_register(&mut a, &SEED_A, [14u8; 16], "froma", "alpha").unwrap();

    let lie: std::collections::BTreeSet<(BodyKey, [u8; 32])> = holdings(&a).into_iter().collect();
    let starved = stage_excluding(&a, &SEED_A, &lie);
    assert!(starved.bodies.is_empty());
    let err = pull_staged(&mut b, &SEED_B, &starved).unwrap_err();
    assert_eq!(
        err,
        replica::transaction::commit::Failure::Illegitimate(
            replica::transaction::commit::Invalid::IncompleteMaterial
        ),
        "the incomplete root is rejected as one semantic bundle"
    );
    assert!(
        register_of(&b, "froma").is_none(),
        "nothing was partially adopted"
    );

    // A truthful retry (no exclusions) recovers completely.
    pull(&mut b, &SEED_B, &a, &SEED_A);
    assert_eq!(register_of(&b, "froma").as_deref(), Some("alpha"));
}

/// Two opaque heads of one Body, delivered in a single bundle, are BOTH
/// retained.
///
/// The opaque branch used to decide `merge_append` from
/// `self.bodies.contains_key` — the COMMITTED map — while every other
/// classification in the same loop reads the `overlay` that exists precisely so
/// "successive same-Body writes within one bundle classify against the staged
/// (not the committed) state" (replica.rs:2216). So two heads of a Body NEW to
/// this replica both planned a replace, and the fold let the second overwrite
/// the first.
///
/// This is the ordinary onboarding shape: a node without the schema (or without
/// the key epoch) doing its first pull of a Body two people edited concurrently.
/// The damage was not a local read — an opaque Body reads as absent either way —
/// it was that the receiver SERVED the truncation onward as a complete,
/// root-validated Body.
#[test]
fn two_opaque_heads_in_one_bundle_are_both_retained() {
    // A and C write the shared Body concurrently; A merges C, so A holds and
    // advertises BOTH heads.
    let mut a = keyed_replica();
    let mut c = keyed_replica();
    commit_register(&mut a, &SEED_A, [41u8; 16], "froma", "alpha").unwrap();
    commit_register(&mut c, &SEED_C, [42u8; 16], "fromc", "gamma").unwrap();
    pull(&mut a, &SEED_A, &c, &SEED_C);
    let served = stage(&a, &SEED_A);
    assert_eq!(served.bodies.len(), 2, "A serves both heads in one bundle");

    // B has the key epoch but NOT the schema: everything is retained opaquely.
    // One Contact, both heads, no upgrade path involved.
    let mut b = Replica::loro().with_keys(keys());
    let outcome = pull_staged(&mut b, &SEED_B, &served).unwrap();
    assert_eq!(outcome.unsupported_retained, 2);

    // Both survive the fold — what B reports retaining is what B holds.
    assert_eq!(
        stage(&b, &SEED_B).bodies.len(),
        2,
        "a bundle that retained two heads must keep two heads"
    );

    // The consequence that mattered: a peer pulling the merged Body THROUGH B
    // gets both authors' work, not one.
    let reserved = stage(&b, &SEED_B);
    let mut d = keyed_replica();
    pull_staged(&mut d, &SEED_C, &reserved).unwrap();
    assert_eq!(register_of(&d, "froma").as_deref(), Some("alpha"));
    assert_eq!(register_of(&d, "fromc").as_deref(), Some("gamma"));

    // A second Contact is idempotent: both heads are recognised, nothing is
    // retained afresh, and the count does not drift.
    let again = pull(&mut b, &SEED_B, &a, &SEED_A);
    assert_eq!(
        (again.unsupported_retained, again.unchanged),
        (0, 2),
        "a converged opaque holder retains nothing new"
    );
    assert_eq!(stage(&b, &SEED_B).bodies.len(), 2);

    // Delivering the same two heads as two SEPARATE bundles is unchanged.
    let mut a1 = keyed_replica();
    let mut c1 = keyed_replica();
    commit_register(&mut a1, &SEED_A, [43u8; 16], "froma", "alpha").unwrap();
    commit_register(&mut c1, &SEED_C, [44u8; 16], "fromc", "gamma").unwrap();
    let mut b3 = Replica::loro().with_keys(keys());
    pull(&mut b3, &SEED_B, &a1, &SEED_A);
    pull(&mut b3, &SEED_B, &c1, &SEED_C);
    assert_eq!(stage(&b3, &SEED_B).bodies.len(), 2);

    // And the interpreted branch, which always read the overlay, still agrees.
    let mut e = keyed_replica();
    pull_staged(&mut e, &SEED_B, &served).unwrap();
    assert_eq!(register_of(&e, "froma").as_deref(), Some("alpha"));
    assert_eq!(register_of(&e, "fromc").as_deref(), Some("gamma"));
    assert_eq!(stage(&e, &SEED_B).bodies.len(), 2);
}
