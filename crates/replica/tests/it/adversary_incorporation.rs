//! Adversarial probes against the incorporation changes on `spike/astryx`.
//!
//! Each test names the change it attacks and states the invariant it breaks.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;

use mechanics::authorization::AuthorizedBodyKey;
use mechanics::ids::SpaceId;
use replica::body::{BodyBinding, Op, QuotaConfig, StaticBodyKeys, SupportedSchemas};
use replica::body::{BodyId, BodyKey, EncodingId, SchemaId, WorldId};
use replica::body::{MUTATION_ATOMIC, MUTATION_COLLABORATIVE};
use replica::content::{
    ContentDescriptor, ContentRef, CHUNK_PLAINTEXT_LEN, CONTENT_FORMAT_VERSION,
};
use replica::convergence::{AuthorityBatchReceipt, AuthorityIncorporator, StagedContactMaterial};
use replica::frontier::AuthorityFrontier;
use replica::transaction::{CommitAuthorization, CommitContext, SeedSigner};
use replica::Replica;

const SEED_A: [u8; 32] = [81u8; 32];
const SEED_B: [u8; 32] = [82u8; 32];
const SEED_C: [u8; 32] = [83u8; 32];
const EPOCH: [u8; 16] = [21u8; 16];
const EPOCH_KEY: [u8; 32] = [22u8; 32];

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn temp_store(tag: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("lait-adv-{tag}-{}-{n}", std::process::id()));
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

fn authority_frontier() -> AuthorityFrontier {
    AuthorityFrontier::from_canonical_bytes(vec![17])
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

/// Every device is an authorized signer, and every verification is counted.
#[derive(Default)]
struct CountingAuthority {
    verifications: AtomicUsize,
    /// Transaction ids this source refuses (a stand-in for "the history that
    /// would answer this is not available here").
    refused: std::sync::Mutex<Vec<[u8; 32]>>,
}

impl CountingAuthority {
    fn take(&self) -> usize {
        self.verifications.swap(0, Ordering::SeqCst)
    }
    fn refuse(&self, tx_id: [u8; 32]) {
        self.refused.lock().unwrap().push(tx_id);
    }
}

impl replica::transaction::AuthoritySource for CountingAuthority {
    fn signer_authorized(&self, _signer: &[u8; 32], _f: &AuthorityFrontier) -> bool {
        true
    }
    fn verify_transaction(
        &self,
        tx: &replica::transaction::Transaction,
    ) -> Result<(), replica::transaction::Refusal> {
        self.verifications.fetch_add(1, Ordering::SeqCst);
        if self.refused.lock().unwrap().contains(&tx.id()) {
            return Err(replica::transaction::Refusal::Unauthorized(
                "history unavailable here".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Default)]
struct AcceptingIncorporator;
impl AuthorityIncorporator for AcceptingIncorporator {
    fn incorporate_authority(
        &mut self,
        records: &[Vec<u8>],
    ) -> Result<AuthorityBatchReceipt, replica::convergence::Failure> {
        Ok(AuthorityBatchReceipt {
            space: space(),
            prior_frontier: AuthorityFrontier::from_canonical_bytes(vec![]),
            resulting_frontier: authority_frontier(),
            batch_digest: *blake3::hash(&records.concat()).as_bytes(),
        })
    }
}

fn ctx_for(seed: &'static [u8; 32]) -> (SpaceId, SeedSigner<'static>) {
    (space(), SeedSigner(seed))
}

fn keyed_replica() -> Replica {
    let mut r = Replica::loro().with_keys(keys());
    r.set_supported(supported());
    r
}

fn commit_register(
    r: &mut Replica,
    seed: &'static [u8; 32],
    request: [u8; 16],
    path: &str,
    value: &str,
) {
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
    .expect("commit");
}

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

/// Validate then incorporate, reporting how many `verify_transaction` calls
/// the INCORPORATION phase alone made (validation's own pass is subtracted).
fn pull_counted(
    into: &mut Replica,
    into_seed: &'static [u8; 32],
    staged: &StagedContactMaterial,
    authority: &CountingAuthority,
) -> (
    Result<replica::convergence::ConvergenceOutcome, replica::transaction::commit::Failure>,
    usize,
) {
    let (space, signer) = ctx_for(into_seed);
    let ctx = CommitContext {
        space: &space,
        signer: &signer,
        authority_frontier: authority_frontier(),
    };
    let mut incorporator = AcceptingIncorporator;
    let bundle = match into.validate_contact(staged, authority, &mut incorporator) {
        Ok(b) => b,
        Err(e) => return (Err(e), authority.take()),
    };
    authority.take();
    let outcome = into.incorporate_bundle(&ctx, bundle, authority);
    let counted = authority.take();
    (outcome, counted)
}

fn register_of(r: &Replica, path: &str) -> Option<String> {
    r.read_collaborative(&shared_body()).ok().and_then(|v| {
        v.registers
            .get(path)
            .map(|b| String::from_utf8_lossy(b).into_owned())
    })
}

// ---------------------------------------------------------------------------
// CHANGE B (b) — the quota projection double-counts every replayed transaction
// record, so a replica sitting at exactly its own resulting usage is refused an
// upgrade that adds nothing at all.
// ---------------------------------------------------------------------------

/// Build B holding `shared_body` opaquely with two retained heads (A's and
/// C's), plus the two source replicas.
fn opaque_two_head_holder() -> (Replica, Replica, Replica, CountingAuthority) {
    let authority = CountingAuthority::default();
    let mut a = keyed_replica();
    let mut c = keyed_replica();
    commit_register(&mut a, &SEED_A, [61u8; 16], "froma", "alpha");
    commit_register(&mut c, &SEED_C, [62u8; 16], "fromc", "gamma");

    // B has the key epoch but not the schema: both heads land opaque.
    let mut b = Replica::loro().with_keys(keys());
    let (out, _) = pull_counted(&mut b, &SEED_B, &stage(&a, &SEED_A), &authority);
    out.unwrap();
    let (out, _) = pull_counted(&mut b, &SEED_B, &stage(&c, &SEED_C), &authority);
    out.unwrap();
    assert_eq!(stage(&b, &SEED_B).bodies.len(), 2, "two heads retained");
    (a, b, c, authority)
}

#[test]
fn opaque_upgrade_reuses_retained_bytes_without_quota_charge() {
    // 1. The control. With no quota pressure, the upgrade lands and the
    //    material ledger is EXACTLY what it already was: the same two heads,
    //    the same two envelopes, the same two transaction records.
    let (a, mut b, c, authority) = opaque_two_head_holder();
    let (bytes_before, bodies_before) = b.usage();
    b.set_supported(supported());
    let (out, _) = pull_counted(&mut b, &SEED_B, &stage(&c, &SEED_C), &authority);
    out.expect("the upgrade lands with headroom");
    assert_eq!(register_of(&b, "froma").as_deref(), Some("alpha"));
    assert_eq!(register_of(&b, "fromc").as_deref(), Some("gamma"));
    let (bytes_after, bodies_after) = b.usage();
    assert_eq!(
        (bytes_before, bodies_before),
        (bytes_after, bodies_after),
        "an upgrade out of opaque moves no bytes: same heads, same envelopes, \
         same transaction records"
    );
    drop((a, c));

    // 2. The same replica, quota set to EXACTLY the usage it already has (and
    //    will still have after the upgrade). Nothing about the resulting state
    //    exceeds it.
    let (_a2, mut b2, c2, authority2) = opaque_two_head_holder();
    let (bytes, _) = b2.usage();
    b2.set_quota(QuotaConfig {
        max_space_bytes: bytes,
        ..QuotaConfig::default()
    });
    b2.set_supported(supported());
    let (out, _) = pull_counted(&mut b2, &SEED_B, &stage(&c2, &SEED_C), &authority2);
    out.expect("a zero-cost local revalidation fits at the exact current quota");
    assert_eq!(register_of(&b2, "froma").as_deref(), Some("alpha"));
    assert_eq!(register_of(&b2, "fromc").as_deref(), Some("gamma"));
    assert_eq!(b2.usage().0, bytes, "revalidation adds no stored bytes");
}

// ---------------------------------------------------------------------------
// CHANGE B (c) — the replay is unconditional, so incorporating ONE opaque head
// costs a full signature/authority verification for every head already
// retained. Onboarding a Body with N concurrent authors is O(N^2) verifications
// and the N is remote-controlled up to the opaque subquota.
// ---------------------------------------------------------------------------

#[test]
fn change_b_replay_makes_opaque_retention_cost_quadratic_verifications() {
    let authority = CountingAuthority::default();
    // B never gets the schema: no upgrade ever happens. The doc comment claims
    // injecting in this case "is harmless".
    let mut b = Replica::loro().with_keys(keys());

    let mut costs = Vec::new();
    for i in 0..8u8 {
        // Each writer is a fresh replica writing the shared Body from the
        // empty base: one distinct concurrent head per contact.
        let mut w = keyed_replica();
        let mut request = [0u8; 16];
        request[0] = 200 + i;
        commit_register(&mut w, &SEED_A, request, &format!("w{i}"), "v");
        let staged = stage(&w, &SEED_A);
        assert_eq!(staged.bodies.len(), 1, "one head per contact");
        let (out, verifications) = pull_counted(&mut b, &SEED_B, &staged, &authority);
        out.unwrap();
        costs.push(verifications);
    }

    assert_eq!(
        stage(&b, &SEED_B).bodies.len(),
        8,
        "eight opaque heads retained"
    );
    let total: usize = costs.iter().sum();
    assert_eq!(
        costs,
        vec![1, 1, 1, 1, 1, 1, 1, 1],
        "one incoming head must cost one verification; instead the cost grows \
         with what is already retained: {costs:?} ({total} verifications for 8 \
         heads, and the retained set is bounded only by the opaque subquota)"
    );
}

// ---------------------------------------------------------------------------
// CHANGE B (a) — a refusal on a REPLAYED (old) transaction fails the whole
// bundle, permanently, and takes unrelated material with it. `verify_authorized`
// itself is historical, but the refusal it can return is documented as
// retryable ("missing history"); replay converts that into a hard, repeating
// `Illegitimate(Signature)` for every future bundle touching the key.
// ---------------------------------------------------------------------------

#[test]
fn change_b_a_refusal_on_replayed_material_permanently_wedges_the_key() {
    let authority = CountingAuthority::default();
    let mut a = keyed_replica();
    let mut c = keyed_replica();
    commit_register(&mut a, &SEED_A, [71u8; 16], "froma", "alpha");
    commit_register(&mut c, &SEED_C, [72u8; 16], "fromc", "gamma");

    let mut b = Replica::loro().with_keys(keys());
    let a_staged = stage(&a, &SEED_A);
    let a_tx = a_staged.bodies[0].0;
    let (out, _) = pull_counted(&mut b, &SEED_B, &a_staged, &authority);
    out.unwrap();
    let (out, _) = pull_counted(&mut b, &SEED_B, &stage(&c, &SEED_C), &authority);
    out.unwrap();

    // The authority can no longer answer for A's (old) transaction. Nothing
    // about C's material changed, and C's bundle does not contain A's
    // transaction at all.
    authority.refuse(a_tx);
    let c_staged = stage(&c, &SEED_C);
    assert!(
        !c_staged.bodies.iter().any(|(tx, _, _)| *tx == a_tx),
        "C's bundle carries only C's own transaction"
    );

    let (out, _) = pull_counted(&mut b, &SEED_B, &c_staged, &authority);
    assert!(
        out.is_ok(),
        "a bundle that does not name the unverifiable transaction must not be \
         refused because of it, but replay injected it into phase 1: {out:?}"
    );

    // And it never recovers: the replay is unconditional, so every future
    // contact for this Body re-injects the same unverifiable unit.
    for _ in 0..3 {
        let (out, _) = pull_counted(&mut b, &SEED_B, &stage(&c, &SEED_C), &authority);
        assert!(out.is_ok(), "permanently wedged: {out:?}");
    }
}

// ---------------------------------------------------------------------------
// CHANGE A — one bundle carrying an INTERPRETED head and an OPAQUE head for the
// same, locally-unknown Body.
//
// This is NOT a regression from CHANGE A: the pre-change code is nondeterministic
// here too. It is the defect the change walks past. Whichever branch classifies
// FIRST writes the overlay the other reads, and unit order is transaction-id
// order — a digest over a randomly minted chain seed. Post-change the two
// landing states are:
//
//   opaque first  -> interpreted head fast-forwards and REPLACES: record is
//                    interpreted, one head, the opaque head's material deleted.
//   interp. first -> opaque head APPENDS (this is what `current_chain.is_some()`
//                    newly enables for a locally-unknown key) and the fold's
//                    `existing.interpreted = change.record.interpreted` flips
//                    the whole record to opaque: it declares no heads, exports
//                    two, and the engine is holding interpreted state the record
//                    says was never interpreted (so a reopen drops it).
//
// Pre-change the second state was (declares 0, exports 1) — the same
// contradiction with one author's work additionally destroyed.
// ---------------------------------------------------------------------------

const EPOCH_2: [u8; 16] = [31u8; 16];
const EPOCH_2_KEY: [u8; 32] = [32u8; 32];

/// A key source that seals under one epoch and can open several.
struct EpochSet {
    seal: AuthorizedBodyKey,
    open: Vec<AuthorizedBodyKey>,
}

impl replica::body::BodyKeySource for EpochSet {
    fn sealing_key(&self) -> Option<AuthorizedBodyKey> {
        Some(self.seal.clone())
    }
    fn opening_key(&self, epoch: &[u8; 16]) -> Option<AuthorizedBodyKey> {
        self.open.iter().find(|k| k.epoch_id() == epoch).cloned()
    }
}

fn epoch_set(seal: AuthorizedBodyKey, open: Vec<AuthorizedBodyKey>) -> Arc<EpochSet> {
    Arc::new(EpochSet { seal, open })
}

fn epoch_1() -> AuthorizedBodyKey {
    AuthorizedBodyKey::for_authorized_epoch(EPOCH, EPOCH_KEY)
}
fn epoch_2() -> AuthorizedBodyKey {
    AuthorizedBodyKey::for_authorized_epoch(EPOCH_2, EPOCH_2_KEY)
}

#[test]
fn change_a_a_mixed_bundle_lands_in_one_of_two_states_at_random() {
    // One observation of the scenario: A seals under epoch 1, C under epoch 2,
    // A merges C and serves BOTH heads in one bundle. B supports the schema and
    // holds only epoch 1, and has never seen this Body — so one head of one
    // bundle interprets and the other is retained opaque.
    fn observe(n: u8) -> (usize, usize, bool, bool) {
        let authority = CountingAuthority::default();
        let mut a = Replica::loro().with_keys(epoch_set(epoch_1(), vec![epoch_1(), epoch_2()]));
        a.set_supported(supported());
        let mut c = Replica::loro().with_keys(epoch_set(epoch_2(), vec![epoch_1(), epoch_2()]));
        c.set_supported(supported());
        let mut r1 = [0u8; 16];
        r1[0] = n;
        let mut r2 = [0u8; 16];
        r2[0] = n.wrapping_add(1);
        r2[1] = 1;
        commit_register(&mut a, &SEED_A, r1, "froma", "alpha");
        commit_register(&mut c, &SEED_C, r2, "fromc", "gamma");
        pull_counted(&mut a, &SEED_A, &stage(&c, &SEED_C), &authority)
            .0
            .unwrap();
        let served = stage(&a, &SEED_A);
        assert_eq!(served.bodies.len(), 2, "A serves both heads in one bundle");

        let mut b = Replica::loro().with_keys(epoch_set(epoch_1(), vec![epoch_1()]));
        b.set_supported(supported());
        let outcome = pull_counted(&mut b, &SEED_B, &served, &authority)
            .0
            .unwrap();
        assert_eq!(
            (outcome.accepted, outcome.unsupported_retained),
            (1, 1),
            "one head interpreted, one retained opaque"
        );
        (
            b.head_commitments().len(),
            stage(&b, &SEED_B).bodies.len(),
            register_of(&b, "froma").is_some(),
            register_of(&b, "fromc").is_some(),
        )
    }

    // The two units are grouped by transaction id (`export_material_excluding`
    // keys a BTreeMap by it), and a transaction id is a digest over a randomly
    // minted chain seed. Which branch runs FIRST is therefore random — and the
    // overlay `merge_append` reads is written by whichever went first.
    let mut seen: std::collections::BTreeSet<(usize, usize, bool, bool)> =
        std::collections::BTreeSet::new();
    for n in 0..24u8 {
        seen.insert(observe(n.wrapping_mul(7).wrapping_add(1)));
    }
    assert_eq!(
        seen.len(),
        1,
        "the same bundle must land the same state every time; instead \
         (declared_heads, exported_heads, froma_readable, fromc_readable) \
         came back as {seen:?}"
    );
}

// ---------------------------------------------------------------------------
// CHANGE C (b) — declaration-only adoption republishes a new signed manifest
// root at an UNCHANGED replica frontier. `ManifestRoot::coordinate()` is
// `(signer, replica_frontier)` and `ManifestBook` treats two different roots at
// one coordinate as equivocation. Every other content path calls
// `advance_published` for exactly this reason.
// ---------------------------------------------------------------------------

mod declaration {
    use super::*;

    const WRITER_SEED: [u8; 32] = [83u8; 32];
    const PEER_SEED: [u8; 32] = [84u8; 32];

    fn body(n: u8) -> BodyKey {
        BodyKey::new(world(), BodyId::from_bytes([n; 16]))
    }
    fn device() -> mechanics::ids::DeviceId {
        mechanics::actor::device_from_seed(&WRITER_SEED)
    }
    fn blob_binding() -> BodyBinding {
        BodyBinding {
            schema: SchemaId::parse("blob").unwrap(),
            schema_version: 1,
            encoding: EncodingId::parse("bytes").unwrap(),
            mutation_model: MUTATION_ATOMIC,
        }
    }
    fn blob_supported() -> SupportedSchemas {
        let mut s = SupportedSchemas::new();
        s.declare(
            world(),
            SchemaId::parse("blob").unwrap(),
            1,
            EncodingId::parse("bytes").unwrap(),
            MUTATION_ATOMIC,
        );
        s
    }

    pub(super) fn open(tag: &str) -> Replica {
        let mut r = Replica::open(temp_store(tag).join("store"), keys()).unwrap();
        r.set_supported(blob_supported());
        r
    }

    fn commit_blob(r: &mut Replica, seq: u8, key: &BodyKey, bytes: &[u8]) {
        let space = space();
        let signer = SeedSigner(&WRITER_SEED);
        let ctx = CommitContext {
            space: &space,
            signer: &signer,
            authority_frontier: authority_frontier(),
        };
        let mut request = [0u8; 16];
        request[0] = seq;
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
            &device(),
            &request,
            &[7u8; 32],
            Vec::new(),
            Vec::new(),
            "blob",
            &[(
                key.clone(),
                Op::ReplaceAtomic {
                    value: bytes.to_vec(),
                },
            )],
            &[(key.clone(), blob_binding())],
            &[],
        )
        .expect("commit");
    }

    fn descriptor_for(nonce: u8, len: u64) -> ContentDescriptor {
        let d = ContentDescriptor {
            format_version: CONTENT_FORMAT_VERSION,
            space: space().as_str().to_string(),
            content_nonce: [nonce; 16],
            plaintext_len: len,
            chunk_plaintext_len: CHUNK_PLAINTEXT_LEN,
            chunk_count: u32::try_from(len.div_ceil(u64::from(CHUNK_PLAINTEXT_LEN)).max(1))
                .unwrap(),
            ciphertext_merkle_root: [nonce; 32],
            epoch: EPOCH,
        };
        d.validate().expect("a well-formed descriptor");
        d
    }

    fn attach(r: &mut Replica, key: &BodyKey, descriptor: &ContentDescriptor) -> ContentRef {
        let space = space();
        let signer = SeedSigner(&WRITER_SEED);
        let ctx = CommitContext {
            space: &space,
            signer: &signer,
            authority_frontier: authority_frontier(),
        };
        let reference = r
            .commit_content(&ctx, std::slice::from_ref(descriptor))
            .expect("commit descriptor")[0];
        let mut declarations = BTreeMap::new();
        declarations.insert(key.clone(), vec![reference]);
        r.declare_content(&ctx, declarations).expect("declare");
        reference
    }

    fn stage_blob(r: &Replica) -> StagedContactMaterial {
        let space = space();
        let signer = SeedSigner(&WRITER_SEED);
        let ctx = CommitContext {
            space: &space,
            signer: &signer,
            authority_frontier: authority_frontier(),
        };
        let material = r.export_material().unwrap();
        let (root, nodes) = r.export_manifest(&ctx).unwrap();
        let mut authority_records = vec![b"mechanics-authority-record".to_vec()];
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
            manifest_nodes: nodes,
            bodies,
        }
    }

    /// The receiver signs its OWN published root with its own device.
    fn contact(receiver: &mut Replica, staged: &StagedContactMaterial) {
        let space = space();
        let signer = SeedSigner(&PEER_SEED);
        let ctx = CommitContext {
            space: &space,
            signer: &signer,
            authority_frontier: authority_frontier(),
        };
        let authority = CountingAuthority::default();
        let mut incorporator = AcceptingIncorporator;
        let bundle = receiver
            .validate_contact(staged, &authority, &mut incorporator)
            .expect("the advertisement validates");
        receiver
            .incorporate_bundle(&ctx, bundle, &authority)
            .expect("incorporates");
    }

    #[test]
    fn change_c_declaration_only_adoption_equivocates_at_its_own_coordinate() {
        use replica::manifest::{ManifestBook, RootObservation};

        let mut author = open("equiv-author");
        let mut peer = open("equiv-peer");

        commit_blob(&mut author, 1, &body(1), b"an issue");
        contact(&mut peer, &stage_blob(&author));

        // The root the peer publishes right now. A peer that pulled from it at
        // this instant has this root at this coordinate.
        let before = peer.published_manifest_root().expect("a published root");
        let authority = CountingAuthority::default();
        let mut book = ManifestBook::new();
        assert_eq!(
            book.observe(&before.clone().verify_authorized(&authority).unwrap())
                .unwrap(),
            RootObservation::Accepted
        );

        // The author attaches content; nothing about the Body's head set moves.
        attach(&mut author, &body(1), &descriptor_for(1, 4096));
        contact(&mut peer, &stage_blob(&author));

        let after = peer.published_manifest_root().expect("a published root");
        assert_ne!(
            before.root_hash(),
            after.root_hash(),
            "the adoption did publish a different catalog"
        );
        assert_ne!(
            before.coordinate(),
            after.coordinate(),
            "a different signed catalog needs a fresh coordinate"
        );

        // Which is precisely what the book calls equivocation. `commit_content`
        // and `declare_content` both call `advance_published` to avoid this;
        // `adopt_declarations_only` passes `self.frontier` straight through.
        let observation = book.observe(&after.verify_authorized(&authority).unwrap());
        assert_eq!(
            observation,
            Ok(RootObservation::Accepted),
            "an honest replica flagged itself as an equivocator by adopting a \
             declaration"
        );
    }

    /// CHANGE C (c) — the adopted declaration is the SENDER's, mapped onto the
    /// RECEIVER's own record. When the receiver is ahead, it republishes a
    /// declaration for a version of the Body it does not hold.
    #[test]
    fn change_c_adopts_a_stale_peer_declaration_over_a_newer_local_body() {
        let mut author = open("stale-author");
        let mut peer = open("stale-peer");

        // Both hold body(1) at v1, with content X declared.
        commit_blob(&mut author, 1, &body(1), b"v1");
        contact(&mut peer, &stage_blob(&author));
        let old_content = attach(&mut author, &body(1), &descriptor_for(1, 4096));
        contact(&mut peer, &stage_blob(&author));
        assert_eq!(peer.declared_content(&body(1)), vec![old_content]);

        // The author moves on: v2 of the Body, and the attachment is replaced.
        // The peer syncs and is now strictly ahead of the snapshot below.
        commit_blob(&mut author, 2, &body(1), b"v2");
        let new_content = attach(&mut author, &body(1), &descriptor_for(2, 4096));
        contact(&mut peer, &stage_blob(&author));
        assert_eq!(peer.declared_content(&body(1)), vec![new_content]);

        // A third replica still serving the OLD snapshot contacts the peer.
        // Its Body head is an ancestor, so nothing is incorporated — but its
        // declaration is adopted anyway, over the newer one.
        let mut stale = open("stale-relay");
        commit_blob(&mut stale, 1, &body(1), b"v1");
        let stale_content = attach(&mut stale, &body(1), &descriptor_for(1, 4096));
        assert_eq!(stale_content, old_content);
        contact(&mut peer, &stage_blob(&stale));

        assert_eq!(
            peer.declared_content(&body(1)),
            vec![new_content],
            "a stale peer's declaration must not overwrite the newer one this \
             replica already holds"
        );
    }
}

// ---------------------------------------------------------------------------
// CHANGE B (d)/(e) — two replicas that received the SAME opaque material in
// opposite orders, then upgraded from the same bundle. Replay order follows
// retention order, so if anything published depended on it these two would
// diverge permanently.
// ---------------------------------------------------------------------------

fn durable_opaque(tag: &str) -> (Replica, PathBuf) {
    let root = temp_store(tag);
    let r = Replica::open(&root, keys()).unwrap();
    (r, root)
}

#[test]
fn change_b_replay_order_does_not_move_the_published_catalog() {
    let authority = CountingAuthority::default();
    let mut a = keyed_replica();
    let mut c = keyed_replica();
    commit_register(&mut a, &SEED_A, [101u8; 16], "froma", "alpha");
    commit_register(&mut c, &SEED_C, [102u8; 16], "fromc", "gamma");
    pull_counted(&mut a, &SEED_A, &stage(&c, &SEED_C), &authority)
        .0
        .unwrap();
    let upgrade = stage(&a, &SEED_A);
    let from_a = stage(&a, &SEED_A);
    let from_c = stage(&c, &SEED_C);

    // b1 retains A's head then C's; b2 retains C's then A's. Neither has the
    // schema yet, so both are pure opaque retention.
    let (mut b1, root1) = durable_opaque("order-1");
    let (mut b2, root2) = durable_opaque("order-2");
    for (r, order) in [(&mut b1, [&from_a, &from_c]), (&mut b2, [&from_c, &from_a])] {
        for staged in order {
            pull_counted(r, &SEED_B, staged, &authority).0.unwrap();
        }
    }
    assert_eq!(
        b1.published_root(),
        b2.published_root(),
        "the opaque catalog already agrees: `ManifestEntry::declaring` sorts \
         and dedups the head list"
    );

    // The schema arrives on both, and both upgrade from the identical bundle.
    b1.set_supported(supported());
    b2.set_supported(supported());
    pull_counted(&mut b1, &SEED_B, &upgrade, &authority)
        .0
        .unwrap();
    pull_counted(&mut b2, &SEED_B, &upgrade, &authority)
        .0
        .unwrap();

    assert_eq!(register_of(&b1, "froma"), register_of(&b2, "froma"));
    assert_eq!(register_of(&b1, "fromc"), register_of(&b2, "fromc"));
    let mut h1 = b1.head_commitments();
    let mut h2 = b2.head_commitments();
    h1.sort();
    h2.sort();
    assert_eq!(h1, h2, "the advertised head sets must agree");
    assert_eq!(
        b1.published_root(),
        b2.published_root(),
        "the published catalog must not depend on the order material arrived in"
    );
    let _ = std::fs::remove_dir_all(&root1);
    let _ = std::fs::remove_dir_all(&root2);
}
