//! C1.3 / G4 — the canonical Body/store representation, through the public API.
//!
//! Proves the durable Replica addresses **canonical objects** — signed
//! transaction records, protected Fabric artifact closures, idempotency
//! receipts, and Manifest root/index — rather than one opaque engine snapshot;
//! that no plaintext Body material is at rest; that receipts and replay survive
//! a cold reopen; and that exact incorporation (signed transaction +
//! descriptor-bound artifacts) converges, refuses illegitimate material,
//! retains unknown material opaquely and byte-identically, and resolves
//! concurrent atomic writes deterministically.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::body::{
    immutable_body_key, BodyBinding, Op, StaticBodyKeys, SupportedSchemas, MUTATION_ATOMIC,
    MUTATION_COLLABORATIVE, MUTATION_IMMUTABLE_ATOMIC,
};
use crate::body::{BodyId, BodyKey, EncodingId, SchemaId, WorldId};
use crate::frontier::AuthorityFrontier;
use crate::transaction::{
    ActionOutcome, CommitAuthorization, CommitContext, PreparedActionOutcome, SeedSigner,
    SignRequest, Transaction, TransactionAuthorizer,
};
use crate::{BodyImageFailure, BodyImagePresence, Replica};
use mechanics::authorization::AuthorizedBodyKey;
use mechanics::ids::SpaceId;

const WRITER_SEED: [u8; 32] = [61u8; 32];
const EPOCH: [u8; 16] = [3u8; 16];
const EPOCH_KEY: [u8; 32] = [4u8; 32];
const TEST_ACTOR: &str = "act_0000000000000000000000000000000000000000000000000000000000000000";

static COUNTER: AtomicU64 = AtomicU64::new(0);

#[cfg(windows)]
fn resident_bytes() -> usize {
    #[repr(C)]
    struct Counters {
        cb: u32,
        page_fault_count: u32,
        peak_working_set_size: usize,
        working_set_size: usize,
        quota_peak_paged_pool_usage: usize,
        quota_paged_pool_usage: usize,
        quota_peak_non_paged_pool_usage: usize,
        quota_non_paged_pool_usage: usize,
        pagefile_usage: usize,
        peak_pagefile_usage: usize,
    }
    unsafe extern "system" {
        fn GetCurrentProcess() -> *mut core::ffi::c_void;
        fn K32GetProcessMemoryInfo(
            process: *mut core::ffi::c_void,
            counters: *mut Counters,
            size: u32,
        ) -> i32;
    }
    let mut counters = Counters {
        cb: u32::try_from(std::mem::size_of::<Counters>()).expect("counter size"),
        page_fault_count: 0,
        peak_working_set_size: 0,
        working_set_size: 0,
        quota_peak_paged_pool_usage: 0,
        quota_paged_pool_usage: 0,
        quota_peak_non_paged_pool_usage: 0,
        quota_non_paged_pool_usage: 0,
        pagefile_usage: 0,
        peak_pagefile_usage: 0,
    };
    // SAFETY: this is the Windows PROCESS_MEMORY_COUNTERS layout and the
    // pseudo-handle is valid for the duration of the call.
    let ok =
        unsafe { K32GetProcessMemoryInfo(GetCurrentProcess(), &raw mut counters, counters.cb) };
    if ok == 0 {
        0
    } else {
        counters.working_set_size
    }
}

#[cfg(not(windows))]
fn resident_bytes() -> usize {
    0
}

fn temp_store(tag: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("lait-canonical-{tag}-{}-{n}", std::process::id()));
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

struct RotatingBodyKeys {
    second: AtomicBool,
}

impl RotatingBodyKeys {
    fn new() -> Self {
        Self {
            second: AtomicBool::new(false),
        }
    }

    fn rotate(&self) {
        self.second.store(true, Ordering::SeqCst);
    }
}

impl crate::body::BodyKeySource for RotatingBodyKeys {
    fn sealing_key(&self) -> Option<AuthorizedBodyKey> {
        if self.second.load(Ordering::SeqCst) {
            Some(AuthorizedBodyKey::for_authorized_epoch(
                [8u8; 16], [18u8; 32],
            ))
        } else {
            Some(AuthorizedBodyKey::for_authorized_epoch(EPOCH, EPOCH_KEY))
        }
    }

    fn opening_key(&self, epoch: &[u8; 16]) -> Option<AuthorizedBodyKey> {
        if epoch == &EPOCH {
            Some(AuthorizedBodyKey::for_authorized_epoch(EPOCH, EPOCH_KEY))
        } else if epoch == &[8u8; 16] {
            Some(AuthorizedBodyKey::for_authorized_epoch(
                [8u8; 16], [18u8; 32],
            ))
        } else {
            None
        }
    }
}

struct NoBodyKeys;

impl crate::body::BodyKeySource for NoBodyKeys {
    fn sealing_key(&self) -> Option<AuthorizedBodyKey> {
        None
    }

    fn opening_key(&self, _epoch: &[u8; 16]) -> Option<AuthorizedBodyKey> {
        None
    }
}

struct RevocableBodyKeys {
    available: AtomicBool,
}

impl RevocableBodyKeys {
    fn new() -> Self {
        Self {
            available: AtomicBool::new(true),
        }
    }

    fn revoke(&self) {
        self.available.store(false, Ordering::SeqCst);
    }
}

impl crate::body::BodyKeySource for RevocableBodyKeys {
    fn sealing_key(&self) -> Option<AuthorizedBodyKey> {
        self.available
            .load(Ordering::SeqCst)
            .then(|| AuthorizedBodyKey::for_authorized_epoch(EPOCH, EPOCH_KEY))
    }

    fn opening_key(&self, epoch: &[u8; 16]) -> Option<AuthorizedBodyKey> {
        (self.available.load(Ordering::SeqCst) && epoch == &EPOCH)
            .then(|| AuthorizedBodyKey::for_authorized_epoch(EPOCH, EPOCH_KEY))
    }
}

fn world() -> WorldId {
    WorldId::parse("com.example.notes").unwrap()
}

/// A test authorizer + commit-authorization helper (the machinery each commit
/// needs now that authorization is bound into the signed transaction).
fn test_auth() -> crate::transaction::StaticAuthorizer {
    crate::transaction::StaticAuthorizer {
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

fn atomic_binding() -> BodyBinding {
    BodyBinding {
        schema: SchemaId::parse("blob").unwrap(),
        schema_version: 1,
        encoding: EncodingId::parse("bytes").unwrap(),
        mutation_model: MUTATION_ATOMIC,
    }
}

fn immutable_binding() -> BodyBinding {
    BodyBinding {
        schema: SchemaId::parse("immutable").unwrap(),
        schema_version: 1,
        encoding: EncodingId::parse("bytes").unwrap(),
        mutation_model: MUTATION_IMMUTABLE_ATOMIC,
    }
}

fn immutable_body(value: &[u8]) -> BodyKey {
    let binding = immutable_binding();
    immutable_body_key(
        &world(),
        &binding.schema,
        binding.schema_version,
        &binding.encoding,
        value,
    )
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
    s.declare(
        world(),
        SchemaId::parse("blob").unwrap(),
        1,
        EncodingId::parse("bytes").unwrap(),
        MUTATION_ATOMIC,
    );
    s.declare(
        world(),
        SchemaId::parse("immutable").unwrap(),
        1,
        EncodingId::parse("bytes").unwrap(),
        MUTATION_IMMUTABLE_ATOMIC,
    );
    s
}

fn device() -> mechanics::ids::DeviceId {
    mechanics::actor::device_from_seed(&WRITER_SEED)
}

fn authority_frontier() -> AuthorityFrontier {
    AuthorityFrontier::from_canonical_bytes(vec![9])
}

/// A mechanics view authorizing exactly the writer device.
struct WriterAuthorized;
impl crate::transaction::AuthoritySource for WriterAuthorized {
    fn signer_authorized(&self, signer: &[u8; 32], _f: &AuthorityFrontier) -> bool {
        *signer == device().key_bytes().unwrap()
    }
}

fn commit(
    r: &mut Replica,
    request: [u8; 16],
    label: &str,
    ops: &[(BodyKey, Op)],
    bindings: &[(BodyKey, BodyBinding)],
) -> Result<ActionOutcome, crate::transaction::commit::Failure> {
    let space = space();
    let signer = SeedSigner(&WRITER_SEED);
    let ctx = CommitContext {
        space: &space,
        signer: &signer,
        authority_frontier: authority_frontier(),
    };
    r.commit_action(
        &ctx,
        &CommitAuthorization {
            actor: TEST_ACTOR,
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
        label,
        ops,
        bindings,
        &[],
    )
}

fn resign_with_descriptor(
    original: &Transaction,
    descriptor: crate::transaction::Descriptor,
) -> Transaction {
    let space = space();
    let signer = SeedSigner(&WRITER_SEED);
    let authorizer = test_auth();
    Transaction::sign_with(
        SignRequest {
            space: &space,
            parent_manifest_root: original.core.parent_manifest_root,
            replica_frontier: original.core.replica_frontier,
            authority_frontier: original.core.authority_frontier.clone(),
            actor: &original.core.actor,
            operation: original.core.operation,
            intent_digest: original.core.intent_digest,
            operations_digest: original.core.operations_digest,
            demand: original.core.demand.clone(),
            descriptors: vec![descriptor],
        },
        &signer,
        |core| authorizer.authorize(core),
    )
    .unwrap()
}

fn open(dir: &PathBuf) -> Replica {
    let mut r = Replica::open(dir, keys()).unwrap();
    r.set_supported(supported());
    r
}

fn journal_object_path(dir: &PathBuf, hash: &[u8; 32]) -> PathBuf {
    let name = hash
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    dir.join("objects").join(name)
}

fn counter_ops(key: &BodyKey, delta: i64) -> Vec<(BodyKey, Op)> {
    vec![(
        key.clone(),
        Op::CounterAdd {
            path: "votes".into(),
            delta,
        },
    )]
}

#[test]
fn a_durable_commit_survives_cold_reopen_with_receipts_and_replay() {
    let dir = temp_store("reopen");
    let mut r = open(&dir);
    let request = [21u8; 16];
    let first = commit(
        &mut r,
        request,
        "bump",
        &counter_ops(&body(1), 5),
        &[(body(1), collab_binding())],
    )
    .unwrap();
    let ActionOutcome::Committed(receipt) = &first else {
        panic!("fresh commit");
    };
    let frontier = r.frontier();
    let receipt_hash = journal::object_content_hash(&receipt.encode());
    drop(r); // crash: no dormancy

    // Cold reopen: state, frontier, AND the idempotency receipt all recovered.
    journal::watch_recovery_object_reads(receipt_hash);
    let mut r = open(&dir);
    assert_eq!(journal::watched_recovery_object_reads(), 0);
    assert_eq!(r.frontier(), frontier);
    assert_eq!(r.read_collaborative(&body(1)).unwrap().counters["votes"], 5);

    // Identical replay AFTER restart returns the original receipt and does
    // not reapply the non-idempotent CounterAdd.
    let replay = commit(
        &mut r,
        request,
        "bump",
        &counter_ops(&body(1), 5),
        &[(body(1), collab_binding())],
    )
    .unwrap();
    assert_eq!(replay, ActionOutcome::Replayed(receipt.clone()));
    assert_eq!(r.read_collaborative(&body(1)).unwrap().counters["votes"], 5);
    assert_eq!(r.frontier(), frontier);
    let footprint = r.receipt_reader().unwrap().footprint();
    assert_eq!(footprint.count, 1);
    assert_eq!(footprint.material_bytes, receipt.encode().len() as u64);
    assert!(footprint.cache_upper_bound <= 16 * 1024 * 1024);
    assert!(footprint.cold_lookup_transient_upper_bound >= 4 * 1024 * 1024);
    std::fs::remove_file(journal_object_path(&dir, &receipt_hash)).unwrap();
    assert_eq!(
        r.receipt_reader()
            .unwrap()
            .lookup_action(&space(), &world(), &device(), &request, &[7u8; 32])
            .unwrap(),
        Some(receipt.clone()),
        "a verified hot replay performs no receipt-object I/O"
    );

    // Conflicting reuse after restart is still refused.
    let space = space();
    let signer = SeedSigner(&WRITER_SEED);
    let ctx = CommitContext {
        space: &space,
        signer: &signer,
        authority_frontier: authority_frontier(),
    };
    let err = r
        .commit_action(
            &ctx,
            &CommitAuthorization {
                actor: TEST_ACTOR,
                parent_manifest_root: [0u8; 32],
                demand: test_demand(),
                intent_digest: [7u8; 32],
                authorizer: &test_auth(),
            },
            &world(),
            &device(),
            &request,
            &[8u8; 32], // different payload hash
            vec![],
            vec![],
            "bump",
            &counter_ops(&body(1), 5),
            &[(body(1), collab_binding())],
            &[],
        )
        .unwrap_err();
    assert_eq!(err, crate::transaction::commit::Failure::RequestIdConflict);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn corrupt_deferred_receipt_does_not_block_open_but_cold_lookup_fails_closed() {
    let dir = temp_store("corrupt-deferred-receipt");
    let mut replica = open(&dir);
    let request = [24u8; 16];
    let ActionOutcome::Committed(receipt) = commit(
        &mut replica,
        request,
        "receipt",
        &counter_ops(&body(2), 1),
        &[(body(2), collab_binding())],
    )
    .unwrap() else {
        panic!("fresh request");
    };
    let receipt_hash = journal::object_content_hash(&receipt.encode());
    drop(replica);

    let path = journal_object_path(&dir, &receipt_hash);
    let mut corrupt = std::fs::read(&path).unwrap();
    corrupt[0] ^= 0x80;
    std::fs::write(&path, corrupt).unwrap();

    // Receipt payloads are deferred: unrelated state opens normally. The
    // authenticated index/object check fails exactly when this scope is read.
    let reopened = open(&dir);
    assert_eq!(
        reopened.read_collaborative(&body(2)).unwrap().counters["votes"],
        1
    );
    assert_eq!(
        reopened
            .receipt_reader()
            .unwrap()
            .lookup_action(&space(), &world(), &device(), &request, &[7u8; 32])
            .unwrap_err(),
        crate::transaction::commit::Failure::Integrity(crate::replica::Defect::Store(
            journal::Defect::CorruptObject
        ))
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn receipt_only_commit_is_quota_admitted_before_durable_write() {
    let dir = temp_store("receipt-only-quota");
    let mut replica = open(&dir);
    let before = replica.usage();
    let prior_quota = *replica.quota();
    replica.set_quota(crate::replica::QuotaConfig {
        max_space_bytes: before.0,
        ..prior_quota
    });
    assert_eq!(
        commit(&mut replica, [25u8; 16], "no-op", &[], &[]).unwrap_err(),
        crate::transaction::commit::Failure::QuotaExceeded
    );
    assert_eq!(replica.usage(), before);
    drop(replica);

    let reopened = open(&dir);
    assert_eq!(reopened.usage(), before);
    assert_eq!(reopened.receipt_reader().unwrap().footprint().count, 0);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn receipt_absence_proof_skips_cold_recheck_and_rejects_stale_or_wrong_scope() {
    let dir = temp_store("receipt-absence-proof");
    let mut replica = open(&dir);
    let space = space();
    let signer = SeedSigner(&WRITER_SEED);
    let ctx = CommitContext {
        space: &space,
        signer: &signer,
        authority_frontier: authority_frontier(),
    };
    let authorizer = test_auth();
    let auth = CommitAuthorization {
        actor: TEST_ACTOR,
        parent_manifest_root: replica.manifest_root(),
        demand: test_demand(),
        intent_digest: [26u8; 32],
        authorizer: &authorizer,
    };
    let request = [26u8; 16];
    let proof = match replica
        .receipt_reader()
        .unwrap()
        .check_action(&space, &world(), &device(), &request, &[7u8; 32])
        .unwrap()
    {
        crate::ReceiptCheck::Absent(proof) => proof,
        crate::ReceiptCheck::Replayed(_) => panic!("fresh scope"),
    };
    assert!(matches!(
        replica.prepare_action_checked(
            &ctx,
            &auth,
            crate::receipt::Interpretation::UNSPECIFIED,
            &world(),
            &device(),
            &[27u8; 16],
            &[7u8; 32],
            Vec::new(),
            Vec::new(),
            "wrong-scope",
            &[],
            &[],
            &[],
            proof,
        ),
        Err(crate::transaction::commit::Failure::ReceiptCheckStale)
    ));

    let proof = match replica
        .receipt_reader()
        .unwrap()
        .check_action(&space, &world(), &device(), &request, &[7u8; 32])
        .unwrap()
    {
        crate::ReceiptCheck::Absent(proof) => proof,
        crate::ReceiptCheck::Replayed(_) => panic!("fresh scope"),
    };
    let winner = commit(&mut replica, request, "winner", &[], &[]).unwrap();
    assert!(matches!(
        replica.prepare_action_checked(
            &ctx,
            &auth,
            crate::receipt::Interpretation::UNSPECIFIED,
            &world(),
            &device(),
            &request,
            &[7u8; 32],
            Vec::new(),
            Vec::new(),
            "stale",
            &[],
            &[],
            &[],
            proof,
        ),
        Err(crate::transaction::commit::Failure::ReceiptCheckStale)
    ));
    let reader = replica.receipt_reader().unwrap();
    let ActionOutcome::Committed(winner_receipt) = winner else {
        panic!("winner commits")
    };
    assert_eq!(
        reader
            .check_action(&space, &world(), &device(), &request, &[7u8; 32])
            .unwrap(),
        crate::ReceiptCheck::Replayed(winner_receipt)
    );
    assert_eq!(
        reader
            .check_action(&space, &world(), &device(), &request, &[8u8; 32])
            .unwrap_err(),
        crate::transaction::commit::Failure::RequestIdConflict
    );
    let checked_request = [28u8; 16];
    let checked = match replica
        .receipt_reader()
        .unwrap()
        .check_action(&space, &world(), &device(), &checked_request, &[28u8; 32])
        .unwrap()
    {
        crate::ReceiptCheck::Absent(proof) => proof,
        crate::ReceiptCheck::Replayed(_) => panic!("fresh checked scope"),
    };
    let prepared = match replica
        .prepare_action_checked(
            &ctx,
            &auth,
            crate::receipt::Interpretation::UNSPECIFIED,
            &world(),
            &device(),
            &checked_request,
            &[28u8; 32],
            b"checked".to_vec(),
            Vec::new(),
            "checked",
            &[],
            &[],
            &[],
            checked,
        )
        .unwrap()
    {
        PreparedActionOutcome::Prepared(prepared) => prepared,
        PreparedActionOutcome::Replayed(_) => panic!("absence proof must prepare"),
    };
    let checked_receipt = prepared.finalize(&mut replica, &ctx).unwrap();
    assert_eq!(
        replica
            .receipt_reader()
            .unwrap()
            .lookup_action(&space, &world(), &device(), &checked_request, &[28u8; 32],)
            .unwrap(),
        Some(checked_receipt)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn prepared_action_is_queryable_before_publish_and_drop_is_exact_rollback() {
    let dir = temp_store("prepared-action");
    let mut replica = open(&dir);
    let prior = replica.read_snapshot();
    let prior_root = replica.manifest_root();
    let prior_frontier = replica.frontier();
    let request = [22u8; 16];
    let space = space();
    let signer = SeedSigner(&WRITER_SEED);
    let ctx = CommitContext {
        space: &space,
        signer: &signer,
        authority_frontier: authority_frontier(),
    };
    let authorizer = test_auth();
    let auth = CommitAuthorization {
        actor: TEST_ACTOR,
        parent_manifest_root: prior_root,
        demand: test_demand(),
        intent_digest: [7u8; 32],
        authorizer: &authorizer,
    };
    let ops = vec![(
        body(1),
        Op::ReplaceAtomic {
            value: b"candidate".to_vec(),
        },
    )];
    let bindings = vec![(body(1), atomic_binding())];

    {
        let outcome = replica
            .prepare_action(
                &ctx,
                &auth,
                crate::receipt::Interpretation::UNSPECIFIED,
                &world(),
                &device(),
                &request,
                &[7u8; 32],
                b"effect".to_vec(),
                vec![body(1)],
                "replace",
                &ops,
                &bindings,
                &[],
            )
            .unwrap();
        match outcome {
            PreparedActionOutcome::Prepared(prepared) => {
                assert_eq!(prepared.context_cardinality_for_test(), (1, 0));
                let candidate = prepared.candidate_snapshot(&prior).unwrap();
                assert_eq!(replica.read(&body(1)), None);
                let busy_started = Instant::now();
                let contender = replica
                    .prepare_action(
                        &ctx,
                        &auth,
                        crate::receipt::Interpretation::UNSPECIFIED,
                        &world(),
                        &device(),
                        &[23u8; 16],
                        &[9u8; 32],
                        Vec::new(),
                        vec![body(1)],
                        "contender",
                        &ops,
                        &bindings,
                        &[],
                    )
                    .err();
                assert!(busy_started.elapsed() < Duration::from_millis(100));
                assert_eq!(
                    contender,
                    Some(crate::transaction::commit::Failure::MutationBusy)
                );
                assert_eq!(candidate.read(&body(1)), Some(b"candidate".to_vec()));
                assert_eq!(
                    candidate.body_keys_with_schema_version(
                        &world(),
                        &SchemaId::parse("blob").unwrap(),
                        1,
                    ),
                    vec![body(1)]
                );
                assert!(prior
                    .body_keys_with_schema_version(&world(), &SchemaId::parse("blob").unwrap(), 1,)
                    .is_empty());
                assert_ne!(candidate.root(), prior.root());
                drop(prepared);
            }
            PreparedActionOutcome::Replayed(_) => panic!("fresh request must prepare"),
        }
    }

    // Candidate extraction rejected it: neither the live semantic image nor
    // the durable coordinate, frontier, or idempotency scope changed.
    assert_eq!(replica.read(&body(1)), None);
    assert_eq!(replica.manifest_root(), prior_root);
    assert_eq!(replica.frontier(), prior_frontier);
    assert_eq!(
        replica
            .lookup_action(&space, &world(), &device(), &request, &[7u8; 32])
            .unwrap(),
        None
    );

    let candidate_root;
    let receipt;
    {
        let outcome = replica
            .prepare_action(
                &ctx,
                &auth,
                crate::receipt::Interpretation::UNSPECIFIED,
                &world(),
                &device(),
                &request,
                &[7u8; 32],
                b"effect".to_vec(),
                vec![body(1)],
                "replace",
                &ops,
                &bindings,
                &[],
            )
            .unwrap();
        match outcome {
            PreparedActionOutcome::Prepared(prepared) => {
                let candidate = prepared.candidate_snapshot(&prior).unwrap();
                candidate_root = candidate.root();
                receipt = prepared.finalize(&mut replica, &ctx).unwrap();
            }
            PreparedActionOutcome::Replayed(_) => panic!("rolled-back request must be fresh"),
        }
    }
    assert_eq!(replica.manifest_root(), candidate_root);
    assert_eq!(replica.read(&body(1)), Some(b"candidate".to_vec()));

    let replay = commit(&mut replica, request, "replace", &ops, &bindings).unwrap();
    assert_eq!(replay, ActionOutcome::Replayed(receipt.clone()));

    drop(replica);
    let reopened = open(&dir);
    assert_eq!(reopened.manifest_root(), candidate_root);
    assert_eq!(reopened.read(&body(1)), Some(b"candidate".to_vec()));
    assert_eq!(
        reopened.read_snapshot().body_keys_with_schema_version(
            &world(),
            &SchemaId::parse("blob").unwrap(),
            1,
        ),
        vec![body(1)]
    );
    assert_eq!(
        reopened
            .lookup_action(&space, &world(), &device(), &request, &[7u8; 32])
            .unwrap(),
        Some(receipt)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn prepared_action_finalize_is_an_exact_parent_cas() {
    let dir = temp_store("prepared-parent-cas");
    let mut replica = open(&dir);
    let prior = replica.read_snapshot();
    let prior_root = replica.manifest_root();
    let space = space();
    let signer = SeedSigner(&WRITER_SEED);
    let ctx = CommitContext {
        space: &space,
        signer: &signer,
        authority_frontier: authority_frontier(),
    };
    let authorizer = test_auth();
    let auth = CommitAuthorization {
        actor: TEST_ACTOR,
        parent_manifest_root: prior_root,
        demand: test_demand(),
        intent_digest: [41u8; 32],
        authorizer: &authorizer,
    };
    let ops = vec![(
        body(1),
        Op::ReplaceAtomic {
            value: b"candidate".to_vec(),
        },
    )];
    let bindings = vec![(body(1), atomic_binding())];
    let prepared = match replica
        .prepare_action(
            &ctx,
            &auth,
            crate::receipt::Interpretation::UNSPECIFIED,
            &world(),
            &device(),
            &[41u8; 16],
            &[41u8; 32],
            Vec::new(),
            vec![body(1)],
            "replace",
            &ops,
            &bindings,
            &[],
        )
        .unwrap()
    {
        PreparedActionOutcome::Prepared(prepared) => prepared,
        PreparedActionOutcome::Replayed(_) => panic!("fresh request must prepare"),
    };
    let candidate = prepared.candidate_snapshot(&prior).unwrap();
    assert_eq!(candidate.read(&body(1)), Some(b"candidate".to_vec()));

    // The production mutation lane prevents this interleaving. The narrow
    // hook models authoritative truth advancing between detached extraction
    // and finalize so the Replica-level compare-and-swap remains independently
    // fail-closed.
    replica.advance_parent_for_test();
    assert_eq!(
        prepared.finalize(&mut replica, &ctx).unwrap_err(),
        crate::transaction::commit::Failure::ParentManifestUnavailable
    );
    assert_eq!(replica.read(&body(1)), None);
    assert_eq!(replica.manifest_root(), prior_root);

    drop(replica);
    let reopened = open(&dir);
    assert_eq!(reopened.read(&body(1)), None);
    assert_eq!(reopened.manifest_root(), prior_root);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn prepared_action_rollback_poison_is_shared_and_fail_closed() {
    let dir = temp_store("prepared-rollback-poison");
    let mut replica = open(&dir);
    let prior_root = replica.manifest_root();
    let space = space();
    let signer = SeedSigner(&WRITER_SEED);
    let ctx = CommitContext {
        space: &space,
        signer: &signer,
        authority_frontier: authority_frontier(),
    };
    let authorizer = test_auth();
    let auth = CommitAuthorization {
        actor: TEST_ACTOR,
        parent_manifest_root: prior_root,
        demand: test_demand(),
        intent_digest: [42u8; 32],
        authorizer: &authorizer,
    };
    let ops = vec![(
        body(1),
        Op::ReplaceAtomic {
            value: b"candidate".to_vec(),
        },
    )];
    let bindings = vec![(body(1), atomic_binding())];
    let prepared = match replica
        .prepare_action(
            &ctx,
            &auth,
            crate::receipt::Interpretation::UNSPECIFIED,
            &world(),
            &device(),
            &[42u8; 16],
            &[42u8; 32],
            Vec::new(),
            vec![body(1)],
            "replace",
            &ops,
            &bindings,
            &[],
        )
        .unwrap()
    {
        PreparedActionOutcome::Prepared(prepared) => prepared,
        PreparedActionOutcome::Replayed(_) => panic!("fresh request must prepare"),
    };
    prepared.simulate_rollback_poison_for_test();
    drop(prepared);
    assert_eq!(replica.read(&body(1)), None);
    assert_eq!(
        replica
            .prepare_action(
                &ctx,
                &auth,
                crate::receipt::Interpretation::UNSPECIFIED,
                &world(),
                &device(),
                &[43u8; 16],
                &[43u8; 32],
                Vec::new(),
                vec![body(1)],
                "replace",
                &ops,
                &bindings,
                &[],
            )
            .err(),
        Some(crate::transaction::commit::Failure::Poisoned)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_store_addresses_canonical_objects_not_an_engine_snapshot() {
    let dir = temp_store("objects");
    let mut r = open(&dir);
    commit(
        &mut r,
        [22u8; 16],
        "created",
        &[(
            body(2),
            Op::RegisterSet {
                path: "title".into(),
                value: b"the plaintext title".to_vec(),
            },
        )],
        &[(body(2), collab_binding())],
    )
    .unwrap();
    drop(r);

    // Inspect the raw store: the required set must name at least the
    // transaction record, one protected Body object, the receipt, and the
    // manifest root — and every required object must decode as one of those
    // canonical forms, an immutable generation delta, or an index node (no
    // whole-engine snapshot object).
    let store = journal::Store::open(&dir).unwrap();
    let required = store.required_objects().unwrap();
    assert!(
        required.len() >= 4,
        "transaction + protected body + receipt + manifest objects, got {}",
        required.len()
    );
    let mut classified = 0;
    for obj in &required {
        let bytes = store.read_object(obj).unwrap();
        let is_tx = crate::transaction::Transaction::decode_canonical(&bytes).is_ok();
        let is_receipt = crate::receipt::RequestReceipt::decode_canonical(&bytes).is_ok();
        let is_root = crate::manifest::ManifestRoot::decode_canonical(&bytes).is_ok();
        let is_node = crate::index::IndexNode::decode_canonical(&bytes).is_ok();
        let is_generation = crate::replica::is_canonical_generation_delta(&bytes);
        let is_protected = mechanics::authorization::body_epoch_id(&bytes) == Some(EPOCH);
        assert!(
            is_tx || is_receipt || is_root || is_node || is_generation || is_protected,
            "an object is none of the canonical forms"
        );
        classified += 1;
        // At rest, no plaintext Body payload anywhere.
        let needle = b"the plaintext title";
        assert!(
            !bytes.windows(needle.len()).any(|w| w == needle.as_slice()),
            "a durable object leaks plaintext Body material"
        );
    }
    assert_eq!(classified, required.len());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn exact_incorporation_converges_two_replicas() {
    let dir_a = temp_store("conv-a");
    let dir_b = temp_store("conv-b");
    let mut a = open(&dir_a);
    let mut b = open(&dir_b);

    commit(
        &mut a,
        [23u8; 16],
        "created",
        &counter_ops(&body(3), 4),
        &[(body(3), collab_binding())],
    )
    .unwrap();

    // A exports its retained material; B incorporates through the exact path.
    let material = a.export_material().unwrap();
    assert_eq!(material.len(), 1);
    let (tx, payloads) = &material[0];
    let space = space();
    let signer = SeedSigner(&WRITER_SEED);
    let ctx = CommitContext {
        space: &space,
        signer: &signer,
        authority_frontier: authority_frontier(),
    };
    let outcome = b
        .incorporate(&ctx, tx, payloads, &WriterAuthorized)
        .unwrap();
    assert_eq!(outcome.accepted, 1, "{outcome:?}");
    assert!(outcome.advanced());
    assert_eq!(outcome.changes.len(), 1);
    assert_eq!(outcome.changes[0].operation, [23u8; 16]);
    assert_eq!(outcome.changes[0].actor.as_str(), TEST_ACTOR);
    assert_eq!(outcome.changes[0].device, device());
    assert_eq!(outcome.changes[0].bodies, vec![body(3)]);
    assert_eq!(b.read_collaborative(&body(3)).unwrap().counters["votes"], 4);

    // B edits; A incorporates back; both agree.
    commit(
        &mut b,
        [24u8; 16],
        "edited",
        &counter_ops(&body(3), 6),
        &[(body(3), collab_binding())],
    )
    .unwrap();
    let material = b.export_material().unwrap();
    let (tx, payloads) = &material[0];
    let outcome = a
        .incorporate(&ctx, tx, payloads, &WriterAuthorized)
        .unwrap();
    assert_eq!(outcome.accepted, 1);
    assert_eq!(outcome.changes[0].operation, [24u8; 16]);
    assert_eq!(
        a.read_collaborative(&body(3)).unwrap().counters["votes"],
        10
    );

    // Re-incorporating known material is unchanged.
    let before = a.frontier();
    let material = b.export_material().unwrap();
    let (tx, payloads) = &material[0];
    let outcome = a
        .incorporate(&ctx, tx, payloads, &WriterAuthorized)
        .unwrap();
    assert_eq!(outcome.accepted, 0);
    assert!(outcome.unchanged >= 1);
    assert_eq!(a.frontier(), before);

    // Incorporated + locally-committed material survives B's cold reopen.
    drop(b);
    let b = open(&dir_b);
    assert_eq!(
        b.read_collaborative(&body(3)).unwrap().counters["votes"],
        10,
        "B durably holds A's incorporated 4 plus its own 6"
    );
    let _ = std::fs::remove_dir_all(&dir_a);
    let _ = std::fs::remove_dir_all(&dir_b);
}

#[test]
fn a_cold_peer_accepts_a_complete_artifact_closure_out_of_dependency_order() {
    let dir_a = temp_store("artifact-order-a");
    let dir_b = temp_store("artifact-order-b");
    let mut a = open(&dir_a);
    let mut b = open(&dir_b);

    commit(
        &mut a,
        [71u8; 16],
        "checkpoint",
        &counter_ops(&body(71), 2),
        &[(body(71), collab_binding())],
    )
    .unwrap();
    commit(
        &mut a,
        [72u8; 16],
        "delta",
        &counter_ops(&body(71), 3),
        &[(body(71), collab_binding())],
    )
    .unwrap();

    let mut material = a.export_material().unwrap();
    let (tx, payloads) = material.first_mut().expect("latest signed head");
    let (key, pack) = payloads.first_mut().expect("one Body closure");
    let descriptor = tx
        .core
        .descriptors
        .iter()
        .find(|descriptor| descriptor.key() == *key)
        .expect("signed Body descriptor");
    let mut hostile = pack.clone();
    hostile[3..11].fill(0xff);
    assert!(crate::replica::decode_artifact_pack(descriptor, &hostile).is_err());
    let mut envelopes =
        crate::replica::decode_artifact_pack(descriptor, pack).expect("artifact pack");
    assert!(envelopes.len() >= 2, "checkpoint plus delta");
    envelopes.reverse();
    *pack = crate::replica::encode_artifact_pack(&envelopes).expect("reordered delivery pack");

    let space = space();
    let signer = SeedSigner(&WRITER_SEED);
    let ctx = CommitContext {
        space: &space,
        signer: &signer,
        authority_frontier: authority_frontier(),
    };
    let outcome = b
        .incorporate(&ctx, tx, payloads, &WriterAuthorized)
        .unwrap();
    assert_eq!(outcome.accepted, 1, "{outcome:?}");
    assert_eq!(
        b.read_collaborative(&body(71)).unwrap().counters["votes"],
        5
    );

    drop(b);
    let b = open(&dir_b);
    assert_eq!(
        b.read_collaborative(&body(71)).unwrap().counters["votes"],
        5,
        "the cold peer reconstructs from signed refs, independent of delivery order"
    );
    let _ = std::fs::remove_dir_all(&dir_a);
    let _ = std::fs::remove_dir_all(&dir_b);
}

#[test]
fn one_signed_closure_may_span_authorized_key_epochs() {
    let dir_a = temp_store("artifact-epochs-a");
    let dir_b = temp_store("artifact-epochs-b");
    let rotating = Arc::new(RotatingBodyKeys::new());
    let mut a = Replica::open(&dir_a, rotating.clone()).unwrap();
    a.set_supported(supported());

    commit(
        &mut a,
        [73u8; 16],
        "old epoch checkpoint",
        &counter_ops(&body(73), 4),
        &[(body(73), collab_binding())],
    )
    .unwrap();
    rotating.rotate();
    commit(
        &mut a,
        [74u8; 16],
        "new epoch delta",
        &counter_ops(&body(73), 6),
        &[(body(73), collab_binding())],
    )
    .unwrap();

    let material = a.export_material().unwrap();
    let (tx, payloads) = material.first().unwrap();
    let (key, pack) = payloads.first().unwrap();
    let descriptor = tx
        .core
        .descriptors
        .iter()
        .find(|descriptor| descriptor.key() == *key)
        .expect("signed Body descriptor");
    let envelopes = crate::replica::decode_artifact_pack(descriptor, pack).unwrap();
    let epochs: std::collections::BTreeSet<[u8; 16]> = envelopes
        .iter()
        .map(|envelope| mechanics::authorization::body_epoch_id(envelope).unwrap())
        .collect();
    assert_eq!(epochs, std::collections::BTreeSet::from([EPOCH, [8u8; 16]]));

    let mut b = Replica::open(&dir_b, rotating.clone()).unwrap();
    b.set_supported(supported());
    let space = space();
    let signer = SeedSigner(&WRITER_SEED);
    let ctx = CommitContext {
        space: &space,
        signer: &signer,
        authority_frontier: authority_frontier(),
    };
    b.incorporate(&ctx, tx, payloads, &WriterAuthorized)
        .unwrap();
    assert_eq!(
        b.read_collaborative(&body(73)).unwrap().counters["votes"],
        10
    );
    drop(b);

    let mut b = Replica::open(&dir_b, rotating).unwrap();
    b.set_supported(supported());
    assert_eq!(
        b.read_collaborative(&body(73)).unwrap().counters["votes"],
        10
    );
    let _ = std::fs::remove_dir_all(&dir_a);
    let _ = std::fs::remove_dir_all(&dir_b);
}

#[test]
fn illegitimate_or_tampered_material_never_reaches_the_engine() {
    let dir_a = temp_store("illeg-a");
    let mut a = open(&dir_a);
    commit(
        &mut a,
        [25u8; 16],
        "created",
        &counter_ops(&body(4), 1),
        &[(body(4), collab_binding())],
    )
    .unwrap();
    let material = a.export_material().unwrap();
    let (tx, payloads) = &material[0];

    struct DenyAll;
    impl crate::transaction::AuthoritySource for DenyAll {
        fn signer_authorized(&self, _s: &[u8; 32], _f: &AuthorityFrontier) -> bool {
            false
        }
    }
    let mut b = Replica::loro().with_keys(keys());
    b.set_supported(supported());
    let space = space();
    let signer = SeedSigner(&WRITER_SEED);
    let ctx = CommitContext {
        space: &space,
        signer: &signer,
        authority_frontier: authority_frontier(),
    };
    // Unauthorized signer: refused before the engine.
    assert!(matches!(
        b.incorporate(&ctx, tx, payloads, &DenyAll),
        Err(crate::transaction::commit::Failure::Illegitimate(_))
    ));
    assert!(b.read_collaborative(&body(4)).is_err());

    // Tampered payload: the commitment binding refuses it.
    let mut tampered = payloads.clone();
    tampered[0].1.push(0);
    assert!(matches!(
        b.incorporate(&ctx, tx, &tampered, &WriterAuthorized),
        Err(crate::transaction::commit::Failure::Illegitimate(_))
    ));
    assert!(b.read_collaborative(&body(4)).is_err());

    // A payload keyed to a Body the transaction has no descriptor for.
    let stray = vec![(body(9), payloads[0].1.clone())];
    assert!(matches!(
        b.incorporate(&ctx, tx, &stray, &WriterAuthorized),
        Err(crate::transaction::commit::Failure::Illegitimate(_))
    ));

    // The untampered material still incorporates.
    b.incorporate(&ctx, tx, payloads, &WriterAuthorized)
        .unwrap();
    assert_eq!(b.read_collaborative(&body(4)).unwrap().counters["votes"], 1);
    let _ = std::fs::remove_dir_all(&dir_a);
}

#[test]
fn unknown_world_material_is_retained_opaquely_and_forwarded_byte_identically() {
    let dir_a = temp_store("opaque-a");
    let dir_b = temp_store("opaque-b");
    let dir_c = temp_store("opaque-c");
    let mut a = open(&dir_a);
    commit(
        &mut a,
        [26u8; 16],
        "created",
        &counter_ops(&body(5), 3),
        &[(body(5), collab_binding())],
    )
    .unwrap();
    let material = a.export_material().unwrap();
    let (tx, payloads) = &material[0];

    // B supports NOTHING: legitimate material is retained opaquely.
    let mut b = Replica::open(&dir_b, keys()).unwrap();
    // (no set_supported — empty)
    let space = space();
    let signer = SeedSigner(&WRITER_SEED);
    let ctx = CommitContext {
        space: &space,
        signer: &signer,
        authority_frontier: authority_frontier(),
    };
    let outcome = b
        .incorporate(&ctx, tx, payloads, &WriterAuthorized)
        .unwrap();
    assert_eq!(outcome.unsupported_retained, 1);
    assert!(outcome.advanced(), "opaque retention advances the frontier");
    assert!(b.is_opaque(&body(5)));
    assert!(
        b.read_collaborative(&body(5)).is_err() && b.read(&body(5)).is_none(),
        "opaque material has no interpreted view"
    );

    // The opaque material survives B's restart.
    drop(b);
    let b = Replica::open(&dir_b, keys()).unwrap();
    assert!(b.is_opaque(&body(5)));

    // B forwards to C byte-identically; C supports the schema and interprets.
    let forwarded = b.export_material().unwrap();
    assert_eq!(forwarded.len(), 1);
    let (ftx, fpayloads) = &forwarded[0];
    assert_eq!(ftx.encode(), tx.encode(), "transaction bytes identical");
    assert_eq!(
        fpayloads[0].1, payloads[0].1,
        "protected payload bytes identical"
    );
    let mut c = open(&dir_c);
    let outcome = c
        .incorporate(&ctx, ftx, fpayloads, &WriterAuthorized)
        .unwrap();
    assert_eq!(outcome.accepted, 1);
    assert_eq!(c.read_collaborative(&body(5)).unwrap().counters["votes"], 3);
    let _ = std::fs::remove_dir_all(&dir_a);
    let _ = std::fs::remove_dir_all(&dir_b);
    let _ = std::fs::remove_dir_all(&dir_c);
}

#[test]
fn a_missing_key_epoch_takes_the_opaque_branch() {
    let dir_a = temp_store("nokey-a");
    let mut a = open(&dir_a);
    commit(
        &mut a,
        [27u8; 16],
        "created",
        &counter_ops(&body(6), 2),
        &[(body(6), collab_binding())],
    )
    .unwrap();
    let material = a.export_material().unwrap();
    let (tx, payloads) = &material[0];

    // B supports the schema but holds a DIFFERENT epoch key.
    let other_keys = Arc::new(StaticBodyKeys::new(
        AuthorizedBodyKey::for_authorized_epoch([8u8; 16], [9u8; 32]),
    ));
    let mut b = Replica::loro().with_keys(other_keys);
    b.set_supported(supported());
    let space = space();
    let signer = SeedSigner(&WRITER_SEED);
    let ctx = CommitContext {
        space: &space,
        signer: &signer,
        authority_frontier: authority_frontier(),
    };
    let outcome = b
        .incorporate(&ctx, tx, payloads, &WriterAuthorized)
        .unwrap();
    assert_eq!(outcome.unsupported_retained, 1);
    assert!(b.is_opaque(&body(6)));
    assert!(b.read_collaborative(&body(6)).is_err());
    let _ = std::fs::remove_dir_all(&dir_a);
}

#[test]
fn durable_atomic_images_stay_cold_across_publish_reopen_and_first_edit() {
    let dir = temp_store("cold-atomic");
    let key = body(42);
    let mut replica = open(&dir);
    commit(
        &mut replica,
        [80u8; 16],
        "cold-create",
        &[(
            key.clone(),
            Op::ReplaceAtomic {
                value: b"first".to_vec(),
            },
        )],
        &[(key.clone(), atomic_binding())],
    )
    .unwrap();

    assert!(
        !replica.atomic_writer_image_loaded_for_test(&key),
        "durable publication releases the Atomic writer Arc"
    );
    let snapshot = replica.read_snapshot();
    let BodyImagePresence::Readable { body: body_ix, .. } = snapshot.body_presence(&key) else {
        panic!("published Atomic Body must be readable")
    };
    let bounds = snapshot
        .body_image_bounds(body_ix)
        .expect("admission bounds");
    assert!(bounds.protected_bytes > 0);
    assert!(
        bounds.decoded_upper_bound >= 3 * u64::try_from(b"first".len()).expect("fixture length")
    );
    assert_eq!(
        snapshot
            .resolve_body_image(body_ix)
            .unwrap()
            .read_shared()
            .as_deref(),
        Some(b"first".as_slice())
    );
    assert!(
        !replica.atomic_writer_image_loaded_for_test(&key),
        "exact reads inflate in an isolated proof Engine, never the writer"
    );

    drop(snapshot);
    drop(replica);
    let mut replica = open(&dir);
    assert!(
        !replica.atomic_writer_image_loaded_for_test(&key),
        "cold reopen verifies coordinates without decrypting Atomic payloads"
    );
    assert_eq!(replica.read(&key), Some(b"first".to_vec()));
    assert!(!replica.atomic_writer_image_loaded_for_test(&key));

    commit(
        &mut replica,
        [81u8; 16],
        "cold-first-edit",
        &[(
            key.clone(),
            Op::ReplaceAtomic {
                value: b"second".to_vec(),
            },
        )],
        &[],
    )
    .unwrap();
    assert_eq!(replica.read(&key), Some(b"second".to_vec()));
    assert!(
        !replica.atomic_writer_image_loaded_for_test(&key),
        "the touched writer image is released again after durability"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn durable_collaborative_images_resolve_cold_and_first_edit_preserves_identity() {
    let dir = temp_store("cold-collaborative");
    let key = body(45);
    let mut replica = open(&dir);
    commit(
        &mut replica,
        [86u8; 16],
        "cold-collaborative-create",
        &[(
            key.clone(),
            Op::TextSplice {
                path: "body".into(),
                index: 0,
                delete: 0,
                insert: "hello world".into(),
            },
        )],
        &[(key.clone(), collab_binding())],
    )
    .unwrap();
    let before = replica.read_snapshot();
    let before_version = before.body_version(&key).expect("causal version");
    let BodyImagePresence::Readable {
        body: before_ix, ..
    } = before.body_presence(&key)
    else {
        panic!("published collaborative Body must be readable")
    };
    let image = before.resolve_body_image(before_ix).unwrap();
    assert_eq!(
        image
            .read_collaborative()
            .unwrap()
            .texts
            .get("body")
            .map(String::as_str),
        Some("hello world")
    );
    let anchor = crate::ReadSnapshot::anchor_in_resolved_image(&key, &image, "body", 6)
        .unwrap()
        .expect("anchor");

    drop(image);
    drop(before);
    drop(replica);
    let mut replica = open(&dir);
    assert!(
        !replica.collaborative_writer_image_loaded_for_test(&key),
        "reopen verifies signed coordinates without reading a collaborative export"
    );
    let cold = replica.read_snapshot();
    assert_eq!(cold.body_version(&key), Some(before_version));
    let BodyImagePresence::Readable { body: cold_ix, .. } = cold.body_presence(&key) else {
        panic!("cold collaborative Body must remain readable")
    };
    let resolved = cold.resolve_body_image(cold_ix).unwrap();
    assert_eq!(
        crate::ReadSnapshot::resolve_anchor_in_resolved_image(&key, &resolved, &anchor).unwrap(),
        fabric::AnchorResolution::Resolved(6)
    );
    assert!(
        !replica.collaborative_writer_image_loaded_for_test(&key),
        "governed reads use an isolated proof Engine"
    );

    commit(
        &mut replica,
        [87u8; 16],
        "cold-collaborative-first-edit",
        &[(
            key.clone(),
            Op::TextSplice {
                path: "body".into(),
                index: 0,
                delete: 0,
                insert: ">> ".into(),
            },
        )],
        &[],
    )
    .unwrap();
    assert!(replica.collaborative_writer_image_loaded_for_test(&key));
    assert_eq!(
        replica.resolve_anchor(&key, &anchor),
        fabric::AnchorResolution::Resolved(9),
        "hydrating from the causal export preserves Loro operation identities"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn durable_collaborative_writer_cache_never_retains_more_than_sixty_four_bodies() {
    let dir = temp_store("collaborative-writer-lru");
    let mut replica = open(&dir);
    let mut ops = Vec::new();
    let mut bindings = Vec::new();
    for number in 60u8..=124u8 {
        let key = body(number);
        ops.extend(counter_ops(&key, 1));
        bindings.push((key, collab_binding()));
    }
    commit(
        &mut replica,
        [88u8; 16],
        "bounded-collaborative-writers",
        &ops,
        &bindings,
    )
    .unwrap();
    assert!(
        replica.writer_body_count_for_test() <= 64,
        "durable causal closures, not frozen exports, own evicted writers"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn cold_atomic_presence_and_resolution_fail_closed() {
    let dir = temp_store("cold-atomic-failures");
    let key = body(43);
    let mut replica = open(&dir);
    commit(
        &mut replica,
        [82u8; 16],
        "cold-failure",
        &[(
            key.clone(),
            Op::ReplaceAtomic {
                value: b"protected".to_vec(),
            },
        )],
        &[(key.clone(), atomic_binding())],
    )
    .unwrap();
    let snapshot = replica.read_snapshot();
    let BodyImagePresence::Readable { body: body_ix, .. } = snapshot.body_presence(&key) else {
        panic!("published Atomic Body must be readable")
    };
    let artifact = snapshot
        .body_image_artifacts_for_test(body_ix)
        .into_iter()
        .next()
        .expect("protected closure");
    let object_name = artifact
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    std::fs::write(dir.join("objects").join(object_name), b"tampered").unwrap();
    drop(snapshot);
    drop(replica);
    let replica = open(&dir);
    let snapshot = replica.read_snapshot();
    let BodyImagePresence::Readable { body: body_ix, .. } = snapshot.body_presence(&key) else {
        panic!("corrupt lazy material does not collapse presence")
    };
    assert!(
        matches!(
            snapshot.resolve_body_image(body_ix),
            Err(BodyImageFailure::Corrupt)
        ),
        "a resolver never caches or returns partially verified material"
    );
    drop(snapshot);
    drop(replica);
    let _ = std::fs::remove_dir_all(&dir);

    let dir = temp_store("cold-atomic-key-loss");
    let mut replica = open(&dir);
    commit(
        &mut replica,
        [83u8; 16],
        "cold-key-loss",
        &[(
            key.clone(),
            Op::ReplaceAtomic {
                value: b"protected".to_vec(),
            },
        )],
        &[(key.clone(), atomic_binding())],
    )
    .unwrap();
    drop(replica);
    let replica = Replica::open(&dir, Arc::new(NoBodyKeys)).unwrap();
    let snapshot = replica.read_snapshot();
    assert!(matches!(
        snapshot.body_presence(&key),
        BodyImagePresence::Opaque { .. }
    ));
    assert!(replica.is_opaque(&key));
    assert_eq!(replica.read(&key), None);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn pinned_atomic_publication_survives_key_retirement_supersession_and_gc() {
    let dir = temp_store("cold-atomic-pin");
    let key = body(44);
    let key_source = Arc::new(RevocableBodyKeys::new());
    let mut replica = Replica::open(&dir, key_source.clone()).unwrap();
    replica.set_supported(supported());
    commit(
        &mut replica,
        [84u8; 16],
        "pinned-first",
        &[(
            key.clone(),
            Op::ReplaceAtomic {
                value: b"pinned-first".to_vec(),
            },
        )],
        &[(key.clone(), atomic_binding())],
    )
    .unwrap();
    let pinned = replica.read_snapshot();
    let BodyImagePresence::Readable {
        body: pinned_ix, ..
    } = pinned.body_presence(&key)
    else {
        panic!("pinned Body must be readable")
    };

    commit(
        &mut replica,
        [85u8; 16],
        "pinned-second",
        &[(
            key.clone(),
            Op::ReplaceAtomic {
                value: b"current-second".to_vec(),
            },
        )],
        &[],
    )
    .unwrap();
    key_source.revoke();
    replica.collect_unreachable_for_test().unwrap();
    assert_eq!(
        pinned
            .resolve_body_image(pinned_ix)
            .unwrap()
            .read_shared()
            .as_deref(),
        Some(b"pinned-first".as_slice()),
        "the exact snapshot owns both its opening key and object lease"
    );
    drop(pinned);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn concurrent_atomic_writes_resolve_to_one_deterministic_winner() {
    // A and B write the same atomic Body concurrently, then exchange. Both
    // must end on the SAME value regardless of incorporation order.
    let dir_a = temp_store("atomic-a");
    let dir_b = temp_store("atomic-b");
    let mut a = open(&dir_a);
    let mut b = open(&dir_b);
    commit(
        &mut a,
        [28u8; 16],
        "write",
        &[(
            body(7),
            Op::ReplaceAtomic {
                value: b"from-a".to_vec(),
            },
        )],
        &[(body(7), atomic_binding())],
    )
    .unwrap();
    commit(
        &mut b,
        [29u8; 16],
        "write",
        &[(
            body(7),
            Op::ReplaceAtomic {
                value: b"from-b".to_vec(),
            },
        )],
        &[(body(7), atomic_binding())],
    )
    .unwrap();

    let space = space();
    let signer = SeedSigner(&WRITER_SEED);
    let ctx = CommitContext {
        space: &space,
        signer: &signer,
        authority_frontier: authority_frontier(),
    };
    let from_a = a.export_material().unwrap();
    let from_b = b.export_material().unwrap();
    let (tx_a, pay_a) = &from_a[0];
    let (tx_b, pay_b) = &from_b[0];
    a.incorporate(&ctx, tx_b, pay_b, &WriterAuthorized).unwrap();
    b.incorporate(&ctx, tx_a, pay_a, &WriterAuthorized).unwrap();
    assert_eq!(
        a.read(&body(7)),
        b.read(&body(7)),
        "deterministic winner regardless of order"
    );
    assert!(a.read(&body(7)).is_some());
    let _ = std::fs::remove_dir_all(&dir_a);
    let _ = std::fs::remove_dir_all(&dir_b);
}

#[test]
fn immutable_atomic_accepts_only_its_content_derived_address() {
    let dir = temp_store("immutable-local");
    let mut replica = open(&dir);
    let value = b"create-once".to_vec();
    let key = immutable_body(&value);
    commit(
        &mut replica,
        [81u8; 16],
        "create immutable",
        &[(
            key.clone(),
            Op::ReplaceAtomic {
                value: value.clone(),
            },
        )],
        &[(key.clone(), immutable_binding())],
    )
    .unwrap();
    assert_eq!(replica.read(&key), Some(value.clone()));

    // A distinct signed request carrying identical bytes is semantically
    // idempotent. Fabric may advance the transaction envelope, but it cannot
    // change the immutable value.
    commit(
        &mut replica,
        [82u8; 16],
        "replay immutable bytes",
        &[(
            key.clone(),
            Op::ReplaceAtomic {
                value: value.clone(),
            },
        )],
        &[],
    )
    .unwrap();
    assert_eq!(replica.read(&key), Some(value));

    let different = commit(
        &mut replica,
        [83u8; 16],
        "replace immutable",
        &[(
            key.clone(),
            Op::ReplaceAtomic {
                value: b"different".to_vec(),
            },
        )],
        &[],
    )
    .unwrap_err();
    assert_eq!(
        different,
        crate::transaction::commit::Failure::ImmutableConflict
    );

    let random_key = body(81);
    let mismatch = commit(
        &mut replica,
        [84u8; 16],
        "bad immutable address",
        &[(
            random_key.clone(),
            Op::ReplaceAtomic {
                value: b"create-once".to_vec(),
            },
        )],
        &[(random_key, immutable_binding())],
    )
    .unwrap_err();
    assert_eq!(
        mismatch,
        crate::transaction::commit::Failure::ImmutableConflict
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn immutable_atomic_peer_incorporation_is_arrival_order_independent() {
    let dir_good = temp_store("immutable-peer-good");
    let dir_bad = temp_store("immutable-peer-bad");
    let dir_a = temp_store("immutable-peer-a");
    let dir_b = temp_store("immutable-peer-b");
    let good_value = b"canonical immutable".to_vec();
    let key = immutable_body(&good_value);

    let mut good_author = open(&dir_good);
    commit(
        &mut good_author,
        [85u8; 16],
        "good immutable",
        &[(
            key.clone(),
            Op::ReplaceAtomic {
                value: good_value.clone(),
            },
        )],
        &[(key.clone(), immutable_binding())],
    )
    .unwrap();
    let good_export = good_author.export_material().unwrap();
    let (good_tx, good_payloads) = &good_export[0];

    // Produce valid signed/protected atomic material for a different value at
    // the good key, then claim the immutable schema in a newly signed
    // descriptor. Structural and authority checks pass; only the
    // content-address invariant can refuse it.
    let mut bad_author = open(&dir_bad);
    commit(
        &mut bad_author,
        [86u8; 16],
        "ordinary atomic forgery source",
        &[(
            key.clone(),
            Op::ReplaceAtomic {
                value: b"different immutable".to_vec(),
            },
        )],
        &[(key.clone(), atomic_binding())],
    )
    .unwrap();
    let bad_export = bad_author.export_material().unwrap();
    let (ordinary_bad_tx, bad_payloads) = &bad_export[0];
    let mut bad_descriptor = ordinary_bad_tx.core.descriptors[0].clone();
    bad_descriptor.schema = immutable_binding().schema;
    bad_descriptor.mutation_model = MUTATION_IMMUTABLE_ATOMIC;
    let bad_tx = resign_with_descriptor(ordinary_bad_tx, bad_descriptor);

    let space = space();
    let signer = SeedSigner(&WRITER_SEED);
    let ctx = CommitContext {
        space: &space,
        signer: &signer,
        authority_frontier: authority_frontier(),
    };
    let mut a = open(&dir_a);
    a.incorporate(&ctx, good_tx, good_payloads, &WriterAuthorized)
        .unwrap();
    assert_eq!(
        a.incorporate(&ctx, &bad_tx, bad_payloads, &WriterAuthorized),
        Err(crate::transaction::commit::Failure::ImmutableConflict)
    );

    let mut b = open(&dir_b);
    assert_eq!(
        b.incorporate(&ctx, &bad_tx, bad_payloads, &WriterAuthorized),
        Err(crate::transaction::commit::Failure::ImmutableConflict)
    );
    b.incorporate(&ctx, good_tx, good_payloads, &WriterAuthorized)
        .unwrap();
    assert_eq!(a.read(&key), Some(good_value.clone()));
    assert_eq!(b.read(&key), Some(good_value));

    let _ = std::fs::remove_dir_all(&dir_good);
    let _ = std::fs::remove_dir_all(&dir_bad);
    let _ = std::fs::remove_dir_all(&dir_a);
    let _ = std::fs::remove_dir_all(&dir_b);
}

#[test]
fn a_durable_replica_refuses_unattributed_commits_and_missing_keys() {
    let dir = temp_store("refuse");
    let mut r = open(&dir);
    // The unattributed test-only commit path is refused on a durable store.
    assert!(matches!(
        r.commit("x", &counter_ops(&body(8), 1)),
        Err(crate::transaction::commit::Failure::Illegitimate(_))
    ));
    drop(r);

    // A durable Replica with no sealing key refuses local writes, typed.
    struct NoKeys;
    impl crate::body::BodyKeySource for NoKeys {
        fn sealing_key(&self) -> Option<AuthorizedBodyKey> {
            None
        }
        fn opening_key(&self, _epoch: &[u8; 16]) -> Option<AuthorizedBodyKey> {
            None
        }
    }
    let mut r = Replica::open(&dir, Arc::new(NoKeys)).unwrap();
    r.set_supported(supported());
    let err = commit(
        &mut r,
        [30u8; 16],
        "x",
        &counter_ops(&body(8), 1),
        &[(body(8), collab_binding())],
    )
    .unwrap_err();
    assert_eq!(err, crate::transaction::commit::Failure::BodyKeyUnavailable);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn schema_bindings_are_immutable_across_writes() {
    let dir = temp_store("binding");
    let mut r = open(&dir);
    commit(
        &mut r,
        [31u8; 16],
        "created",
        &counter_ops(&body(9), 1),
        &[(body(9), collab_binding())],
    )
    .unwrap();
    // A later write declaring a DIFFERENT binding for the same Body refuses.
    let err = commit(
        &mut r,
        [32u8; 16],
        "edited",
        &counter_ops(&body(9), 1),
        &[(body(9), atomic_binding())],
    )
    .unwrap_err();
    assert_eq!(err, crate::transaction::commit::Failure::SchemaMismatch);
    // And an op with NO binding on a brand-new Body refuses (no declaration).
    let err = commit(
        &mut r,
        [33u8; 16],
        "edited",
        &counter_ops(&body(10), 1),
        &[],
    )
    .unwrap_err();
    assert_eq!(err, crate::transaction::commit::Failure::SchemaMismatch);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_engine_export_envelope_is_gone() {
    // C1.2's deletion gate: the reserved interim envelope is absent from the
    // public surface. (The names would only reappear as a compile error here.)
    // This is a compile-time proof by absence: `crate::ENGINE_EXPORT_WORLD`
    // no longer exists, and the only incorporation path takes a signed
    // transaction plus descriptor-bound payloads.
    #[allow(clippy::type_complexity)]
    let _: fn(
        &mut Replica,
        &CommitContext<'_>,
        &Transaction,
        &[(BodyKey, Vec<u8>)],
        &dyn crate::transaction::AuthoritySource,
    ) -> Result<
        crate::convergence::ConvergenceOutcome,
        crate::transaction::commit::Failure,
    > = Replica::incorporate;
}

#[test]
fn a_prebuilt_checkpoint_installs_without_a_hard_threshold_cliff() {
    let mut r = Replica::loro().with_keys(keys());
    r.set_supported(supported());
    let key = body(42);
    commit(
        &mut r,
        [40u8; 16],
        "create-hot-body",
        &counter_ops(&key, 1),
        &[(key.clone(), collab_binding())],
    )
    .unwrap();

    let mut ordinary = Vec::new();
    for sequence in 1u16..=192 {
        let mut request = [0u8; 16];
        request[..2].copy_from_slice(&sequence.to_be_bytes());
        request[2] = 41;
        let started = std::time::Instant::now();
        commit(&mut r, request, "hot-edit", &counter_ops(&key, 1), &[]).unwrap();
        ordinary.push(started.elapsed());
    }

    // The soft watermark edit publishes first; checkpoint construction then
    // runs independently of the committing Replica borrow.
    std::thread::sleep(std::time::Duration::from_millis(100));
    let started = std::time::Instant::now();
    commit(
        &mut r,
        [42u8; 16],
        "install-prebuilt-checkpoint",
        &counter_ops(&key, 1),
        &[],
    )
    .unwrap();
    let install = started.elapsed();

    let material = r.export_material().unwrap();
    let descriptor = material[0]
        .0
        .core
        .descriptors
        .iter()
        .find(|descriptor| descriptor.key() == key)
        .unwrap();
    assert_eq!(
        descriptor.artifact_refs().count(),
        2,
        "ready checkpoint + the installing edit; the 192-artifact prefix is gone"
    );

    ordinary.sort();
    let p99 = ordinary[ordinary.len() * 99 / 100];
    let ceiling = std::cmp::max(
        p99.saturating_mul(20),
        std::time::Duration::from_millis(250),
    );
    eprintln!("checkpoint action p99={p99:?} install={install:?} ceiling={ceiling:?}");
    assert!(
        install <= ceiling,
        "installing ready material must stay in the ordinary-action timing class"
    );
}

#[test]
fn a_stalled_checkpoint_worker_never_turns_the_target_crossing_into_a_body_snapshot() {
    const TEXT_BYTES: usize = 1024 * 1024;
    const EDITS: u16 = 300;

    let mut replica = Replica::loro().with_keys(keys());
    replica.set_supported(supported());
    let key = body(43);
    let text = incompressible_ascii(TEXT_BYTES);
    let mut initial_ops = vec![(key.clone(), Op::Create)];
    for (chunk, bytes) in text.as_bytes().chunks(64 * 1024).enumerate() {
        initial_ops.push((
            key.clone(),
            Op::TextSplice {
                path: "description".to_owned(),
                index: u64::try_from(chunk * 64 * 1024).expect("chunk offset"),
                delete: 0,
                insert: std::str::from_utf8(bytes)
                    .expect("ASCII fixture")
                    .to_owned(),
            },
        ));
    }
    commit(
        &mut replica,
        [43u8; 16],
        "large-body",
        &initial_ops,
        &[(key.clone(), collab_binding())],
    )
    .unwrap();

    // Hold an ordinary checkpoint job permanently in `Empty`: this models a
    // worker that was reserved at the soft watermark but cannot make progress.
    replica.stall_checkpoint_for_test(&key);
    let mut before_target = Vec::new();
    let mut after_target = Vec::new();
    for sequence in 1..=EDITS {
        let mut request = [0u8; 16];
        request[..2].copy_from_slice(&sequence.to_be_bytes());
        request[2] = 44;
        let started = std::time::Instant::now();
        commit(
            &mut replica,
            request,
            "scalar-edit",
            &[(
                key.clone(),
                Op::TextSplice {
                    path: "description".to_owned(),
                    index: u64::from(sequence % 1024),
                    delete: 1,
                    insert: "z".to_owned(),
                },
            )],
            &[],
        )
        .unwrap();
        let elapsed = started.elapsed();
        if sequence < 192 {
            before_target.push(elapsed);
        } else if sequence > 256 {
            after_target.push(elapsed);
        }
    }

    let exported = replica.export_material().unwrap();
    let descriptor = exported[0]
        .0
        .core
        .descriptors
        .iter()
        .find(|descriptor| descriptor.key() == key)
        .unwrap();
    assert_eq!(
        descriptor.material.delta_tail.len(),
        usize::from(EDITS),
        "crossing the 256-delta maintenance target must append a delta, not replace it with a synchronous checkpoint"
    );
    let policy = fabric::CheckpointPolicy::default();
    assert!(policy.should_checkpoint(
        descriptor.material.delta_tail.len(),
        usize::try_from(descriptor.material.tail_bytes()).unwrap_or(usize::MAX),
    ));
    assert!(descriptor.material.validate().is_ok());
    assert!(
        descriptor.material.delta_tail.len() <= policy.max_tail_deltas
            && descriptor.material.tail_bytes()
                <= u64::try_from(policy.max_tail_bytes).unwrap_or(u64::MAX),
        "cold reconstruction and transfer remain inside the explicit emergency envelope"
    );

    before_target.sort();
    after_target.sort();
    let ordinary_p99 = before_target[before_target.len() * 99 / 100];
    let stalled_p99 = after_target[after_target.len() * 99 / 100];
    let ceiling = std::cmp::max(
        ordinary_p99.saturating_mul(20),
        std::time::Duration::from_millis(250),
    );
    eprintln!(
        "stalled checkpoint: ordinary p99={ordinary_p99:?} target+ p99={stalled_p99:?} tail={} refs/{:.2} MiB",
        descriptor.material.delta_tail.len(),
        descriptor.material.tail_bytes() as f64 / (1024.0 * 1024.0)
    );
    assert!(
        stalled_p99 <= ceiling,
        "a stalled worker must leave target-crossing edits in the ordinary delta timing class"
    );
}

#[test]
fn thousands_of_hot_bodies_use_one_bounded_checkpoint_executor() {
    use crate::replica::CheckpointExecutor;

    const WORKERS: usize = 2;
    const QUEUE: usize = 8;
    const HOT_BODIES: usize = 4_096;

    let executor = CheckpointExecutor::new(WORKERS, QUEUE);
    assert_eq!(executor._workers, WORKERS);
    let release = Arc::new(AtomicBool::new(false));
    let active = Arc::new(AtomicUsize::new(0));
    let maximum = Arc::new(AtomicUsize::new(0));
    let completed = Arc::new(AtomicUsize::new(0));
    let seeds_captured = AtomicUsize::new(0);
    let mut admitted = 0usize;

    for _ in 0..HOT_BODIES {
        let Some(permit) = executor.try_reserve() else {
            continue;
        };
        // This counter stands in for the synchronous `checkpoint_seed` call:
        // production reserves at this exact point, before a frozen Body can be
        // imported or a live document cloned.
        seeds_captured.fetch_add(1, Ordering::SeqCst);
        let release = Arc::clone(&release);
        let active = Arc::clone(&active);
        let maximum = Arc::clone(&maximum);
        let completed = Arc::clone(&completed);
        let work = Box::new(move || {
            let now = active.fetch_add(1, Ordering::SeqCst) + 1;
            let mut observed = maximum.load(Ordering::SeqCst);
            while now > observed {
                match maximum.compare_exchange(observed, now, Ordering::SeqCst, Ordering::SeqCst) {
                    Ok(_) => break,
                    Err(current) => observed = current,
                }
            }
            while !release.load(Ordering::SeqCst) {
                std::thread::yield_now();
            }
            active.fetch_sub(1, Ordering::SeqCst);
            completed.fetch_add(1, Ordering::SeqCst);
        });
        if permit.submit(work).is_ok() {
            admitted += 1;
        }
    }

    assert!(
        seeds_captured.load(Ordering::SeqCst) <= WORKERS + QUEUE,
        "seed capture itself must be bounded, not only worker execution"
    );
    assert!(
        admitted <= WORKERS + QUEUE,
        "a burst may occupy only the fixed workers and bounded queue"
    );
    release.store(true, Ordering::SeqCst);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while completed.load(Ordering::SeqCst) != admitted && std::time::Instant::now() < deadline {
        std::thread::yield_now();
    }
    assert_eq!(completed.load(Ordering::SeqCst), admitted);
    assert!(maximum.load(Ordering::SeqCst) <= WORKERS);
    eprintln!(
        "checkpoint burst: offered={HOT_BODIES} admitted={admitted} seeds={} max_active={}",
        seeds_captured.load(Ordering::SeqCst),
        maximum.load(Ordering::SeqCst)
    );
}

fn read_snapshot_record_scale(total: u32) {
    use crate::replica::ReadSnapshot;

    const MAX_RSS_BYTES_PER_BODY: usize = 4 * 1024;
    const MAX_STARTUP: std::time::Duration = std::time::Duration::from_secs(60);

    // Manufacture one real collaborative export, then give every record its
    // own Arc-backed copy. Equal facts are deliberately not shared here: cold
    // recovery currently obtains one verified Artifact/BodySnapshot per Body,
    // so sharing the template Arc would understate production residency.
    let template_key = fabric::Key::from_bytes(b"record-template".to_vec());
    let mut template_engine = fabric::Engine::new();
    template_engine
        .commit(fabric::Transaction::new(
            "record-template",
            vec![
                fabric::Op::CreateBody {
                    key: template_key.clone(),
                },
                fabric::Op::RegisterSet {
                    key: template_key.clone(),
                    path: "kind".to_owned(),
                    value: b"relation".to_vec(),
                },
            ],
        ))
        .expect("template commit");
    let template = template_engine
        .export_body(&template_key)
        .expect("collaborative export");
    let export_bytes = match &template {
        fabric::BodyExport::Collaborative(bytes) => bytes.len(),
        fabric::BodyExport::Atomic(_) => panic!("template must be collaborative"),
    };
    drop(template_engine);

    let binding = collab_binding();
    let before = resident_bytes();
    let started = std::time::Instant::now();
    let snapshot = ReadSnapshot::from_body_rows_for_test((0..total).map(|number| {
        let mut body_id = [0u8; 16];
        body_id[..4].copy_from_slice(&number.to_be_bytes());
        let key = BodyKey::new(world(), BodyId::from_bytes(body_id));
        let body = fabric::BodySnapshot::from_export(&template_key, template.clone())
            .expect("valid collaborative image");
        (key, binding.clone(), number.to_be_bytes().to_vec(), body)
    }));
    let elapsed = started.elapsed();
    let after = resident_bytes();
    let rss = after.saturating_sub(before);
    let rss_per_body = rss / usize::try_from(total).expect("body count");

    assert_eq!(snapshot.body_count(), u64::from(total));
    assert!(
        elapsed <= MAX_STARTUP,
        "record read-image startup regressed: {elapsed:?} > {MAX_STARTUP:?}"
    );
    if rss != 0 {
        assert!(
            rss_per_body <= MAX_RSS_BYTES_PER_BODY,
            "record read-image RSS/Body regressed: {rss_per_body} > {MAX_RSS_BYTES_PER_BODY}"
        );
    }
    eprintln!(
        "read-snapshot-record-scale bodies={total} export_bytes={export_bytes} startup_ms={} rss_mib={:.1} rss_bytes_per_body={rss_per_body}",
        elapsed.as_millis(),
        rss as f64 / (1024.0 * 1024.0),
    );
}

#[test]
fn schema_body_pages_merge_versions_without_skip_or_duplicate() {
    use crate::replica::ReadSnapshot;

    let fabric_key = fabric::Key::from_bytes(b"schema-page".to_vec());
    let snapshot = ReadSnapshot::from_body_rows_for_test((0u8..7).map(|number| {
        let key = BodyKey::new(world(), BodyId::from_bytes([number; 16]));
        let mut binding = atomic_binding();
        binding.schema = SchemaId::parse("paged").unwrap();
        binding.schema_version = 1 + u32::from(number % 2);
        let body = fabric::BodySnapshot::from_export(
            &fabric_key,
            fabric::BodyExport::Atomic(vec![number]),
        )
        .unwrap();
        (key, binding, vec![number], body)
    }));
    let schema = SchemaId::parse("paged").unwrap();
    let first = snapshot.body_keys_page_with_schema(&world(), &schema, None, 3);
    assert_eq!(first.len(), 3);
    let second = snapshot.body_keys_page_with_schema(&world(), &schema, first.last(), 3);
    let third = snapshot.body_keys_page_with_schema(&world(), &schema, second.last(), 3);
    let mut all = first;
    all.extend(second);
    all.extend(third);
    assert_eq!(all.len(), 7);
    assert!(all.windows(2).all(|pair| pair[0] < pair[1]));
}

#[test]
#[ignore = "release-scale one-record-per-Body ReadSnapshot residency fixture"]
fn read_snapshot_100k_record_bodies() {
    read_snapshot_record_scale(100_000);
}

#[test]
#[ignore = "release-scale one-record-per-Body ReadSnapshot residency fixture"]
fn read_snapshot_1m_record_bodies() {
    read_snapshot_record_scale(1_000_000);
}

fn incompressible_ascii(bytes: usize) -> String {
    let mut state = 0x9e37_79b9_7f4a_7c15u64;
    let mut out = String::with_capacity(bytes);
    for _ in 0..bytes {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        out.push(char::from(
            b'!' + u8::try_from(state % 90).expect("printable"),
        ));
    }
    out
}

#[test]
#[ignore = "release hot-Body durable publication and retained-generation fixture"]
fn a_one_megabyte_issue_stays_in_the_bounded_action_class_across_64_generations() {
    const TEXT_BYTES: usize = 1024 * 1024;
    const GENERATIONS: usize = 64;
    const MAX_ACTION: std::time::Duration = std::time::Duration::from_millis(500);
    const MAX_SNAPSHOT: std::time::Duration = std::time::Duration::from_millis(150);
    const MAX_RETAINED_RSS: usize = 256 * 1024 * 1024;

    let dir = temp_store("hot-issue");
    let mut replica = open(&dir);
    let key = body(77);
    let text = incompressible_ascii(TEXT_BYTES);
    let mut initial_ops = vec![(key.clone(), Op::Create)];
    for (chunk, bytes) in text.as_bytes().chunks(64 * 1024).enumerate() {
        initial_ops.push((
            key.clone(),
            Op::TextSplice {
                path: "description".to_owned(),
                index: u64::try_from(chunk * 64 * 1024).expect("chunk offset"),
                delete: 0,
                insert: std::str::from_utf8(bytes)
                    .expect("ASCII fixture")
                    .to_owned(),
            },
        ));
    }
    commit(
        &mut replica,
        [70u8; 16],
        "large-issue",
        &initial_ops,
        &[(key.clone(), collab_binding())],
    )
    .expect("large issue commit");

    let mut current = replica.read_snapshot();
    let mut retained = vec![current.clone()];
    let before = resident_bytes();
    let mut actions = Vec::with_capacity(GENERATIONS);
    let mut snapshots = Vec::with_capacity(GENERATIONS);
    for generation in 0..GENERATIONS {
        let mut request = [0u8; 16];
        request[..8].copy_from_slice(&(generation as u64 + 1).to_be_bytes());
        request[8] = 71;
        let action_started = std::time::Instant::now();
        commit(
            &mut replica,
            request,
            "single-scalar-edit",
            &[(
                (key.clone()),
                Op::TextSplice {
                    path: "description".to_owned(),
                    index: u64::try_from(generation).expect("index"),
                    delete: 1,
                    insert: if generation % 2 == 0 { "x" } else { "y" }.to_owned(),
                },
            )],
            &[],
        )
        .expect("durable scalar edit");
        actions.push(action_started.elapsed());

        let snapshot_started = std::time::Instant::now();
        current = replica.advance_read_snapshot(&current, std::slice::from_ref(&key));
        snapshots.push(snapshot_started.elapsed());
        retained.push(current.clone());
    }
    let after = resident_bytes();
    let retained_rss = after.saturating_sub(before);
    actions.sort();
    snapshots.sort();
    let action_p99 = actions[actions.len() * 99 / 100];
    let snapshot_p99 = snapshots[snapshots.len() * 99 / 100];
    assert!(
        action_p99 <= MAX_ACTION,
        "durable action p99={action_p99:?}"
    );
    assert!(
        snapshot_p99 <= MAX_SNAPSHOT,
        "snapshot publication p99={snapshot_p99:?}"
    );
    if retained_rss != 0 {
        assert!(
            retained_rss <= MAX_RETAINED_RSS,
            "64 retained generations used {:.1} MiB",
            retained_rss as f64 / (1024.0 * 1024.0)
        );
    }
    let view = current
        .read_collaborative(&key)
        .expect("current collaborative view");
    assert_eq!(view.texts["description"].len(), TEXT_BYTES);
    eprintln!(
        "hot-issue-durable text_mib=1 generations={} action_p99_us={} snapshot_p99_us={} retained_rss_mib={:.1}",
        retained.len(),
        action_p99.as_micros(),
        snapshot_p99.as_micros(),
        retained_rss as f64 / (1024.0 * 1024.0),
    );
    let _ = std::fs::remove_dir_all(&dir);
}
