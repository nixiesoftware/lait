//! [`Replica`] — the committing semantic layer over a Engine engine and the
//! canonical durable Body store.
//!
//! Replica translates a validated set of staged [`Op`]s into semantic
//! [`Op`]s, submits them to a Engine engine for an atomic apply, and
//! advances its semantic frontier **only** from the returned Engine receipt.
//! It never authors a raw document delta and never fabricates a receipt.
//!
//! **The canonical store.** A durable Replica persists — through the Engine
//! journal's six-step commit protocol, at one linearization point per
//! transaction — the canonical signed [`Transaction`] record whose descriptors
//! carry bounded Fabric [`CausalMaterial`] closures, the individually protected
//! artifact objects those closures name (`epoch_id[16] || nonce[12] ||
//! ciphertext_and_tag`; no plaintext Body payload is ever at rest), the
//! [`RequestReceipt`] idempotency record, and the signed Manifest root over the
//! full Body set. Recovery reopens exactly that graph: a
//! Body whose key-epoch material is locally held is opened, validated, and
//! imported into the engine; a Body whose epoch key is absent is retained
//! **opaquely** — byte-identical, never decrypted, absent from reads — until a
//! key legitimately arrives.
//!
//! **Convergence.** [`Replica::incorporate`] accepts only a signed
//! [`Transaction`] plus the exact protected artifacts its descriptors name:
//! mechanics validates the signer's standing at the transaction's referenced
//! authority frontier, every artifact must match a signed [`ArtifactRef`], and
//! only then does material reach the engine — per Body, via
//! [`fabric::Engine::import_artifact`], never as a raw engine snapshot. Supported
//! material becomes exact per-Body Engine changes; unsupported-but-legitimate
//! material (unknown World/schema, or no local key) is retained opaquely and
//! forwarded byte-identically. Body-level tombstones are local retirement:
//! cross-replica deletion is application state inside a Body, so a tombstoned
//! Body simply leaves this Replica's manifest.

use std::borrow::Borrow;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex, MutexGuard, OnceLock};

use fabric::{
    commit::Failure as EngineFailure, Artifact, ArtifactRef, BodyExport, CausalRelation,
    CheckpointPolicy, Engine, Key, Material as CausalMaterial, Version as CausalVersion,
    CAUSAL_FORMAT_VERSION,
};
use journal::{Object, Store};
use mechanics::authorization::{AuthorizedBodyKey, BODY_ENVELOPE_OVERHEAD, BODY_EPOCH_ID_LEN};
use mechanics::ids::SpaceId;
use serde::{Deserialize, Serialize};

use crate::algebra;
use crate::body::Op;
use crate::convergence::ConvergenceOutcome;
use crate::frontier::{AuthorityFrontier, ReplicaFrontier};
use crate::ids::{BodyKey, EncodingId, SchemaId, WorldId};
use crate::manifest::{body_index_key, ManifestEntry, ManifestHead, ManifestRoot};
use crate::protected::{
    open_artifact, seal_artifact, BodyKeySource, Invalid as BodyInvalid, MAX_BODY_BYTES,
};
use crate::receipt::RequestReceipt;
use crate::transaction::{AuthoritySource, Core, Descriptor, SignRequest, Signer, Transaction};

pub mod generation;

/// Domain separator for deriving a Engine key from a Body key.
const BODY_KEY_DOMAIN: &[u8] = b"lait/fabric-key/1";
/// Domain separator for advancing the semantic frontier from a commit receipt.
const FRONTIER_DOMAIN: &[u8] = b"lait/replica-frontier/1";
/// Domain separator for advancing a Body's chain frontier from a transaction.
const BODY_CHAIN_DOMAIN: &[u8] = b"lait/body-chain/1";

struct StoreNodes<'a>(&'a Store);

impl crate::index::NodeSource for StoreNodes<'_> {
    fn node(&self, hash: &[u8; 32]) -> Option<Vec<u8>> {
        self.0.read(hash).ok()
    }
}

struct ReaderNodes<'a>(&'a journal::Reader);

impl crate::index::NodeSource for ReaderNodes<'_> {
    fn node(&self, hash: &[u8; 32]) -> Option<Vec<u8>> {
        self.0.read(hash).ok()
    }
}

/// The mutation-model tags shared with [`crate::protected`].
pub use crate::protected::{MUTATION_ATOMIC, MUTATION_COLLABORATIVE, MUTATION_IMMUTABLE_ATOMIC};

/// Why a Replica commit failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// A staged operation is not supported by the current engine (the in-memory
    /// reference engine is atomic-only).
    UnsupportedOp,
    /// An operation's path violates the frozen path grammar.
    PathInvalid,
    /// An operation exceeds a frozen algebra limit (value/key/insert size).
    OpLimit,
    /// The Body reached the protocol's emergency causal-tail envelope while
    /// its already-enqueued checkpoint maintenance was unavailable. The edit
    /// was not committed. This is fast, retryable backpressure; the action
    /// path never serializes the full Body to escape the bound.
    CheckpointBackpressure,
    /// The operation's type conflicts with what its target is already bound to
    /// (atomic vs collaborative Body, or a second collaborative type at a
    /// bound path).
    TypeConflict,
    /// The operation was structurally invalid at apply time (out-of-bounds
    /// index, unknown element id, counter overflow). Nothing was committed.
    InvalidOp(fabric::commit::Invalid),
    /// A staged operation addressed a Body whose immutable schema binding
    /// disagrees with the declared binding. Nothing was committed.
    SchemaMismatch,
    /// A create-once atomic Body was addressed by a non-canonical id, carried
    /// different bytes under its established address, or was subjected to an
    /// operation other than its one canonical replacement. Nothing was
    /// committed or incorporated.
    ImmutableConflict,
    /// Incoming material failed legitimacy validation (signature, signer
    /// authority, or payload binding). Nothing was incorporated.
    Illegitimate(Invalid),
    /// The same, for Contact material, carrying **why**.
    ///
    /// `Invalid` is `Copy` and deliberately coarse, and `From<String> for
    /// Invalid` throws the description away — so thirteen distinct refusals
    /// inside `validate_contact` all surfaced as `Illegitimate(Binding)`, and
    /// `Binding` did not even mean a binding check had failed. It was the sink
    /// every string-described refusal fell into.
    ///
    /// That is the wall this variant exists to remove. A replica that refuses
    /// everything a peer sends is otherwise indistinguishable from one that
    /// refuses it for any of: a duplicated transaction id, a manifest that does
    /// not parse, material offered without a manifest advertisement, a payload
    /// outside the advertised manifest, a payload that does not match its signed
    /// commitment, or a transaction from another Space.
    IllegitimateContact { kind: Invalid, reason: String },
    /// The mechanics authorizer refused to produce an authorization receipt
    /// for a local commit. The refusal is carried whole because its variants
    /// name entirely different problems — a real standing denial, an inactive
    /// implementation, a malformed demand (a World bug), or a ledger that
    /// could not evaluate at all — and a caller that cannot see which will
    /// phrase every one of them as "you lack write standing". Nothing
    /// committed.
    Unauthorized(mechanics::authorization::Refusal),
    /// A referenced parent Manifest is not locally reconstructable; retry once
    /// the exact material arrives. Never falls back to current state.
    ParentManifestUnavailable,
    /// A prior pre-v1 store contains signed whole-Body heads which cannot be
    /// translated into current causal descriptors without a real World actor,
    /// authorization demand, and new signed transactions. The validated prior
    /// source is intact; composition must run the semantic migration step into
    /// a fresh target before activation.
    NeedsSemanticMigration { bodies: u64 },
    /// The durable store failed integrity validation on open — never repaired
    /// heuristically; recreation guidance is the caller's.
    Integrity(Defect),
    /// A derived-generation operation failed integrity validation, retaining
    /// both the failed operation and the concrete lower-layer cause.
    IntegrityCause {
        defect: Defect,
        operation: &'static str,
        reason: String,
    },
    /// The Engine engine failed to apply the transaction.
    Engine(fabric::commit::Failure),
    /// No authorized key material is held for sealing new local material.
    /// Nothing was committed.
    BodyKeyUnavailable,
    /// The durable write of the committed state failed. The acknowledged
    /// frontier did not advance, and the Replica is poisoned (fail-stop) so the
    /// diverged in-memory representation can never acknowledge further commits.
    Durability(journal::Failure),
    /// The durable commit's authoritative switch happened but its durability
    /// confirmation failed: the outcome is unknown until the store is reopened
    /// (recovery resolves it from the on-disk manifest). The Replica is
    /// poisoned; NEVER retry the operation through this error — reopen and
    /// re-query instead, or a durably applied operation could be duplicated.
    OutcomeUnknown,
    /// A previous durability failure poisoned this Replica; reopen from the
    /// durable store.
    Poisoned,
    /// A request id was reused with a different payload hash. Nothing was
    /// committed; the original receipt is untouched.
    RequestIdConflict,
    /// An immutable receipt-absence proof was minted at a different durable
    /// commit point or for a different idempotency scope. The caller must
    /// recheck through a fresh [`ReceiptReader`]; no mutation was prepared.
    ReceiptCheckStale,
    /// The application effect exceeded [`crate::receipt::MAX_EFFECT_BYTES`].
    /// Nothing was committed.
    EffectTooLarge,
    /// The Space material quota (bytes or Body count) would be exceeded.
    /// Nothing was committed and no staging was retained.
    QuotaExceeded,
    /// Another local or Contact mutation already owns the bounded preparation
    /// lane. Callers must surface this promptly as Pending/Busy; they may not
    /// wait invisibly behind size-proportional extraction or publication work.
    MutationBusy,
    /// The unknown-World retention subquota would be exceeded. No eviction is
    /// performed, neither manifest nor frontier changes, and no staging
    /// objects are retained.
    OpaqueQuotaExceeded,
    /// A Body-owned identity or transaction seed could not obtain entropy.
    Body(crate::body::Failure),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Invalid {
    Signature,
    Binding,
    Encoding,
    Index,
    Space,
    IncompleteMaterial,
    UnbackedContent,
}

impl From<&str> for Invalid {
    fn from(_: &str) -> Self {
        Self::Binding
    }
}

impl From<String> for Invalid {
    fn from(_: String) -> Self {
        Self::Binding
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Defect {
    Store(journal::Defect),
    Encoding,
    Index,
    MissingMaterial,
    CorruptMaterial,
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for Failure {}

fn integrity_cause(
    defect: Defect,
    operation: &'static str,
    error: impl std::fmt::Debug,
) -> Failure {
    Failure::IntegrityCause {
        defect,
        operation,
        reason: format!("{error:?}"),
    }
}

fn annotate_integrity(
    failure: Failure,
    operation: &'static str,
    reason: impl std::fmt::Debug,
) -> Failure {
    match failure {
        Failure::Integrity(defect) => integrity_cause(defect, operation, reason),
        other => other,
    }
}

/// The outcome of committing a request through the persistent-idempotency
/// scope: either a fresh commit or a replay of the original receipt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActionOutcome {
    /// The request committed now; the receipt records its result.
    Committed(RequestReceipt),
    /// The identical request had already committed; the original receipt is
    /// returned and **nothing was reapplied**.
    Replayed(RequestReceipt),
}

/// The result of preparing an idempotent local action. A replay never opens a
/// candidate transaction; a fresh request is represented by an RAII guard
/// whose live Replica reads expose the candidate until it is finalized or
/// dropped.
pub enum PreparedActionOutcome {
    Prepared(PreparedAction),
    Replayed(RequestReceipt),
}

impl PreparedActionOutcome {
    /// The original receipt for a replay, or the candidate receipt for a fresh
    /// request.
    pub fn receipt(&self) -> Result<&RequestReceipt, Failure> {
        match self {
            Self::Prepared(prepared) => prepared.receipt(),
            Self::Replayed(receipt) => Ok(receipt),
        }
    }
}

/// One locally prepared action detached from the Replica metadata writer.
///
/// The owning Runtime retains one try-admitted mutation-lane permit while this
/// value exists, but releases the Replica mutex before snapshot extraction and
/// Corpus construction. The exact parent coordinate is compared again when
/// finalizing. Dropping the value rolls the Fabric candidate back through its
/// independent writer cell and releases the in-flight marker.
pub struct PreparedAction {
    fabric: Arc<Mutex<Engine>>,
    in_flight: Arc<AtomicBool>,
    rollback_poisoned: Arc<AtomicBool>,
    parent_root: [u8; 32],
    parent_frontier: ReplicaFrontier,
    snapshot: PreparedSnapshotContext,
    state: Option<PreparedActionState>,
}

struct PreparedSnapshotContext {
    durable: Option<journal::Reader>,
    keys: Option<Arc<dyn BodyKeySource>>,
    content_index_root: Option<IndexRef>,
    declaration_counts: BTreeMap<[u8; 32], u64>,
}

struct PreparationClaim {
    flag: Arc<AtomicBool>,
    armed: bool,
}

impl PreparationClaim {
    fn acquire(flag: &Arc<AtomicBool>) -> Result<Self, Failure> {
        flag.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| Failure::MutationBusy)?;
        Ok(Self {
            flag: Arc::clone(flag),
            armed: true,
        })
    }

    fn transfer(mut self) -> Arc<AtomicBool> {
        self.armed = false;
        Arc::clone(&self.flag)
    }
}

impl Drop for PreparationClaim {
    fn drop(&mut self) {
        if self.armed {
            self.flag.store(false, Ordering::Release);
        }
    }
}

fn lock_fabric(fabric: &Arc<Mutex<Engine>>) -> MutexGuard<'_, Engine> {
    fabric
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

enum PreparedActionState {
    Noop {
        receipt: RequestReceipt,
    },
    Mutation {
        fabric: fabric::Prepared,
        data: PreparedMutation,
    },
}

struct PreparedMutation {
    new_records: BTreeMap<BodyKey, Option<BodyRecord>>,
    sealed: Vec<(BodyKey, Vec<u8>, CausalMaterial)>,
    transaction: Option<Transaction>,
    receipt: RequestReceipt,
    next_frontier: ReplicaFrontier,
    declared: BTreeMap<BodyKey, Vec<[u8; 32]>>,
    candidate_root: [u8; 32],
    manifest_space: SpaceId,
    manifest_authority_frontier: AuthorityFrontier,
    manifest_signer: [u8; 32],
}

impl ActionOutcome {
    /// The receipt either way.
    pub fn receipt(&self) -> &RequestReceipt {
        match self {
            ActionOutcome::Committed(r) | ActionOutcome::Replayed(r) => r,
        }
    }
}

/// A Body's immutable schema binding, established at create and never changed
/// implicitly by a later write.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BodyBinding {
    pub schema: SchemaId,
    pub schema_version: u32,
    pub encoding: EncodingId,
    /// [`MUTATION_ATOMIC`], [`MUTATION_IMMUTABLE_ATOMIC`], or
    /// [`MUTATION_COLLABORATIVE`].
    pub mutation_model: u8,
}

type IncorporationUnit = (Transaction, Vec<(BodyKey, Vec<u8>)>);

/// One exported unit per retained transaction: the signed record plus its
/// per-Body canonical packs of protected artifact envelopes, byte-identical to
/// what was committed or incorporated.
pub type ExportedMaterial = Vec<(Transaction, Vec<(BodyKey, Vec<u8>)>)>;

/// The Space material quotas, enforced transactionally under the Replica
/// writer. The ledger counts canonical material bytes — protected Body
/// envelopes, distinct transaction records, and idempotency receipts;
/// manifests and journal bookkeeping are derived from ledgered material and
/// bounded proportionally by it. Operator configuration may **lower** any
/// protocol maximum but never raise it, and the configured limits persist in
/// the store meta so a restart cannot accidentally increase capacity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuotaConfig {
    /// Per-Body protected envelope maximum (protocol max 64 MiB).
    pub max_body_bytes: u64,
    /// Per-Space material bytes (protocol max 16 TiB).
    pub max_space_bytes: u64,
    /// Per-Space Body count (protocol max 10,000,000).
    pub max_space_bodies: u64,
    /// Retained-unknown-World material bytes, logical per World (1 GiB).
    pub max_unknown_world_bytes: u64,
    /// Retained-unknown-World Body count, logical per World (25,000).
    pub max_unknown_world_bodies: u64,
}

impl Default for QuotaConfig {
    fn default() -> Self {
        Self {
            max_body_bytes: u64::try_from(MAX_BODY_BYTES).unwrap_or(u64::MAX),
            max_space_bytes: 16 * 1024 * 1024 * 1024 * 1024,
            max_space_bodies: 10_000_000,
            max_unknown_world_bytes: 1024 * 1024 * 1024,
            max_unknown_world_bodies: 25_000,
        }
    }
}

impl QuotaConfig {
    /// Clamp every limit to its protocol maximum (lowering is allowed,
    /// raising is not).
    pub fn clamped(self) -> Self {
        let max = Self::default();
        Self {
            max_body_bytes: self.max_body_bytes.min(max.max_body_bytes),
            max_space_bytes: self.max_space_bytes.min(max.max_space_bytes),
            max_space_bodies: self.max_space_bodies.min(max.max_space_bodies),
            max_unknown_world_bytes: self
                .max_unknown_world_bytes
                .min(max.max_unknown_world_bytes),
            max_unknown_world_bodies: self
                .max_unknown_world_bodies
                .min(max.max_unknown_world_bodies),
        }
    }
}

/// The commit attribution a durable transaction is signed with: the Space, the
/// committing device's signing capability, and the authority frontier the
/// request was authorized at.
pub struct CommitContext<'a> {
    pub space: &'a SpaceId,
    pub signer: &'a dyn Signer,
    pub authority_frontier: AuthorityFrontier,
}

/// The World-authorization inputs a local durable commit binds into its signed
/// transaction: the acting principal, the parent Manifest root the request was
/// authored against, the canonical demand, the intent digest, and the
/// mechanics authorizer that — given the built core digest — produces the
/// canonical [`mechanics::authorization::AuthorizationReceipt`] bytes (or a typed
/// denial). Incorporation carries fully-formed transactions and needs none of
/// this.
pub struct CommitAuthorization<'a> {
    pub actor: &'a str,
    pub parent_manifest_root: [u8; 32],
    pub demand: Vec<u8>,
    pub intent_digest: [u8; 32],
    pub authorizer: &'a dyn TransactionAuthorizer,
}

/// The mechanics seam that turns a built transaction core into a signed
/// authorization receipt. It reads the coordinates the core already carries
/// (actor, device, Space, frontier, parent root, demand, intent/operation
/// digests) and adds the World-scoped facts (implementation id, checkpoint
/// commitment, policy evidence). The composition root implements it over the
/// authority ledger; a denial is a typed `Err`, never a receipt.
pub trait TransactionAuthorizer {
    fn authorize(&self, core: &Core) -> Result<Vec<u8>, mechanics::authorization::Refusal>;
}

/// A self-contained [`TransactionAuthorizer`] that builds a structurally-valid
/// authorization receipt without a real policy history — for fixtures and the
/// non-durable/keyed local path where no ledger is present. A real deployment
/// uses a ledger-backed authorizer that also evaluates the demand at the
/// pinned frontier.
pub struct StaticAuthorizer {
    pub world: WorldId,
    pub implementation_id: [u8; 32],
}

impl TransactionAuthorizer for StaticAuthorizer {
    fn authorize(&self, core: &Core) -> Result<Vec<u8>, mechanics::authorization::Refusal> {
        let space = std::str::from_utf8(&core.space)
            .map_err(|_| {
                mechanics::authorization::Refusal::Denied(
                    mechanics::authorization::DenialReason::Internal("space bytes are not UTF-8"),
                )
            })?
            .to_string();
        let demand = mechanics::authorization::AuthorizationDemand::decode_canonical(&core.demand)
            .map_err(mechanics::authorization::Refusal::Demand)?;
        let receipt = mechanics::authorization::AuthorizationReceipt {
            space,
            world: self.world.as_str().to_string(),
            actor: core.actor.clone(),
            device: core.signer,
            authority_frontier: core.authority_frontier.as_bytes().to_vec(),
            authority_checkpoint_commitment: [0u8; 32],
            policy_evidence_digest: mechanics::authorization::policy_evidence_digest(&[]),
            parent_manifest_root: core.parent_manifest_root,
            implementation_id: self.implementation_id,
            intent_digest: core.intent_digest,
            demand_digest: demand
                .digest()
                .map_err(mechanics::authorization::Refusal::Demand)?,
            effect_operations_digest: core.operations_digest,
            body_transaction_core_digest: core.digest(),
            decision: 1,
        };
        Ok(receipt.encode())
    }
}

/// The schemas locally supported for interpreting remote material, declared
/// from the runtime's World registry. Anything not declared here takes the
/// opaque-retention branch during Convergence.
#[derive(Debug, Clone, Default)]
pub struct SupportedSchemas {
    entries: BTreeMap<(WorldId, SchemaId, u32), (EncodingId, u8)>,
}

impl SupportedSchemas {
    pub fn new() -> Self {
        Self::default()
    }
    /// Declare a supported `(world, schema, version)` with its encoding and
    /// mutation-model tag.
    pub fn declare(
        &mut self,
        world: WorldId,
        schema: SchemaId,
        version: u32,
        encoding: EncodingId,
        mutation_model: u8,
    ) {
        self.entries
            .insert((world, schema, version), (encoding, mutation_model));
    }
    pub fn lookup(
        &self,
        world: &WorldId,
        schema: &SchemaId,
        version: u32,
    ) -> Option<&(EncodingId, u8)> {
        self.entries.get(&(world.clone(), schema.clone(), version))
    }
}

/// One constituent **head** of a Body: a single author-signed transaction and
/// its protected artifact closure. A Body converged from concurrent writers carries
/// several heads whose engine-merged union is the current state — every byte
/// that ever crosses a wire or lands durable is one author's original signed
/// material; a replica never re-signs what it merged. A local commit collapses
/// the set back to one head whose signed descriptor names the merged state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct BodyHead {
    /// The id (full signed-envelope digest) of this head's transaction —
    /// the export-grouping key.
    tx: [u8; 32],
    /// Hash of this head's descriptor (manifest entry input).
    descriptor_hash: [u8; 32],
    /// Commitment to this head's signed transaction bytes.
    tx_commitment: [u8; 32],
    /// Protected Fabric artifact objects in signed descriptor order when this
    /// head is one member of a concurrent set. `None` is the overwhelmingly
    /// common singleton form: its exact closure is the record's authoritative
    /// causal Material, avoiding a second copy of the checkpoint/tail refs.
    /// `Some(empty)` is the distinct local-only/unsealed form.
    artifacts: Option<Box<[ArtifactRef]>>,
    /// The signed transaction record object (durable stores only).
    transaction: Option<Object>,
    /// Total protected artifact bytes named by this head (quota ledger input).
    artifact_bytes: u64,
    /// The signed transaction record length (quota ledger input; distinct
    /// transactions count once).
    tx_len: u64,
}

/// One Body's record in the Replica index.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct BodyRecord {
    binding: BodyBinding,
    /// The per-Body chain frontier: a commitment to this Body's transaction
    /// chain (root, height). Atomic concurrent writes resolve to the
    /// deterministic maximum of `(height, root)`; collaborative chains are
    /// bookkeeping (the engine's causal merge is authoritative).
    chain: ReplicaFrontier,
    /// The constituent heads (never empty). One after any local commit;
    /// several while concurrent remote writes are held merged but not yet
    /// re-sealed by a local write.
    heads: smallvec::SmallVec<[BodyHead; 1]>,
    /// Whether the Body is interpreted by the local engine. `false` is the
    /// opaque branch: retained byte-identically, absent from reads.
    interpreted: bool,
    /// The protected causal artifact closure for the current interpreted
    /// state. It is local durable material, not part of the signed peer head:
    /// the interchange protocol still carries the author's original envelope.
    /// Ordinary edits extend this bounded descriptor by one delta reference.
    /// The one-time indexed-v2 baseline migration decodes its prior record
    /// shape explicitly and initializes this to `None`.
    causal: Option<Arc<CausalMaterial>>,
}

/// Borrowed exact artifact closure for one Body head without materializing a
/// temporary Vec. Singleton heads traverse their shared causal Material;
/// concurrent heads traverse the explicit signed closure retained per head.
struct BodyArtifactRefs<'a> {
    first: Option<&'a ArtifactRef>,
    rest: std::slice::Iter<'a, ArtifactRef>,
}

impl<'a> Iterator for BodyArtifactRefs<'a> {
    type Item = &'a ArtifactRef;

    fn next(&mut self) -> Option<Self::Item> {
        self.first.take().or_else(|| self.rest.next())
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let len = usize::from(self.first.is_some()).saturating_add(self.rest.len());
        (len, Some(len))
    }
}

impl ExactSizeIterator for BodyArtifactRefs<'_> {}

/// The mutable Replica's publication-independent Body directory.
///
/// General-purpose `BTreeMap` nodes made each small record pay a separate
/// pointer-rich tree allocation. Dense immutable leaves retain one Arc to each
/// record and path-copy at most 256 cheap Arc handles for an edit; the record's
/// head/material vectors are never deep-cloned by directory maintenance.
#[derive(Debug, Clone, Default)]
struct RecordDirectory {
    entries: SnapshotDirectory<BodyKey, Arc<BodyRecord>>,
    retained_bytes: u64,
}

type ReceiptRecord = (RequestReceipt, Option<Object>);

/// Persistent idempotency receipts in bounded dense leaves.
///
/// A receipt is looked up by an arbitrary canonical scope byte string, but it
/// is otherwise immutable. A general-purpose `BTreeMap` paid one tree node and
/// allocator object per receipt and made a record-shaped World retain a second
/// pointer-rich million-entry catalog beside its durable receipt index. Dense
/// leaves preserve logarithmic lookup and path-copy only the touched leaf.
#[derive(Debug, Clone, Default)]
struct ReceiptDirectory {
    entries: SnapshotDirectory<Vec<u8>, Arc<ReceiptRecord>>,
    retained_bytes: u64,
}

impl ReceiptDirectory {
    fn len(&self) -> usize {
        self.entries.len()
    }

    fn get(&self, key: &[u8]) -> Option<&ReceiptRecord> {
        self.entries.get(key).map(Arc::as_ref)
    }

    fn insert(&mut self, key: Vec<u8>, record: ReceiptRecord) -> Option<ReceiptRecord> {
        let retained = receipt_record_retained_estimate(&key, &record.0);
        let prior_retained = self
            .entries
            .get_key_value(key.as_slice())
            .map(|(scope, prior)| receipt_record_retained_estimate(scope, &prior.0))
            .unwrap_or(0);
        let prior = self.entries.insert(key, Arc::new(record));
        self.retained_bytes = self
            .retained_bytes
            .saturating_sub(prior_retained)
            .saturating_add(retained);
        prior.map(|prior| Arc::try_unwrap(prior).unwrap_or_else(|prior| (*prior).clone()))
    }

    fn remove(&mut self, key: &[u8]) -> Option<ReceiptRecord> {
        let prior_retained = self
            .entries
            .get_key_value(key)
            .map(|(scope, prior)| receipt_record_retained_estimate(scope, &prior.0))?;
        let prior = self.entries.remove(key)?;
        self.retained_bytes = self.retained_bytes.saturating_sub(prior_retained);
        Some(Arc::try_unwrap(prior).unwrap_or_else(|prior| (*prior).clone()))
    }

    fn values(&self) -> impl Iterator<Item = &ReceiptRecord> {
        self.entries.iter().map(|(_, record)| record.as_ref())
    }

    const fn retained_bytes_estimate(&self) -> u64 {
        self.retained_bytes
    }
}

const HOT_RECEIPT_CACHE: usize = 256;
const HOT_RECEIPT_CACHE_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Debug, Default)]
struct ReceiptCache {
    entries: ReceiptDirectory,
    order: VecDeque<Vec<u8>>,
}

impl ReceiptCache {
    fn get(&mut self, scope: &[u8]) -> Option<RequestReceipt> {
        let receipt = self.entries.get(scope)?.0.clone();
        if let Some(position) = self.order.iter().position(|held| held.as_slice() == scope) {
            self.order.remove(position);
        }
        self.order.push_back(scope.to_vec());
        Some(receipt)
    }

    fn insert(&mut self, scope: Vec<u8>, receipt: RequestReceipt, object: Object) {
        if let Some(position) = self.order.iter().position(|held| held == &scope) {
            self.order.remove(position);
        }
        self.entries.insert(scope.clone(), (receipt, Some(object)));
        self.order.push_back(scope);
        while self.order.len() > HOT_RECEIPT_CACHE
            || self.retained_bytes_estimate() > HOT_RECEIPT_CACHE_BYTES
        {
            if let Some(expired) = self.order.pop_front() {
                self.entries.remove(&expired);
            }
        }
    }

    fn retained_bytes_estimate(&self) -> u64 {
        self.order.iter().fold(
            self.entries.retained_bytes_estimate().saturating_add(
                u64::try_from(self.order.capacity())
                    .unwrap_or(u64::MAX)
                    .saturating_mul(
                        u64::try_from(std::mem::size_of::<Vec<u8>>()).unwrap_or(u64::MAX),
                    ),
            ),
            |total, scope| {
                total
                    .saturating_add(u64::try_from(scope.capacity()).unwrap_or(u64::MAX))
                    .saturating_add(16)
            },
        )
    }
}

fn receipt_record_retained_estimate(scope: &[u8], receipt: &RequestReceipt) -> u64 {
    const ALLOCATION_HEADER: u64 = 16;
    const DIRECTORY_AND_ALLOCATOR_SLACK: u64 = 96;
    u64::try_from(
        std::mem::size_of::<SnapshotDirectoryEntry<Vec<u8>, Arc<ReceiptRecord>>>()
            .saturating_add(std::mem::size_of::<ReceiptRecord>()),
    )
    .unwrap_or(u64::MAX)
    .saturating_add(u64::try_from(scope.len()).unwrap_or(u64::MAX))
    .saturating_add(u64::try_from(receipt.effect.capacity()).unwrap_or(u64::MAX))
    .saturating_add(
        u64::try_from(receipt.bodies.capacity())
            .unwrap_or(u64::MAX)
            .saturating_mul(u64::try_from(std::mem::size_of::<BodyKey>()).unwrap_or(u64::MAX)),
    )
    .saturating_add(receipt.bodies.iter().fold(0u64, |bytes, body| {
        bytes
            .saturating_add(u64::try_from(body.world.as_bytes().len()).unwrap_or(u64::MAX))
            .saturating_add(ALLOCATION_HEADER)
    }))
    .saturating_add(3 * ALLOCATION_HEADER)
    .saturating_add(DIRECTORY_AND_ALLOCATOR_SLACK)
}

fn declared_body_retained_estimate(refs: &Vec<[u8; 32]>) -> u64 {
    const ALLOCATION_HEADER: u64 = 16;
    const BTREE_NODE_AND_ALLOCATOR_SLACK: u64 = 192;
    u64::try_from(
        std::mem::size_of::<BodyKey>().saturating_add(std::mem::size_of::<Vec<[u8; 32]>>()),
    )
    .unwrap_or(u64::MAX)
    .saturating_add(ALLOCATION_HEADER)
    .saturating_add(
        u64::try_from(refs.capacity())
            .unwrap_or(u64::MAX)
            .saturating_mul(32),
    )
    .saturating_add(BTREE_NODE_AND_ALLOCATOR_SLACK)
}

const fn declared_count_retained_estimate() -> u64 {
    // Hash key + count plus a conservative BTree node/allocator share.
    32 + 8 + 160
}

const fn declared_world_retained_estimate() -> u64 {
    // `WorldId` is an `Arc<str>`, so this is a pointer and a refcount bump and
    // never a second copy of the name — plus a BTree node/allocator share.
    8 + 8 + 160
}

impl RecordDirectory {
    fn len(&self) -> usize {
        self.entries.len()
    }

    fn contains_key(&self, key: &BodyKey) -> bool {
        self.entries.get(key).is_some()
    }

    fn get(&self, key: &BodyKey) -> Option<&BodyRecord> {
        self.entries.get(key).map(Arc::as_ref)
    }

    fn insert(&mut self, key: BodyKey, record: BodyRecord) -> Option<BodyRecord> {
        let retained = body_record_retained_estimate(&record);
        let prior = self.entries.insert(key, Arc::new(record));
        if let Some(prior) = &prior {
            self.retained_bytes = self
                .retained_bytes
                .saturating_sub(body_record_retained_estimate(prior));
        }
        self.retained_bytes = self.retained_bytes.saturating_add(retained);
        prior.map(|prior| Arc::try_unwrap(prior).unwrap_or_else(|prior| (*prior).clone()))
    }

    fn remove(&mut self, key: &BodyKey) -> Option<BodyRecord> {
        let prior = self.entries.remove(key)?;
        self.retained_bytes = self
            .retained_bytes
            .saturating_sub(body_record_retained_estimate(&prior));
        Some(Arc::try_unwrap(prior).unwrap_or_else(|prior| (*prior).clone()))
    }

    fn iter(&self) -> impl Iterator<Item = (&BodyKey, &BodyRecord)> {
        self.entries
            .iter()
            .map(|(key, record)| (key, record.as_ref()))
    }

    fn keys(&self) -> impl Iterator<Item = &BodyKey> {
        self.entries.keys()
    }

    fn values(&self) -> impl Iterator<Item = &BodyRecord> {
        self.iter().map(|(_, record)| record)
    }

    const fn retained_bytes_estimate(&self) -> u64 {
        self.retained_bytes
    }
}

fn body_record_retained_estimate(record: &BodyRecord) -> u64 {
    const ALLOCATION_HEADER: u64 = 16;
    const DIRECTORY_AND_ALLOCATOR_SLACK: u64 = 160;
    let fixed = u64::try_from(
        std::mem::size_of::<SnapshotDirectoryEntry<BodyKey, Arc<BodyRecord>>>()
            .saturating_add(std::mem::size_of::<BodyRecord>()),
    )
    .unwrap_or(u64::MAX)
    .saturating_add(ALLOCATION_HEADER)
    .saturating_add(DIRECTORY_AND_ALLOCATOR_SLACK);
    let heads = record
        .heads
        .spilled()
        .then(|| {
            ALLOCATION_HEADER.saturating_add(
                u64::try_from(record.heads.capacity())
                    .unwrap_or(u64::MAX)
                    .saturating_mul(
                        u64::try_from(std::mem::size_of::<BodyHead>()).unwrap_or(u64::MAX),
                    ),
            )
        })
        .unwrap_or(0);
    let head_artifacts = record.heads.iter().fold(0u64, |bytes, head| {
        bytes.saturating_add(head.artifacts.as_ref().map_or(0, |artifacts| {
            ALLOCATION_HEADER.saturating_add(
                u64::try_from(artifacts.len())
                    .unwrap_or(u64::MAX)
                    .saturating_mul(
                        u64::try_from(std::mem::size_of::<ArtifactRef>()).unwrap_or(u64::MAX),
                    ),
            )
        }))
    });
    let material = record.causal.as_ref().map_or(0, |material| {
        ALLOCATION_HEADER
            .saturating_add(
                u64::try_from(std::mem::size_of::<CausalMaterial>()).unwrap_or(u64::MAX),
            )
            .saturating_add(ALLOCATION_HEADER)
            .saturating_add(
                u64::try_from(material.delta_tail.capacity())
                    .unwrap_or(u64::MAX)
                    .saturating_mul(
                        u64::try_from(std::mem::size_of::<ArtifactRef>()).unwrap_or(u64::MAX),
                    ),
            )
    });
    fixed
        .saturating_add(heads)
        .saturating_add(head_artifacts)
        .saturating_add(material)
}

impl BodyRecord {
    /// The primary head — the only head on every single-writer path.
    fn head(&self) -> Result<&BodyHead, Failure> {
        self.heads
            .first()
            .ok_or(Failure::Integrity(Defect::MissingMaterial))
    }
    fn head_mut(&mut self) -> Result<&mut BodyHead, Failure> {
        self.heads
            .first_mut()
            .ok_or(Failure::Integrity(Defect::MissingMaterial))
    }
    fn artifacts<'a>(&'a self, head: &'a BodyHead) -> BodyArtifactRefs<'a> {
        match &head.artifacts {
            Some(artifacts) => BodyArtifactRefs {
                first: None,
                rest: artifacts.iter(),
            },
            None => self.causal.as_ref().map_or(
                BodyArtifactRefs {
                    first: None,
                    rest: [].iter(),
                },
                |material| BodyArtifactRefs {
                    first: Some(&material.checkpoint),
                    rest: material.delta_tail.iter(),
                },
            ),
        }
    }
    fn promote_singleton_closure(&mut self) -> Result<(), Failure> {
        if self.heads.len() != 1
            || self
                .heads
                .first()
                .is_some_and(|head| head.artifacts.is_some())
        {
            return Ok(());
        }
        let material = self
            .causal
            .as_ref()
            .ok_or(Failure::Integrity(Defect::MissingMaterial))?;
        let artifacts = std::iter::once(material.checkpoint)
            .chain(material.delta_tail.iter().copied())
            .collect::<Vec<_>>()
            .into_boxed_slice();
        self.head_mut()?.artifacts = Some(artifacts);
        Ok(())
    }
    /// Use the compact singleton sentinel only when the signed head closure is
    /// exactly the record-wide causal closure. A receiver may reconstruct the
    /// same Fabric state under its own sealing material, yielding different
    /// protected object hashes; in that case the original signed descriptor
    /// refs must remain explicit so restart and re-export still verify it.
    fn compact_singleton_closure(&mut self) {
        if self.heads.len() != 1 {
            return;
        }
        let Some(material) = self.causal.as_ref() else {
            return;
        };
        let Some(head) = self.heads.first() else {
            return;
        };
        let Some(artifacts) = head.artifacts.as_deref() else {
            return;
        };
        let causal_len = 1usize.saturating_add(material.delta_tail.len());
        let matches = artifacts.len() == causal_len
            && artifacts.first() == Some(&material.checkpoint)
            && artifacts.get(1..) == Some(material.delta_tail.as_slice());
        if matches {
            if let Some(head) = self.heads.first_mut() {
                head.artifacts = None;
            }
        }
    }
    fn replace_causal(&mut self, causal: Option<Arc<CausalMaterial>>) -> Result<(), Failure> {
        // Freeze the old signed-head coordinate before replacing the derived
        // record-wide material. The replacement may be a checkpoint or a
        // locally re-sealed equivalent whose protected refs are different.
        self.promote_singleton_closure()?;
        self.causal = causal;
        self.compact_singleton_closure();
        Ok(())
    }
    /// Total protected artifact bytes across heads (quota ledger input).
    fn protected_total(&self) -> u64 {
        self.heads
            .iter()
            .fold(0u64, |a, h| a.saturating_add(h.artifact_bytes))
    }
    /// Whether some head carries this transaction commitment (already-known
    /// staged material).
    fn has_commitment(&self, commitment: &[u8; 32]) -> bool {
        self.heads.iter().any(|h| &h.tx_commitment == commitment)
    }
}

/// The store's opaque caller metadata, persisted with every commit at the
/// journal's manifest linearization point.
///
/// Every large map here is a *root*, not a vector. The shape this replaced
/// carried the complete Body catalog inline, so changing one Body re-encoded
/// and fsynced all of them — 28.8 MB at 100,000 Bodies, which is the whole
/// reason this docket exists. What is left is fixed-size.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoreMeta {
    format_version: u8,
    space: Option<SpaceId>,
    frontier: ReplicaFrontier,
    quota: QuotaConfig,
    /// `BodyKey` → the Body's record. Nodes live in the object store and are
    /// kept alive through the journal's `caller_index_roots`.
    body_index_root: Option<IndexRef>,
    /// The published catalog root the signed manifest commits to.
    manifest_body_root: Option<IndexRef>,
    /// The content catalog root the signed manifest commits to.
    content_index_root: Option<IndexRef>,
    /// Idempotency scope → the receipt object that answers a replay.
    receipt_index_root: Option<IndexRef>,
    /// Exact durable receipt ledger. The root is enough for lookup but not for
    /// quota admission: retaining the aggregate here keeps both open and a new
    /// action O(1) in the lifetime receipt count.
    receipt_count: u64,
    receipt_material_bytes: u64,
    /// Generation id → immutable changed-Body delta object. The delta objects
    /// themselves are retained requirements; this index makes ancestry and
    /// exact historical reconstruction logarithmically addressable.
    generation_index_root: Option<IndexRef>,
    /// Protected artifact digest -> canonical length/epoch + owner count.
    /// The Journal's deferred membership delta is derived from zero/first
    /// crossings in this authenticated index, never from an in-memory guess.
    ownership_index_root: Option<IndexRef>,
    manifest_root: Option<Object>,
}

/// The generation-journal format, version 3: [`StoreMeta`] without the two
/// receipt aggregates and the ownership root, each of which is derivable or
/// empty. A lossless input.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct PriorGenerationStoreMeta {
    format_version: u8,
    space: Option<SpaceId>,
    frontier: ReplicaFrontier,
    quota: QuotaConfig,
    body_index_root: Option<IndexRef>,
    manifest_body_root: Option<IndexRef>,
    content_index_root: Option<IndexRef>,
    receipt_index_root: Option<IndexRef>,
    generation_index_root: Option<IndexRef>,
    manifest_root: Option<Object>,
}

/// The indexed-catalog format, version 2 — one generation behind
/// [`PriorGenerationStoreMeta`]. It carries no generation journal root, so a
/// version-2 store opens by recording its current committed state as the first
/// complete generation baseline.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct PriorIndexedStoreMeta {
    format_version: u8,
    space: Option<SpaceId>,
    frontier: ReplicaFrontier,
    quota: QuotaConfig,
    body_index_root: Option<IndexRef>,
    manifest_body_root: Option<IndexRef>,
    content_index_root: Option<IndexRef>,
    receipt_index_root: Option<IndexRef>,
    manifest_root: Option<Object>,
}

/// The store meta's encoded generation.
const STORE_META_FORMAT_VERSION: u8 = 8;
/// The generation immediately behind [`STORE_META_FORMAT_VERSION`].
const PRIOR_GENERATION_STORE_META_FORMAT_VERSION: u8 = 3;
/// The oldest generation still read here.
const READABLE_STORE_META_FORMAT_VERSION: u8 = 2;

/// The version a store's metadata claims. First field of every generation, so
/// it reads even when the rest does not — which is what lets a refusal name
/// both figures.
fn claimed_store_meta_version(bytes: &[u8]) -> Option<u8> {
    #[derive(Deserialize)]
    struct Claim {
        format_version: u8,
    }
    postcard::take_from_bytes::<Claim>(bytes)
        .ok()
        .map(|(claim, _)| claim.format_version)
}

fn decode_store_meta(bytes: &[u8]) -> Result<(StoreMeta, bool), Failure> {
    if let Ok(meta) = postcard::from_bytes::<StoreMeta>(bytes) {
        if meta.format_version == STORE_META_FORMAT_VERSION {
            return Ok((meta, true));
        }
    }
    if let Ok(prior) = postcard::from_bytes::<PriorGenerationStoreMeta>(bytes) {
        if prior.format_version == PRIOR_GENERATION_STORE_META_FORMAT_VERSION {
            return Ok((
                StoreMeta {
                    format_version: STORE_META_FORMAT_VERSION,
                    space: prior.space,
                    frontier: prior.frontier,
                    quota: prior.quota,
                    body_index_root: prior.body_index_root,
                    manifest_body_root: prior.manifest_body_root,
                    content_index_root: prior.content_index_root,
                    receipt_index_root: prior.receipt_index_root,
                    receipt_count: prior.receipt_index_root.map_or(0, |root| root.count),
                    receipt_material_bytes: 0,
                    generation_index_root: prior.generation_index_root,
                    ownership_index_root: None,
                    manifest_root: prior.manifest_root,
                },
                false,
            ));
        }
    }
    let prior: PriorIndexedStoreMeta =
        postcard::from_bytes(bytes).map_err(|error| match claimed_store_meta_version(bytes) {
            Some(found) => Failure::IntegrityCause {
                defect: Defect::Encoding,
                operation: "decode store metadata",
                reason: format!(
                    "this store's metadata is version {found}; this build writes \
                     {STORE_META_FORMAT_VERSION} and reads \
                     {READABLE_STORE_META_FORMAT_VERSION}..={STORE_META_FORMAT_VERSION}. \
                     The data is not damaged — this build cannot interpret it."
                ),
            },
            None => integrity_cause(
                Defect::Encoding,
                "decode prior indexed store metadata",
                error,
            ),
        })?;
    if prior.format_version != READABLE_STORE_META_FORMAT_VERSION {
        return Err(Failure::IntegrityCause {
            defect: Defect::Encoding,
            operation: "decode store metadata",
            reason: format!(
                "this store's metadata is version {}; this build writes \
                 {STORE_META_FORMAT_VERSION} and reads \
                 {READABLE_STORE_META_FORMAT_VERSION}..={STORE_META_FORMAT_VERSION}.",
                prior.format_version
            ),
        });
    }
    Ok((
        StoreMeta {
            format_version: STORE_META_FORMAT_VERSION,
            space: prior.space,
            frontier: prior.frontier,
            quota: prior.quota,
            body_index_root: prior.body_index_root,
            manifest_body_root: prior.manifest_body_root,
            content_index_root: prior.content_index_root,
            receipt_index_root: prior.receipt_index_root,
            receipt_count: prior.receipt_index_root.map_or(0, |root| root.count),
            receipt_material_bytes: 0,
            generation_index_root: None,
            ownership_index_root: None,
            manifest_root: prior.manifest_root,
        },
        false,
    ))
}

type IndexRef = crate::index::ChildRef;

/// One Body's indexed entry. The key travels with the record because an index
/// key is a hash and cannot be inverted, and reopening needs the Body back.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct IndexedBody {
    key: BodyKey,
    record: BodyRecord,
}

/// One receipt's indexed entry, same reasoning.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct IndexedReceipt {
    scope: Vec<u8>,
    object: Object,
}

/// One Body replacement in a durable generation delta. Presence and
/// interpretability are explicit so typed readers never collapse an opaque
/// retained Body into absence. The descriptor names protected causal
/// artifacts rather than embedding a full plaintext Body export.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ArchivedBody {
    key: BodyKey,
    present: bool,
    interpreted: bool,
    binding: Option<BodyBinding>,
    stamp: Vec<u8>,
    material: Option<Arc<CausalMaterial>>,
}

/// The immutable material needed to replay one read generation from its
/// parent. Cost is proportional to the commit, never to the World.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct GenerationDelta {
    format_version: u8,
    root: [u8; 32],
    parent: Option<[u8; 32]>,
    frontier: ReplicaFrontier,
    changed: Vec<ArchivedBody>,
    descriptors: Vec<crate::content::ContentDescriptor>,
    removed_descriptors: Vec<[u8; 32]>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct IndexedGeneration {
    root: [u8; 32],
    object: Object,
    footprint: GenerationFootprint,
}

/// Authenticated pre-inflation admission metadata for one exact durable
/// generation. It is stored beside the generation root in the persistent
/// index, so lookup is O(log generations + source schemas), reads no delta or
/// Body object, and never prices historical work from mutable current state.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GenerationFootprint {
    pub body_count: u64,
    pub snapshot_retained_bytes: u64,
    /// Number of immutable generation deltas from this root to its baseline.
    pub reconstruction_depth: u32,
    /// Sum of the authenticated canonical delta-object lengths in that chain.
    pub reconstruction_delta_bytes: u64,
    /// Conservative peak bytes needed while `read_generation` retains the
    /// decoded chain and its selected-Body directory before installing the
    /// final immutable snapshot.
    pub reconstruction_transient_bytes: u64,
    pub sources: Vec<GenerationSourceFootprint>,
}

/// Exact readable source aggregate used with an implementation's declared
/// extractor shapes before historical reconstruction begins.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct GenerationSourceFootprint {
    pub world: WorldId,
    pub schema: SchemaId,
    pub version: u32,
    pub body_count: u64,
    pub payload_bytes: u64,
}

const MAX_GENERATION_SOURCES: usize = 4_096;
const MAX_GENERATION_RECONSTRUCTION_DEPTH: u32 = 1_048_576;
const GENERATION_DELTA_DECODE_MULTIPLIER: u64 = 4;
const GENERATION_DELTA_FIXED_TRANSIENT_BYTES: u64 = 1_024;
const GENERATION_CHANGED_BODY_TRANSIENT_BYTES: u64 = 1_024;

impl GenerationFootprint {
    fn validate(&self) -> Result<(), Failure> {
        let readable_bodies = self.sources.iter().try_fold(0u64, |total, source| {
            total
                .checked_add(source.body_count)
                .ok_or(Failure::Integrity(Defect::Encoding))
        })?;
        if self.sources.len() > MAX_GENERATION_SOURCES
            || self
                .sources
                .windows(2)
                .any(|pair| matches!(pair, [left, right] if left >= right))
            || self.sources.iter().any(|source| source.body_count == 0)
            || readable_bodies > self.body_count
            || (self.body_count == 0) != (self.snapshot_retained_bytes == 0)
            || self.reconstruction_depth > MAX_GENERATION_RECONSTRUCTION_DEPTH
            || (self.reconstruction_depth == 0)
                != (self.reconstruction_delta_bytes == 0
                    && self.reconstruction_transient_bytes == 0)
            || self.reconstruction_transient_bytes < self.reconstruction_delta_bytes
        {
            return Err(Failure::Integrity(Defect::Encoding));
        }
        Ok(())
    }

    fn record_generation_delta(
        &mut self,
        parent: Option<&Self>,
        delta: &GenerationDelta,
        encoded_len: u64,
    ) -> Result<(), Failure> {
        let parent_depth = parent.map_or(0, |parent| parent.reconstruction_depth);
        let parent_delta_bytes = parent.map_or(0, |parent| parent.reconstruction_delta_bytes);
        let parent_transient = parent.map_or(0, |parent| parent.reconstruction_transient_bytes);
        self.reconstruction_depth = parent_depth
            .checked_add(1)
            .filter(|depth| *depth <= MAX_GENERATION_RECONSTRUCTION_DEPTH)
            .ok_or(Failure::QuotaExceeded)?;
        self.reconstruction_delta_bytes = parent_delta_bytes
            .checked_add(encoded_len)
            .ok_or(Failure::QuotaExceeded)?;

        // `read_generation` keeps every decoded delta until it has selected
        // the visible Body rows. Canonical bytes account for all variable
        // strings/material vectors; the multiplier covers decoded capacities,
        // while the per-delta and per-Body terms cover Vec/BTreeMap nodes,
        // references, and allocator headers. The final ReadSnapshot is priced
        // separately by `snapshot_retained_bytes`.
        let changed = u64::try_from(delta.changed.len()).unwrap_or(u64::MAX);
        let delta_transient = encoded_len
            .checked_mul(GENERATION_DELTA_DECODE_MULTIPLIER)
            .and_then(|bytes| bytes.checked_add(GENERATION_DELTA_FIXED_TRANSIENT_BYTES))
            .and_then(|bytes| {
                changed
                    .checked_mul(GENERATION_CHANGED_BODY_TRANSIENT_BYTES)
                    .and_then(|changed| bytes.checked_add(changed))
            })
            .ok_or(Failure::QuotaExceeded)?;
        self.reconstruction_transient_bytes = parent_transient
            .checked_add(delta_transient)
            .ok_or(Failure::QuotaExceeded)?;
        self.validate()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct IndexedOwnership {
    object: Object,
    class: OwnedObjectClass,
    owners: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
enum OwnedObjectClass {
    Eager,
    DeferredArtifact { epoch: [u8; 16] },
    DeferredReceipt,
}

const GENERATION_DELTA_FORMAT_VERSION: u8 = 3;

#[cfg(test)]
pub(crate) fn is_canonical_generation_delta(bytes: &[u8]) -> bool {
    postcard::from_bytes::<GenerationDelta>(bytes).is_ok_and(|delta| {
        delta.format_version == GENERATION_DELTA_FORMAT_VERSION
            && postcard::to_stdvec(&delta).is_ok_and(|canonical| canonical == bytes)
    })
}

fn generation_index_key(root: &[u8; 32]) -> [u8; 32] {
    let mut hash = blake3::Hasher::new();
    hash.update(b"lait/replica/read-generation/1");
    hash.update(root);
    *hash.finalize().as_bytes()
}

fn ownership_index_key(hash: &[u8; 32]) -> [u8; 32] {
    let mut digest = blake3::Hasher::new();
    digest.update(b"lait/replica/object-ownership/1");
    digest.update(hash);
    *digest.finalize().as_bytes()
}

/// The index key a receipt scope sits under.
fn receipt_index_key(scope: &[u8]) -> [u8; 32] {
    let mut h = blake3::Hasher::new();
    h.update(b"lait/replica/receipt-scope/1");
    h.update(scope);
    *h.finalize().as_bytes()
}

/// Receipt effects are capped at 1 MiB and a transaction can name at most
/// 4,096 Bodies. Four MiB leaves generous canonical-coordinate overhead while
/// preventing a hostile authenticated length from becoming an unbounded
/// replay allocation.
const MAX_RECEIPT_OBJECT_BYTES: u64 = 4 * 1024 * 1024;

fn validate_receipt_for_storage(receipt: &RequestReceipt) -> Result<Vec<u8>, Failure> {
    let bytes = receipt.encode();
    if receipt.version != 2 {
        return Err(Failure::Integrity(Defect::Encoding));
    }
    if receipt.effect.len() > crate::receipt::MAX_EFFECT_BYTES {
        return Err(Failure::EffectTooLarge);
    }
    if receipt.bodies.len() > crate::transaction::MAX_DESCRIPTORS
        || u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_RECEIPT_OBJECT_BYTES
    {
        return Err(Failure::OpLimit);
    }
    if receipt
        .bodies
        .iter()
        .any(|body| body.world != receipt.world)
    {
        return Err(Failure::Illegitimate(
            "receipt Body lies outside its World".into(),
        ));
    }
    Ok(bytes)
}

fn validate_receipt_material(scope: &[u8], bytes: &[u8]) -> Result<RequestReceipt, Failure> {
    let receipt = RequestReceipt::decode_canonical(bytes)
        .map_err(|_| Failure::Integrity(Defect::Encoding))?;
    if receipt.scope_key().as_slice() != scope
        || receipt.bodies.len() > crate::transaction::MAX_DESCRIPTORS
        || receipt
            .bodies
            .iter()
            .any(|body| body.world != receipt.world)
        || u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_RECEIPT_OBJECT_BYTES
    {
        return Err(Failure::Integrity(Defect::Encoding));
    }
    Ok(receipt)
}

/// The Orbit's durable local materialization, over a Engine engine.
struct PreparedCheckpoint {
    base: Arc<CausalMaterial>,
    receiver: mpsc::Receiver<Option<(ArtifactRef, Vec<u8>)>>,
    #[cfg(test)]
    _held_sender: Option<mpsc::SyncSender<Option<(ArtifactRef, Vec<u8>)>>>,
}

pub(crate) type CheckpointWork = Box<dyn FnOnce() + Send + 'static>;

/// Snapshot construction is intentionally process-bounded rather than
/// Body-bounded. A large transaction can make thousands of Bodies cross the
/// soft watermark together; spawning one OS thread per Body would merely move
/// the publication cliff from serialization to scheduling. The fixed workers
/// consume a bounded queue, and a full queue is a retryable cache miss: the
/// touched Body is reconsidered on its next publication/incorporation.
pub(crate) struct CheckpointExecutor {
    sender: mpsc::SyncSender<CheckpointWork>,
    pub(crate) _workers: usize,
}

pub(crate) struct CheckpointReservation {
    sender: mpsc::SyncSender<CheckpointWork>,
}

const CHECKPOINT_WORKERS: usize = 2;
const CHECKPOINT_QUEUE_CAPACITY: usize = 64;

impl CheckpointExecutor {
    pub(crate) fn new(workers: usize, queue_capacity: usize) -> Self {
        let (sender, receiver) = mpsc::sync_channel::<CheckpointWork>(queue_capacity);
        let receiver = Arc::new(Mutex::new(receiver));
        let mut started = 0usize;
        for worker in 0..workers {
            let receiver = Arc::clone(&receiver);
            if std::thread::Builder::new()
                .name(format!("lait-checkpoint-{worker}"))
                .spawn(move || loop {
                    let work = receiver
                        .lock()
                        .ok()
                        .and_then(|receiver| receiver.recv().ok());
                    let Some(work) = work else {
                        break;
                    };
                    work();
                })
                .is_ok()
            {
                started = started.saturating_add(1);
            }
        }
        Self {
            sender,
            _workers: started,
        }
    }

    /// Reserve the complete seed+queue+export budget before any Body is
    /// inflated or cloned. A caller that gets `None` performs no checkpoint
    /// work and may retry on the Body's next publication.
    pub(crate) fn try_reserve(&self) -> Option<CheckpointReservation> {
        let (sender, receiver) = mpsc::sync_channel::<CheckpointWork>(1);
        let reserved: CheckpointWork = Box::new(move || {
            if let Ok(work) = receiver.recv() {
                work();
            }
        });
        self.sender
            .try_send(reserved)
            .ok()
            .map(|_| CheckpointReservation { sender })
    }
}

impl CheckpointReservation {
    pub(crate) fn submit(
        self,
        work: CheckpointWork,
    ) -> Result<(), mpsc::SendError<CheckpointWork>> {
        self.sender.send(work)
    }
}

fn checkpoint_executor() -> &'static CheckpointExecutor {
    static EXECUTOR: OnceLock<CheckpointExecutor> = OnceLock::new();
    EXECUTOR.get_or_init(|| CheckpointExecutor::new(CHECKPOINT_WORKERS, CHECKPOINT_QUEUE_CAPACITY))
}

pub struct Replica {
    /// Fabric's bounded mutation-hot writer set is independently synchronized
    /// so an owned prepared action can build its immutable read projection
    /// after releasing the much wider Replica metadata writer.
    fabric: Arc<Mutex<Engine>>,
    frontier: ReplicaFrontier,
    durable: Option<Store>,
    /// When this Replica's durable material was last verified end to end, in
    /// milliseconds since the unix epoch.
    ///
    /// `None` is not "verified long ago" — it is *nobody has ever checked*,
    /// which is the truthful state of a Replica that was never opened from a
    /// store. It stays `None` rather than becoming a zero: a figure nobody
    /// measured, drawn as a number, is the same defect as a failed peer sample
    /// drawn as "no peers", and harder to spot because it looks like data.
    verified_at_ms: Option<u64>,
    poisoned: bool,
    /// Shared with detached preparations so an RAII rollback failure remains
    /// fail-stop even though no `&mut Replica` is held at drop time.
    rollback_poisoned: Arc<AtomicBool>,
    /// Exactly one detached local/Contact candidate may exist. Contenders
    /// refuse promptly instead of waiting behind extractor or Corpus work.
    prepared_in_flight: Arc<AtomicBool>,
    keys: Option<Arc<dyn BodyKeySource>>,
    space: Option<SpaceId>,
    supported: SupportedSchemas,
    quota: QuotaConfig,
    bodies: RecordDirectory,
    receipts: ReceiptDirectory,
    /// Durable receipts live in the authenticated index and are decoded only
    /// when their exact scope is replayed. This cache is deliberately bounded;
    /// non-durable replicas continue to use `receipts` as their complete store.
    receipt_cache: Arc<Mutex<ReceiptCache>>,
    receipt_count: u64,
    receipt_material_bytes: u64,
    /// Roots of the durable catalogs, carried so a commit can apply a delta
    /// rather than rebuild.
    body_index_root: Option<IndexRef>,
    /// The published catalog: head sets and declarations, no local refs.
    manifest_body_root: Option<IndexRef>,
    /// `ContentId` -> committed descriptor.
    content_index_root: Option<IndexRef>,
    /// Declared content references per Body, for the reachability sweep.
    declared_content: BTreeMap<BodyKey, Vec<[u8; 32]>>,
    /// Number of live Body declarations naming each descriptor. This derived
    /// index makes a candidate read image update its content view from only the
    /// touched declarations instead of rescanning every Body in the Space.
    declared_content_counts: BTreeMap<[u8; 32], u64>,
    /// The reverse of [`Self::declared_content`], narrowed to the World: which
    /// Worlds declare each descriptor, and how many of their live Bodies do.
    ///
    /// World rather than Body, because runtime cannot name a finer resource
    /// without borrowing product vocabulary — and because this stays bounded by
    /// installed Worlds where a `BodyKey` set would not. The inner count is
    /// what makes removal exact.
    declared_content_worlds: BTreeMap<[u8; 32], BTreeMap<WorldId, u64>>,
    /// O(1) physical upper estimate for the two declaration directories.
    /// Updated only by `replace_declared_content`, the single mutation seam.
    declared_content_retained_bytes: u64,
    /// Content committed but not yet declared by any Body, and the moment each
    /// hold lapses.
    ///
    /// There is a window between committing a descriptor and the Body that
    /// names it reaching the store — an upload finishes, then a person decides
    /// which issue to attach it to. Reachability is derived from live Bodies,
    /// so for the whole of that window the content is garbage by the sweep's
    /// only rule, and the sweep is right: nothing distinguishes an upload
    /// awaiting an attach from an upload nobody ever attached.
    ///
    /// A hold is what distinguishes them, and it is deliberately in memory. A
    /// hold is a claim about an operation this process is running; after a
    /// restart there is no such operation, so the claim is correctly gone and
    /// the abandoned upload becomes collectable — which is the behaviour a
    /// durable hold would have had to reproduce with an expiry sweep anyway.
    pending_content: BTreeMap<[u8; 32], std::time::Instant>,
    receipt_index_root: Option<IndexRef>,
    generation_index_root: Option<IndexRef>,
    generation_footprint: GenerationFootprint,
    ownership_index_root: Option<IndexRef>,
    manifest_root_object: Option<Object>,
    /// Opaque retained material kept in memory for non-durable replicas (a
    /// durable store keeps it as objects; this map indexes the raw envelope
    /// bytes + transaction bytes for byte-identical forwarding either way).
    raw_material: BTreeMap<BodyKey, Vec<RetainedHead>>,
    /// Bounded delivery-only knowledge of recently replaced signed heads.
    /// Eviction costs bandwidth only: a cache miss serves the full closure.
    recent_head_artifacts: BTreeMap<(BodyKey, [u8; 32]), Vec<Object>>,
    recent_head_order: VecDeque<(BodyKey, [u8; 32])>,
    /// At most one detached ordinary checkpoint builder per hot Body. The
    /// mutex is only for worker-result bookkeeping; document export and
    /// protection happen outside it and outside the user action path.
    checkpoint_jobs: Mutex<BTreeMap<BodyKey, PreparedCheckpoint>>,
}

/// A dense Body identity meaningful only with the exact [`ReadSnapshot`] that
/// issued it. Slot numbers persist across descendant publications: replacing
/// a Body path-copies one slot page, while insertion allocates a free/tail
/// slot without renumbering any existing Body.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BodyIx(u32);

impl BodyIx {
    pub const fn as_u32(self) -> u32 {
        self.0
    }
}

/// Portable identity of one exact canonical Body image. Unlike a slot, this
/// survives process-local BodyIx assignment and commits the Body address,
/// immutable schema binding, publication stamp, signed causal closure/final
/// Version, protected-key epochs, and authenticated plaintext-size hint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BodyImageId([u8; 32]);

impl BodyImageId {
    pub const fn as_bytes(self) -> [u8; 32] {
        self.0
    }
}

/// Why a cold exact Body image could not be resolved. Resolution is fail
/// closed: no partially imported image is cached or installed into Replica.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BodyImageFailure {
    MaterialUnavailable,
    Io,
    KeyUnavailable,
    Opaque,
    Capacity,
    Corrupt,
    ModelMismatch,
    ImmutableConflict,
}

impl std::fmt::Display for BodyImageFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl std::error::Error for BodyImageFailure {}

/// Pre-resolution admission coordinates authenticated by the signed Material.
/// `decoded_upper_bound` covers the retained plaintext plus the largest
/// simultaneous artifact/Engine replacement working set; it is deliberately
/// not just the final plaintext length. Runtime reserves both fields before
/// reading/decrypting the first artifact and still validates actual retained
/// bytes after resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BodyImageBounds {
    pub protected_bytes: u64,
    pub decoded_upper_bound: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BodyImagePresence {
    Absent,
    Opaque { image: BodyImageId },
    Readable { body: BodyIx, image: BodyImageId },
}

#[derive(Debug, Clone)]
enum SnapshotImage {
    /// Non-durable/test Bodies remain shared Fabric images.
    Resident(fabric::BodySnapshot),
    /// Every interpreted durable Body retains only its signed causal closure.
    /// Its binding decides whether exact resolution yields an Atomic value or
    /// a canonical collaborative export with stable scaffold operation ids.
    Cold(Arc<CausalMaterial>),
    /// A not-yet-durable candidate is readable from the already-verified hot
    /// image while Corpus validation runs. Runtime clears this cell after the
    /// journal commit; subsequent reads take the same cold closure path.
    Pending {
        material: Arc<CausalMaterial>,
        hot: Arc<Mutex<Option<Arc<fabric::BodySnapshot>>>>,
    },
    /// Legitimate retained material this publication cannot interpret. The
    /// compact signed identity remains visible to typed readers; product facts
    /// and schema indexes never admit it.
    Opaque(Option<Arc<CausalMaterial>>),
}

impl SnapshotImage {
    fn material(&self) -> Option<&Arc<CausalMaterial>> {
        match self {
            Self::Resident(_) | Self::Opaque(None) => None,
            Self::Cold(material) | Self::Pending { material, .. } => Some(material),
            Self::Opaque(Some(material)) => Some(material),
        }
    }

    fn retained_bytes_estimate(&self) -> u64 {
        match self {
            Self::Resident(snapshot) => snapshot.retained_bytes(),
            Self::Cold(material) => u64::try_from(material.encode().len())
                .unwrap_or(u64::MAX)
                .saturating_add(32),
            Self::Pending { material, hot } => u64::try_from(material.encode().len())
                .unwrap_or(u64::MAX)
                .saturating_add(64)
                .saturating_add(
                    hot.lock()
                        .ok()
                        .and_then(|snapshot| {
                            snapshot.as_ref().map(|snapshot| snapshot.retained_bytes())
                        })
                        .unwrap_or(0),
                ),
            Self::Opaque(material) => material
                .as_ref()
                .map(|material| {
                    u64::try_from(material.encode().len())
                        .unwrap_or(u64::MAX)
                        .saturating_add(32)
                })
                .unwrap_or(32),
        }
    }

    fn is_readable(&self) -> bool {
        !matches!(self, Self::Opaque(_))
    }
}

/// Immutable object reader plus the exact epoch capabilities pinned by a
/// publication. There is deliberately no payload cache here; Runtime owns the
/// one governed singleflight/cache used by readers and extractors.
#[derive(Debug)]
struct BodyImageResolver {
    /// Swapped exactly once for a prepared local publication: before durable
    /// finalize it names the prior root (changed Bodies are resident); after
    /// finalize it names the newly authoritative deferred root. The snapshot
    /// and Corpus share this resolver Arc, so the O(1) root rebind reaches both
    /// without rebuilding either immutable projection.
    store: Mutex<journal::Reader>,
    /// Persistent epoch capability dictionary. A next publication path-copies
    /// only newly observed epochs; it never consults a mutable live key source
    /// while resolving an exact historical image.
    keys: imbl::OrdMap<[u8; 16], AuthorizedBodyKey>,
}

impl BodyImageResolver {
    fn attach_durable_root(&self, store: journal::Reader) {
        let mut held = self
            .store
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *held = store;
    }

    fn resolve(
        &self,
        key: &BodyKey,
        binding: &BodyBinding,
        material: &CausalMaterial,
    ) -> Result<Arc<fabric::BodySnapshot>, BodyImageFailure> {
        material.validate().map_err(|_| BodyImageFailure::Corrupt)?;
        let store = self
            .store
            .lock()
            .map_err(|_| BodyImageFailure::MaterialUnavailable)?
            .clone();
        let mut engine = Engine::new();
        for reference in std::iter::once(&material.checkpoint).chain(&material.delta_tail) {
            let object = Object {
                hash: reference.hash,
                len: reference.len,
            };
            let envelope = store
                .read_deferred_object_bounded(&object, reference.len)
                .map_err(|failure| match failure {
                    journal::Failure::Operation { .. } => BodyImageFailure::Io,
                    journal::Failure::Integrity(journal::Defect::MissingObject) => {
                        BodyImageFailure::MaterialUnavailable
                    }
                    _ => BodyImageFailure::Corrupt,
                })?;
            let epoch = mechanics::authorization::body_epoch_id(&envelope)
                .ok_or(BodyImageFailure::Corrupt)?;
            if epoch != reference.epoch {
                return Err(BodyImageFailure::Corrupt);
            }
            let opening = self
                .keys
                .get(&epoch)
                .ok_or(BodyImageFailure::KeyUnavailable)?;
            let artifact =
                open_artifact(opening, &envelope).map_err(|_| BodyImageFailure::Corrupt)?;
            let status = engine
                .import_artifact(&fabric_key(key), &artifact)
                .map_err(|_| BodyImageFailure::Corrupt)?;
            if status.pending {
                return Err(BodyImageFailure::Corrupt);
            }
        }
        let version = engine
            .version(&fabric_key(key))
            .map_err(|_| BodyImageFailure::Corrupt)?;
        if version != material.version {
            return Err(BodyImageFailure::Corrupt);
        }
        let snapshot = engine
            .body_snapshot(&fabric_key(key))
            .map_err(|_| BodyImageFailure::Corrupt)?
            .ok_or(BodyImageFailure::Corrupt)?;
        match binding.mutation_model {
            MUTATION_ATOMIC | MUTATION_IMMUTABLE_ATOMIC => {
                let Some(value) = snapshot.read_shared() else {
                    return Err(BodyImageFailure::ModelMismatch);
                };
                if u64::try_from(value.len()).unwrap_or(u64::MAX) != material.plaintext_size {
                    return Err(BodyImageFailure::Corrupt);
                }
                if binding.mutation_model == MUTATION_IMMUTABLE_ATOMIC
                    && !immutable_key_matches(
                        key,
                        &binding.schema,
                        binding.schema_version,
                        &binding.encoding,
                        value.as_ref(),
                    )
                {
                    return Err(BodyImageFailure::ImmutableConflict);
                }
            }
            MUTATION_COLLABORATIVE => {
                if snapshot.read_shared().is_some() {
                    return Err(BodyImageFailure::ModelMismatch);
                }
                // The signed Material hint prices the causal artifact working
                // set; a canonical scaffold snapshot can differ from the sum of a
                // checkpoint and update tail. Validate the actual immutable
                // export against the same conservative bound Runtime reserves.
                let largest = std::iter::once(&material.checkpoint)
                    .chain(&material.delta_tail)
                    .map(|reference| reference.len)
                    .max()
                    .unwrap_or(0);
                let decoded_bound = material
                    .plaintext_size
                    .max(largest)
                    .saturating_mul(3)
                    .saturating_add(64 * 1024);
                if snapshot.retained_bytes() > decoded_bound {
                    return Err(BodyImageFailure::Capacity);
                }
            }
            _ => return Err(BodyImageFailure::ModelMismatch),
        }
        Ok(Arc::new(snapshot))
    }
}

fn body_image_id(
    key: &BodyKey,
    binding: &BodyBinding,
    stamp: &[u8; 32],
    material: &CausalMaterial,
) -> BodyImageId {
    fn field(hasher: &mut blake3::Hasher, bytes: &[u8]) {
        hasher.update(&u64::try_from(bytes.len()).unwrap_or(u64::MAX).to_be_bytes());
        hasher.update(bytes);
    }
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"lait/body-image/1\0");
    field(&mut hasher, key.world.as_bytes());
    hasher.update(&key.body.as_bytes());
    field(&mut hasher, binding.schema.as_bytes());
    hasher.update(&binding.schema_version.to_be_bytes());
    field(&mut hasher, binding.encoding.as_bytes());
    hasher.update(&[binding.mutation_model]);
    hasher.update(stamp);
    field(&mut hasher, &material.encode());
    BodyImageId(*hasher.finalize().as_bytes())
}

/// One interpreted Body in an immutable Replica generation.
#[derive(Debug, Clone)]
struct SnapshotBody {
    binding: BodyBinding,
    stamp: [u8; 32],
    image_id: BodyImageId,
    plaintext_size: u64,
    image: SnapshotImage,
}

/// Calibrated above the 1-record-per-Body release fixture after excluding the
/// exact export/stamp bytes: shared BodyKey, dense Body directory, schema
/// membership, binding, and leaf/spine slack.
const SNAPSHOT_BODY_FIXED_ESTIMATE: u64 = 400;

fn snapshot_stamp(stamp: &[u8]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"lait/read-snapshot/body-stamp/1\0");
    hasher.update(stamp);
    *hasher.finalize().as_bytes()
}

fn snapshot_body_retained_estimate(body: &SnapshotBody) -> u64 {
    SNAPSHOT_BODY_FIXED_ESTIMATE
        .saturating_add(32)
        .saturating_add(body.image.retained_bytes_estimate())
}

impl SnapshotBody {
    fn resident(
        key: &BodyKey,
        binding: BodyBinding,
        stamp: [u8; 32],
        body: fabric::BodySnapshot,
    ) -> Self {
        let plaintext_size = body.retained_bytes();
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"lait/resident-body-image/1\0");
        hasher.update(key.world.as_bytes());
        hasher.update(&key.body.as_bytes());
        hasher.update(&stamp);
        hasher.update(&plaintext_size.to_be_bytes());
        Self {
            binding,
            stamp,
            image_id: BodyImageId(*hasher.finalize().as_bytes()),
            plaintext_size,
            image: SnapshotImage::Resident(body),
        }
    }

    fn cold(
        key: &BodyKey,
        binding: BodyBinding,
        stamp: [u8; 32],
        material: Arc<CausalMaterial>,
    ) -> Self {
        Self {
            image_id: body_image_id(key, &binding, &stamp, &material),
            plaintext_size: material.plaintext_size,
            binding,
            stamp,
            image: SnapshotImage::Cold(material),
        }
    }

    fn pending(
        key: &BodyKey,
        binding: BodyBinding,
        stamp: [u8; 32],
        material: Arc<CausalMaterial>,
        body: fabric::BodySnapshot,
    ) -> Self {
        Self {
            image_id: body_image_id(key, &binding, &stamp, &material),
            plaintext_size: material.plaintext_size,
            binding,
            stamp,
            image: SnapshotImage::Pending {
                material,
                hot: Arc::new(Mutex::new(Some(Arc::new(body)))),
            },
        }
    }

    fn opaque(
        key: &BodyKey,
        binding: BodyBinding,
        stamp: [u8; 32],
        material: Option<Arc<CausalMaterial>>,
    ) -> Self {
        let plaintext_size = material
            .as_ref()
            .map(|material| material.plaintext_size)
            .unwrap_or(0);
        let image_id = material.as_ref().map_or_else(
            || {
                let mut hasher = blake3::Hasher::new();
                hasher.update(b"lait/opaque-body-image/1\0");
                hasher.update(key.world.as_bytes());
                hasher.update(&key.body.as_bytes());
                hasher.update(&stamp);
                BodyImageId(*hasher.finalize().as_bytes())
            },
            |material| body_image_id(key, &binding, &stamp, material),
        );
        Self {
            binding,
            stamp,
            image_id,
            plaintext_size,
            image: SnapshotImage::Opaque(material),
        }
    }
}

fn snapshot_directory_retained_estimate(bodies: &BodyDirectory) -> u64 {
    bodies.iter().fold(0u64, |total, (_, body)| {
        total.saturating_add(snapshot_body_retained_estimate(body))
    })
}

fn record_snapshot_retained_estimate(record: &BodyRecord) -> u64 {
    SNAPSHOT_BODY_FIXED_ESTIMATE
        .saturating_add(32)
        .saturating_add(record.causal.as_ref().map_or(32, |material| {
            u64::try_from(material.encode().len())
                .unwrap_or(u64::MAX)
                .saturating_add(32)
        }))
}

impl GenerationFootprint {
    fn from_records(records: &RecordDirectory) -> Result<Self, Failure> {
        let mut footprint = Self::default();
        for (key, record) in records.iter() {
            footprint.adjust_record(key, record, true)?;
        }
        footprint.validate()?;
        Ok(footprint)
    }

    fn after_changes(
        &self,
        current: &RecordDirectory,
        changed: &BTreeMap<BodyKey, Option<BodyRecord>>,
    ) -> Result<Self, Failure> {
        let mut next = self.clone();
        for (key, replacement) in changed {
            if let Some(prior) = current.get(key) {
                next.adjust_record(key, prior, false)?;
            }
            if let Some(replacement) = replacement {
                next.adjust_record(key, replacement, true)?;
            }
        }
        next.validate()?;
        Ok(next)
    }

    fn adjust_record(
        &mut self,
        key: &BodyKey,
        record: &BodyRecord,
        add: bool,
    ) -> Result<(), Failure> {
        let retained = record_snapshot_retained_estimate(record);
        if add {
            self.body_count = self
                .body_count
                .checked_add(1)
                .ok_or(Failure::QuotaExceeded)?;
            self.snapshot_retained_bytes = self
                .snapshot_retained_bytes
                .checked_add(retained)
                .ok_or(Failure::QuotaExceeded)?;
        } else {
            self.body_count = self
                .body_count
                .checked_sub(1)
                .ok_or(Failure::Integrity(Defect::Index))?;
            self.snapshot_retained_bytes = self
                .snapshot_retained_bytes
                .checked_sub(retained)
                .ok_or(Failure::Integrity(Defect::Index))?;
        }
        if !record.interpreted {
            return Ok(());
        }
        let payload_bytes = record
            .causal
            .as_ref()
            .map_or(0, |material| material.plaintext_size);
        let probe = GenerationSourceFootprint {
            world: key.world.clone(),
            schema: record.binding.schema.clone(),
            version: record.binding.schema_version,
            body_count: 0,
            payload_bytes: 0,
        };
        match self.sources.binary_search_by(|source| {
            (&source.world, &source.schema, source.version).cmp(&(
                &probe.world,
                &probe.schema,
                probe.version,
            ))
        }) {
            Ok(index) => {
                let source = self
                    .sources
                    .get_mut(index)
                    .ok_or(Failure::Integrity(Defect::Index))?;
                if add {
                    source.body_count = source
                        .body_count
                        .checked_add(1)
                        .ok_or(Failure::QuotaExceeded)?;
                    source.payload_bytes = source
                        .payload_bytes
                        .checked_add(payload_bytes)
                        .ok_or(Failure::QuotaExceeded)?;
                } else {
                    source.body_count = source
                        .body_count
                        .checked_sub(1)
                        .ok_or(Failure::Integrity(Defect::Index))?;
                    source.payload_bytes = source
                        .payload_bytes
                        .checked_sub(payload_bytes)
                        .ok_or(Failure::Integrity(Defect::Index))?;
                    if source.body_count == 0 {
                        self.sources.remove(index);
                    }
                }
            }
            Err(index) if add => {
                if self.sources.len() >= MAX_GENERATION_SOURCES {
                    return Err(Failure::QuotaExceeded);
                }
                self.sources.insert(
                    index,
                    GenerationSourceFootprint {
                        body_count: 1,
                        payload_bytes,
                        ..probe
                    },
                );
            }
            Err(_) => return Err(Failure::Integrity(Defect::Index)),
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
struct BodySlot {
    key: Arc<BodyKey>,
    value: SnapshotBody,
}

/// Publication-persistent Body slots plus one compact ordered lookup.
///
/// The key is allocated exactly once and shared by the lookup leaf and reverse
/// slot. Schema membership stores only the four-byte slot. A lexical-front
/// insertion therefore replaces one bounded lookup leaf and one slot-vector
/// path; it never renumbers or rewrites the million existing rows.
#[derive(Debug, Clone, Default)]
struct BodyDirectory {
    lookup: SnapshotDirectory<Arc<BodyKey>, BodyIx>,
    slots: imbl::Vector<Option<BodySlot>>,
    free: imbl::OrdSet<BodyIx>,
}

#[derive(Default)]
struct BodyDirectoryBuilder {
    lookup: SnapshotDirectoryBuilder<Arc<BodyKey>, BodyIx>,
    slots: imbl::Vector<Option<BodySlot>>,
}

impl BodyDirectoryBuilder {
    fn push(&mut self, key: Arc<BodyKey>, value: SnapshotBody) {
        let Ok(raw_index) = u32::try_from(self.slots.len()) else {
            return;
        };
        let index = BodyIx(raw_index);
        self.lookup.push(key.clone(), index);
        self.slots.push_back(Some(BodySlot { key, value }));
    }

    fn finish(self) -> BodyDirectory {
        BodyDirectory {
            lookup: self.lookup.finish(),
            slots: self.slots,
            free: imbl::OrdSet::new(),
        }
    }
}

impl BodyDirectory {
    fn len(&self) -> usize {
        self.lookup.len()
    }

    fn is_empty(&self) -> bool {
        self.lookup.is_empty()
    }

    fn body_ix(&self, key: &BodyKey) -> Option<BodyIx> {
        self.lookup.get(key).copied()
    }

    fn slot(&self, index: BodyIx) -> Option<&BodySlot> {
        self.slots.get(usize::try_from(index.0).ok()?)?.as_ref()
    }

    fn get(&self, key: &BodyKey) -> Option<&SnapshotBody> {
        self.slot(self.body_ix(key)?).map(|slot| &slot.value)
    }

    fn get_key_value(&self, key: &BodyKey) -> Option<(&Arc<BodyKey>, &SnapshotBody)> {
        let slot = self.slot(self.body_ix(key)?)?;
        Some((&slot.key, &slot.value))
    }

    fn insert(&mut self, key: Arc<BodyKey>, value: SnapshotBody) -> Option<SnapshotBody> {
        if let Some(index) = self.body_ix(&key) {
            let slot = self
                .slots
                .get_mut(usize::try_from(index.0).ok()?)?
                .as_mut()?;
            return Some(std::mem::replace(&mut slot.value, value));
        }
        let index = if let Some(index) = self.free.iter().next().copied() {
            self.free.remove(&index);
            let slot = self.slots.get_mut(usize::try_from(index.0).ok()?)?;
            debug_assert!(slot.is_none());
            *slot = Some(BodySlot {
                key: key.clone(),
                value,
            });
            index
        } else {
            let index = BodyIx(u32::try_from(self.slots.len()).ok()?);
            self.slots.push_back(Some(BodySlot {
                key: key.clone(),
                value,
            }));
            index
        };
        // The insertion must happen in every profile; only the emptiness CLAIM
        // is debug-only. With the insert inside the assertion, release builds
        // stored the slot and never indexed it — a Body invisible to lookup
        // and iteration alike, surfacing as a bare Integrity(Encoding) on the
        // first write into a rebuilt store, in release binaries only.
        let previous = self.lookup.insert(key, index);
        debug_assert!(previous.is_none());
        None
    }

    fn remove(&mut self, key: &BodyKey) -> Option<SnapshotBody> {
        let index = self.lookup.remove(key)?;
        let slot = self.slots.get_mut(usize::try_from(index.0).ok()?)?.take()?;
        self.free.insert(index);
        Some(slot.value)
    }

    fn iter(&self) -> impl Iterator<Item = (&Arc<BodyKey>, &SnapshotBody)> {
        self.lookup
            .iter()
            .filter_map(|(_, index)| self.slot(*index).map(|slot| (&slot.key, &slot.value)))
    }

    fn iter_with_ix(&self) -> impl Iterator<Item = (BodyIx, &Arc<BodyKey>, &SnapshotBody)> {
        self.lookup.iter().filter_map(|(_, index)| {
            self.slot(*index)
                .map(|slot| (*index, &slot.key, &slot.value))
        })
    }

    fn keys(&self) -> impl Iterator<Item = &Arc<BodyKey>> {
        self.iter().map(|(key, _)| key)
    }
}

const SNAPSHOT_DIRECTORY_LEAF: usize = 256;

#[derive(Debug, Clone)]
struct SnapshotDirectoryEntry<K, V> {
    key: K,
    value: V,
}

/// Persistent sorted directory with dense, bounded leaves.
///
/// A record-shaped World can retain millions of Bodies. General-purpose
/// persistent map nodes made their directory and schema-membership overhead
/// larger than the frozen records. Replacing a key here clones one 256-entry
/// leaf and the persistent vector spine; unchanged leaves and their Body
/// images remain shared across generations.
#[derive(Debug, Clone)]
struct SnapshotDirectory<K, V> {
    leaves: imbl::Vector<Arc<[SnapshotDirectoryEntry<K, V>]>>,
    len: usize,
}

struct SnapshotDirectoryBuilder<K, V> {
    leaves: imbl::Vector<Arc<[SnapshotDirectoryEntry<K, V>]>>,
    current: Vec<SnapshotDirectoryEntry<K, V>>,
    len: usize,
}

impl<K, V> Default for SnapshotDirectoryBuilder<K, V> {
    fn default() -> Self {
        Self {
            leaves: imbl::Vector::new(),
            current: Vec::with_capacity(SNAPSHOT_DIRECTORY_LEAF),
            len: 0,
        }
    }
}

impl<K: Ord, V> SnapshotDirectoryBuilder<K, V> {
    fn push(&mut self, key: K, value: V) {
        let prior = self.current.last().map(|entry| &entry.key).or_else(|| {
            self.leaves
                .back()
                .and_then(|leaf| leaf.last().map(|entry| &entry.key))
        });
        debug_assert!(prior.is_none_or(|prior| prior < &key));
        self.current.push(SnapshotDirectoryEntry { key, value });
        self.len = self.len.saturating_add(1);
        if self.current.len() == SNAPSHOT_DIRECTORY_LEAF {
            self.flush();
        }
    }

    fn flush(&mut self) {
        if self.current.is_empty() {
            return;
        }
        self.leaves
            .push_back(Arc::from(std::mem::take(&mut self.current)));
        self.current = Vec::with_capacity(SNAPSHOT_DIRECTORY_LEAF);
    }

    fn finish(mut self) -> SnapshotDirectory<K, V> {
        self.flush();
        SnapshotDirectory {
            leaves: self.leaves,
            len: self.len,
        }
    }
}

impl<K, V> Default for SnapshotDirectory<K, V> {
    fn default() -> Self {
        Self {
            leaves: imbl::Vector::new(),
            len: 0,
        }
    }
}

impl<K: Clone + Ord, V: Clone> SnapshotDirectory<K, V> {
    fn len(&self) -> usize {
        self.len
    }

    fn is_empty(&self) -> bool {
        self.len == 0
    }

    fn leaf_for<Q>(&self, key: &Q) -> Option<usize>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        let mut low = 0usize;
        let mut high = self.leaves.len();
        while low < high {
            let mid = low.saturating_add(high.saturating_sub(low) / 2);
            let leaf = self.leaves.get(mid)?;
            let last = leaf.last()?;
            if last.key.borrow() < key {
                low = mid.saturating_add(1);
            } else {
                high = mid;
            }
        }
        Some(low.min(self.leaves.len().saturating_sub(1)))
    }

    fn get_key_value<Q>(&self, key: &Q) -> Option<(&K, &V)>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        if self.leaves.is_empty() {
            return None;
        }
        let leaf = self.leaves.get(self.leaf_for(key)?)?;
        let position = leaf
            .binary_search_by(|entry| entry.key.borrow().cmp(key))
            .ok()?;
        let entry = leaf.get(position)?;
        Some((&entry.key, &entry.value))
    }

    fn get<Q>(&self, key: &Q) -> Option<&V>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        self.get_key_value(key).map(|(_, value)| value)
    }

    fn insert(&mut self, key: K, value: V) -> Option<V> {
        if self.leaves.is_empty() {
            self.leaves
                .push_back(Arc::from([SnapshotDirectoryEntry { key, value }]));
            self.len = 1;
            return None;
        }
        let leaf_index = self.leaf_for(&key)?;
        let mut leaf = self.leaves.get(leaf_index)?.to_vec();
        match leaf.binary_search_by(|entry| entry.key.cmp(&key)) {
            Ok(position) => {
                let old = std::mem::replace(&mut leaf.get_mut(position)?.value, value);
                self.leaves.set(leaf_index, Arc::from(leaf));
                Some(old)
            }
            Err(position) => {
                leaf.insert(position, SnapshotDirectoryEntry { key, value });
                self.len = self.len.saturating_add(1);
                if leaf.len() <= SNAPSHOT_DIRECTORY_LEAF {
                    self.leaves.set(leaf_index, Arc::from(leaf));
                } else {
                    let right = leaf.split_off(leaf.len() / 2);
                    self.leaves.set(leaf_index, Arc::from(leaf));
                    self.leaves
                        .insert(leaf_index.saturating_add(1), Arc::from(right));
                }
                None
            }
        }
    }

    fn remove<Q>(&mut self, key: &Q) -> Option<V>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        if self.leaves.is_empty() {
            return None;
        }
        let leaf_index = self.leaf_for(key)?;
        let mut leaf = self.leaves.get(leaf_index)?.to_vec();
        let position = leaf
            .binary_search_by(|entry| entry.key.borrow().cmp(key))
            .ok()?;
        let removed = leaf.remove(position).value;
        self.len = self.len.saturating_sub(1);
        if leaf.is_empty() {
            self.leaves.remove(leaf_index);
        } else {
            self.leaves.set(leaf_index, Arc::from(leaf));
        }
        Some(removed)
    }

    fn iter(&self) -> impl Iterator<Item = (&K, &V)> {
        self.leaves
            .iter()
            .flat_map(|leaf| leaf.iter().map(|entry| (&entry.key, &entry.value)))
    }

    fn keys(&self) -> impl Iterator<Item = &K> {
        self.iter().map(|(key, _)| key)
    }

    fn keys_page_after<Q>(&self, after: Option<&Q>, limit: usize) -> Vec<K>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        if limit == 0 || self.leaves.is_empty() {
            return Vec::new();
        }
        let mut leaf_index = after.and_then(|key| self.leaf_for(key)).unwrap_or(0);
        let mut first = true;
        let mut page = Vec::with_capacity(limit);
        while leaf_index < self.leaves.len() && page.len() < limit {
            let Some(leaf) = self.leaves.get(leaf_index) else {
                break;
            };
            let start = if first {
                first = false;
                after.map_or(0, |key| {
                    leaf.partition_point(|entry| entry.key.borrow() <= key)
                })
            } else {
                0
            };
            page.extend(
                leaf.get(start..)
                    .into_iter()
                    .flatten()
                    .take(limit.saturating_sub(page.len()))
                    .map(|entry| entry.key.clone()),
            );
            leaf_index = leaf_index.saturating_add(1);
        }
        page
    }
}

/// A shareable, writer-independent read generation.
///
/// The maps are persistent: publishing a commit replaces only the touched
/// Body paths and shares every unchanged branch with the previous generation.
/// Cloning a snapshot is O(1), and a query may hold it for arbitrarily long
/// without holding the Replica's committing mutex.
#[derive(Debug, Clone)]
pub struct ReadSnapshot {
    root: [u8; 32],
    frontier: ReplicaFrontier,
    /// Exact object/key closure for cold Atomic image inflation. Opening keys
    /// are cloned capabilities pinned for this snapshot; key-source retirement
    /// after publication cannot invalidate a cursor that already holds it.
    resolver: Option<Arc<BodyImageResolver>>,
    bodies: BodyDirectory,
    /// Exact schema membership, persistently shared across generations.
    /// Exec and World projections enter through this index rather than
    /// scanning the Space-wide Body map.
    schema_bodies:
        imbl::OrdMap<(WorldId, SchemaId, u32), SnapshotDirectory<crate::body::BodyId, BodyIx>>,
    /// Exact canonical payload bytes by readable source coordinate. This is
    /// maintained with the same changed-Body delta as `schema_bodies`, so a
    /// World build can be admitted from observed source size without scanning
    /// or decoding every Body first.
    schema_payload_bytes: imbl::OrdMap<(WorldId, SchemaId, u32), u64>,
    /// Content declarations by Body at this exact generation.
    ///
    /// Keeping this beside the persistent Body map is what lets the next
    /// generation update descriptor reachability from the changed Body set;
    /// otherwise an incremental Body freeze would still hide an O(all Bodies)
    /// declaration scan.
    declared_content: imbl::OrdMap<BodyKey, Arc<[[u8; 32]]>>,
    content: imbl::OrdMap<[u8; 32], crate::content::ContentDescriptor>,
    /// Incrementally maintained admission price for all retained Body images
    /// and their compact directories. It includes exact canonical export and
    /// stamp bytes plus a release-calibrated conservative fixed cost.
    retained_bytes_estimate: u64,
}

/// An immutable durable-generation reader pinned at one Replica commit point.
///
/// It owns the exact generation index root, key capability, and current shared
/// snapshot needed for reconstruction. Deep ancestry/object reads therefore
/// run without borrowing the Replica writer; a later commit cannot change the
/// semantic index this reader follows.
#[derive(Clone)]
pub struct GenerationReader {
    store: Option<journal::Reader>,
    keys: Option<Arc<dyn BodyKeySource>>,
    generation_index_root: Option<IndexRef>,
    current: Arc<ReadSnapshot>,
}

/// Durable ancestry metadata for one read generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadGeneration {
    pub root: [u8; 32],
    pub parent: Option<[u8; 32]>,
    pub frontier: ReplicaFrontier,
}

/// Immutable point reader for durable idempotency receipts.
///
/// The reader pins one journal commit point and performs one authenticated
/// receipt-index path plus one bounded deferred-object read on a cold miss.
/// It is owned and `Send + Sync`, so Runtime can place cold lookup on its
/// blocking host lane without holding the Replica writer or reactor lock.
#[derive(Clone)]
pub struct ReceiptReader {
    store: journal::Reader,
    receipt_index_root: Option<IndexRef>,
    cache: Arc<Mutex<ReceiptCache>>,
    footprint: ReceiptFootprint,
    sequence: u64,
}

/// Result of one authenticated immutable receipt check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReceiptCheck {
    Replayed(RequestReceipt),
    Absent(ReceiptAbsence),
}

/// Opaque proof that one exact idempotency scope was absent at a pinned
/// journal commit point. Only Replica can inspect or mint its coordinates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceiptAbsence {
    sequence: u64,
    receipt_index_root: Option<IndexRef>,
    scope: Vec<u8>,
}

/// Authenticated O(1) receipt-ledger admission metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReceiptFootprint {
    pub count: u64,
    pub material_bytes: u64,
    pub cache_upper_bound: u64,
    pub cold_lookup_transient_upper_bound: u64,
}

impl ReceiptReader {
    /// The exact durable count/encoded-byte ledger plus conservative cache and
    /// cold point-lookup memory bounds. This is read from authenticated
    /// StoreMeta; it does not walk the receipt index.
    pub const fn footprint(&self) -> ReceiptFootprint {
        self.footprint
    }

    /// Authenticate replay/conflict/absence at this immutable commit point.
    /// The absence proof lets the mutation path avoid repeating the cold index
    /// read while still rejecting any intervening durable truth.
    pub fn check_action(
        &self,
        space: &SpaceId,
        world: &WorldId,
        device: &mechanics::ids::DeviceId,
        request: &[u8; 16],
        payload_hash: &[u8; 32],
    ) -> Result<ReceiptCheck, Failure> {
        let scope = crate::receipt::scope_key(space, world, device, request);
        match self.lookup_scope(&scope)? {
            Some(receipt) if &receipt.payload_hash == payload_hash => {
                Ok(ReceiptCheck::Replayed(receipt))
            }
            Some(_) => Err(Failure::RequestIdConflict),
            None => Ok(ReceiptCheck::Absent(ReceiptAbsence {
                sequence: self.sequence,
                receipt_index_root: self.receipt_index_root,
                scope,
            })),
        }
    }

    /// Look up one exact idempotency scope. Matching payload returns the
    /// committed receipt; mismatching payload is a typed conflict; absence is
    /// the only `Ok(None)` case.
    pub fn lookup_action(
        &self,
        space: &SpaceId,
        world: &WorldId,
        device: &mechanics::ids::DeviceId,
        request: &[u8; 16],
        payload_hash: &[u8; 32],
    ) -> Result<Option<RequestReceipt>, Failure> {
        match self.check_action(space, world, device, request, payload_hash)? {
            ReceiptCheck::Replayed(receipt) => Ok(Some(receipt)),
            ReceiptCheck::Absent(_) => Ok(None),
        }
    }

    fn lookup_scope(&self, scope: &[u8]) -> Result<Option<RequestReceipt>, Failure> {
        if let Some(receipt) = self
            .cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(scope)
        {
            return Ok(Some(receipt));
        }
        let value = crate::index::lookup(
            &ReaderNodes(&self.store),
            self.receipt_index_root,
            &receipt_index_key(scope),
        )
        .map_err(|_| Failure::Integrity(Defect::Index))?;
        let Some(value) = value else {
            return Ok(None);
        };
        let indexed: IndexedReceipt =
            postcard::from_bytes(&value).map_err(|_| Failure::Integrity(Defect::Encoding))?;
        if postcard::to_stdvec(&indexed).ok().as_deref() != Some(value.as_slice())
            || indexed.scope.as_slice() != scope
            || receipt_index_key(&indexed.scope) != receipt_index_key(scope)
        {
            return Err(Failure::Integrity(Defect::Index));
        }
        let bytes = self
            .store
            .read_deferred_object_bounded(&indexed.object, MAX_RECEIPT_OBJECT_BYTES)
            .map_err(|failure| match failure {
                journal::Failure::Integrity(defect) => Failure::Integrity(Defect::Store(defect)),
                other => Failure::Durability(other),
            })?;
        let receipt = validate_receipt_material(scope, &bytes)?;
        self.cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(indexed.scope, receipt.clone(), indexed.object);
        Ok(Some(receipt))
    }
}

type SchemaBodyIndex =
    imbl::OrdMap<(WorldId, SchemaId, u32), SnapshotDirectory<crate::body::BodyId, BodyIx>>;
type SchemaPayloadIndex = imbl::OrdMap<(WorldId, SchemaId, u32), u64>;

fn pin_body_image_resolver<'a>(
    store: journal::Reader,
    key_source: &Arc<dyn BodyKeySource>,
    base: Option<&BodyImageResolver>,
    materials: impl IntoIterator<Item = &'a CausalMaterial>,
) -> Result<Arc<BodyImageResolver>, BodyImageFailure> {
    let mut keys = base
        .map(|resolver| resolver.keys.clone())
        .unwrap_or_default();
    for material in materials {
        material.validate().map_err(|_| BodyImageFailure::Corrupt)?;
        for reference in std::iter::once(&material.checkpoint).chain(&material.delta_tail) {
            if keys.contains_key(&reference.epoch) {
                continue;
            }
            let opening = key_source
                .opening_key(&reference.epoch)
                .ok_or(BodyImageFailure::KeyUnavailable)?;
            keys.insert(reference.epoch, opening);
        }
    }
    Ok(Arc::new(BodyImageResolver {
        store: Mutex::new(store),
        keys,
    }))
}

fn insert_schema_body(
    index: &mut SchemaBodyIndex,
    body_ix: BodyIx,
    key: &BodyKey,
    binding: &BodyBinding,
) {
    let coordinate = (
        key.world.clone(),
        binding.schema.clone(),
        binding.schema_version,
    );
    let mut keys = index.get(&coordinate).cloned().unwrap_or_default();
    keys.insert(key.body.clone(), body_ix);
    index.insert(coordinate, keys);
}

fn remove_schema_body(index: &mut SchemaBodyIndex, key: &BodyKey, binding: &BodyBinding) {
    let coordinate = (
        key.world.clone(),
        binding.schema.clone(),
        binding.schema_version,
    );
    let Some(mut keys) = index.get(&coordinate).cloned() else {
        return;
    };
    keys.remove(&key.body);
    if keys.is_empty() {
        index.remove(&coordinate);
    } else {
        index.insert(coordinate, keys);
    }
}

fn schema_body_index(bodies: &BodyDirectory) -> SchemaBodyIndex {
    let mut grouped = BTreeMap::<
        (WorldId, SchemaId, u32),
        SnapshotDirectoryBuilder<crate::body::BodyId, BodyIx>,
    >::new();
    for (body_ix, key, body) in bodies.iter_with_ix() {
        if !body.image.is_readable() {
            continue;
        }
        grouped
            .entry((
                key.world.clone(),
                body.binding.schema.clone(),
                body.binding.schema_version,
            ))
            .or_default()
            .push(key.body.clone(), body_ix);
    }
    grouped
        .into_iter()
        .map(|(coordinate, builder)| (coordinate, builder.finish()))
        .collect()
}

fn schema_payload_index(bodies: &BodyDirectory) -> SchemaPayloadIndex {
    let mut bytes = SchemaPayloadIndex::new();
    for (_, key, body) in bodies.iter_with_ix() {
        if !body.image.is_readable() {
            continue;
        }
        let coordinate = (
            key.world.clone(),
            body.binding.schema.clone(),
            body.binding.schema_version,
        );
        let next = bytes
            .get(&coordinate)
            .copied()
            .unwrap_or(0u64)
            .saturating_add(body.plaintext_size);
        bytes.insert(coordinate, next);
    }
    bytes
}

fn adjust_schema_payload(
    index: &mut SchemaPayloadIndex,
    key: &BodyKey,
    body: &SnapshotBody,
    add: bool,
) {
    let coordinate = (
        key.world.clone(),
        body.binding.schema.clone(),
        body.binding.schema_version,
    );
    let current = index.get(&coordinate).copied().unwrap_or(0);
    let next = if add {
        current.saturating_add(body.plaintext_size)
    } else {
        current.saturating_sub(body.plaintext_size)
    };
    if next == 0 {
        index.remove(&coordinate);
    } else {
        index.insert(coordinate, next);
    }
}

impl ReadSnapshot {
    /// Construct the exact persistent read-image shape for scale tests without
    /// manufacturing a million signed commits. Each supplied BodySnapshot is
    /// still a real Fabric image; this helper skips only Replica durability and
    /// publication ceremony, then builds the same Body and schema indexes used
    /// by production generations.
    #[cfg(any(test, feature = "scale-fixtures"))]
    #[doc(hidden)]
    pub fn from_body_rows_for_test(
        rows: impl IntoIterator<Item = (BodyKey, BodyBinding, Vec<u8>, fabric::BodySnapshot)>,
    ) -> Self {
        let mut builder = BodyDirectoryBuilder::default();
        for (key, binding, stamp, body) in rows {
            let stamp = snapshot_stamp(&stamp);
            let snapshot = SnapshotBody::resident(&key, binding, stamp, body);
            builder.push(Arc::new(key), snapshot);
        }
        let bodies = builder.finish();
        let schema_bodies = schema_body_index(&bodies);
        let schema_payload_bytes = schema_payload_index(&bodies);
        let retained_bytes_estimate = snapshot_directory_retained_estimate(&bodies);
        Self {
            root: [0u8; 32],
            frontier: ReplicaFrontier::EMPTY,
            resolver: None,
            bodies,
            schema_bodies,
            schema_payload_bytes,
            declared_content: imbl::OrdMap::new(),
            content: imbl::OrdMap::new(),
            retained_bytes_estimate,
        }
    }

    /// Construct the production cold-image directory shape for residency
    /// fixtures. Unlike `from_body_rows_for_test`, no canonical Body payload is
    /// retained: each row owns only the signed causal closure and authenticated
    /// plaintext-size coordinate used by the real durable read path.
    #[cfg(any(test, feature = "scale-fixtures"))]
    #[doc(hidden)]
    pub fn from_cold_body_rows_for_test(
        rows: impl IntoIterator<Item = (BodyKey, BodyBinding, Vec<u8>, CausalMaterial)>,
    ) -> Self {
        let mut builder = BodyDirectoryBuilder::default();
        for (key, binding, stamp, material) in rows {
            let stamp = snapshot_stamp(&stamp);
            let snapshot = SnapshotBody::cold(&key, binding, stamp, Arc::new(material));
            builder.push(Arc::new(key), snapshot);
        }
        let bodies = builder.finish();
        let schema_bodies = schema_body_index(&bodies);
        let schema_payload_bytes = schema_payload_index(&bodies);
        let retained_bytes_estimate = snapshot_directory_retained_estimate(&bodies);
        Self {
            root: [0u8; 32],
            frontier: ReplicaFrontier::EMPTY,
            resolver: None,
            bodies,
            schema_bodies,
            schema_payload_bytes,
            declared_content: imbl::OrdMap::new(),
            content: imbl::OrdMap::new(),
            retained_bytes_estimate,
        }
    }

    pub fn root(&self) -> [u8; 32] {
        self.root
    }

    pub fn frontier(&self) -> ReplicaFrontier {
        self.frontier
    }

    pub fn body_image_id(&self, body: BodyIx) -> Option<BodyImageId> {
        Some(self.bodies.slot(body)?.value.image_id)
    }

    pub fn body_image_plaintext_bytes(&self, body: BodyIx) -> Option<u64> {
        Some(self.bodies.slot(body)?.value.plaintext_size)
    }

    pub fn body_image_bounds(&self, body: BodyIx) -> Option<BodyImageBounds> {
        let body = &self.bodies.slot(body)?.value;
        let material = body.image.material();
        let protected_bytes = material.map_or(0, |material| {
            std::iter::once(&material.checkpoint)
                .chain(&material.delta_tail)
                .fold(0u64, |bytes, reference| bytes.saturating_add(reference.len))
        });
        let largest_protected = material.map_or(0, |material| {
            std::iter::once(&material.checkpoint)
                .chain(&material.delta_tail)
                .map(|reference| reference.len)
                .max()
                .unwrap_or(0)
        });
        // A fresh Engine can briefly hold the prior Atomic Arc, the opened
        // artifact Vec, and its replacement Arc at once. Protected envelope
        // bytes are priced separately above; this bound covers decoded
        // working state and fixed Engine/import bookkeeping.
        let decoded_upper_bound = if material.is_some() {
            body.plaintext_size
                .max(largest_protected)
                .saturating_mul(3)
                .saturating_add(64 * 1024)
        } else {
            body.plaintext_size
        };
        Some(BodyImageBounds {
            protected_bytes,
            decoded_upper_bound,
        })
    }

    #[cfg(test)]
    pub(crate) fn body_image_artifacts_for_test(&self, body: BodyIx) -> Vec<[u8; 32]> {
        self.bodies
            .slot(body)
            .and_then(|slot| slot.value.image.material())
            .map(|material| {
                std::iter::once(&material.checkpoint)
                    .chain(&material.delta_tail)
                    .map(|reference| reference.hash)
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn body_presence(&self, key: &BodyKey) -> BodyImagePresence {
        let Some(body) = self.bodies.body_ix(key) else {
            return BodyImagePresence::Absent;
        };
        let Some(slot) = self.bodies.slot(body) else {
            return BodyImagePresence::Absent;
        };
        if slot.value.image.is_readable() {
            BodyImagePresence::Readable {
                body,
                image: slot.value.image_id,
            }
        } else {
            BodyImagePresence::Opaque {
                image: slot.value.image_id,
            }
        }
    }

    /// Resolve one exact Body slot without populating a cache. The returned
    /// image has passed content-address, epoch, AEAD, canonical artifact,
    /// final-Version, mutation-model, size-hint, and immutable-address checks.
    pub fn resolve_body_image(
        &self,
        body: BodyIx,
    ) -> Result<Arc<fabric::BodySnapshot>, BodyImageFailure> {
        let slot = self
            .bodies
            .slot(body)
            .ok_or(BodyImageFailure::MaterialUnavailable)?;
        match &slot.value.image {
            SnapshotImage::Resident(snapshot) => Ok(Arc::new(snapshot.clone())),
            SnapshotImage::Pending { hot, material } => {
                if let Some(snapshot) = hot
                    .lock()
                    .map_err(|_| BodyImageFailure::MaterialUnavailable)?
                    .as_ref()
                    .cloned()
                {
                    return Ok(snapshot);
                }
                self.resolver
                    .as_ref()
                    .ok_or(BodyImageFailure::MaterialUnavailable)?
                    .resolve(&slot.key, &slot.value.binding, material)
            }
            SnapshotImage::Cold(material) => self
                .resolver
                .as_ref()
                .ok_or(BodyImageFailure::MaterialUnavailable)?
                .resolve(&slot.key, &slot.value.binding, material),
            SnapshotImage::Opaque(_) => Err(BodyImageFailure::Opaque),
        }
    }

    fn resolve_body_key(
        &self,
        key: &BodyKey,
    ) -> Result<Arc<fabric::BodySnapshot>, BodyImageFailure> {
        let body = self
            .bodies
            .body_ix(key)
            .ok_or(BodyImageFailure::MaterialUnavailable)?;
        self.resolve_body_image(body)
    }

    /// Lossy compatibility read. Exact callers must use `body_presence` and
    /// `resolve_body_image`; only those APIs preserve absence versus opaque,
    /// key-unavailable, corrupt, capacity, and unavailable-material outcomes.
    pub fn read(&self, key: &BodyKey) -> Option<Vec<u8>> {
        self.resolve_body_key(key).ok()?.read()
    }

    pub fn read_collaborative(
        &self,
        key: &BodyKey,
    ) -> Result<fabric::CollaborativeView, fabric::projection::Failure> {
        let Some(body) = self.bodies.get(key) else {
            return Err(fabric::projection::Failure::NotCollaborative);
        };
        match &body.image {
            SnapshotImage::Resident(snapshot) => snapshot.read_collaborative(),
            SnapshotImage::Cold(_) | SnapshotImage::Pending { .. } => self
                .resolve_body_key(key)
                .map_err(|_| fabric::projection::Failure::NotCollaborative)?
                .read_collaborative(),
            SnapshotImage::Opaque(_) => Err(fabric::projection::Failure::NotCollaborative),
        }
    }

    pub fn body_version(&self, key: &BodyKey) -> Option<fabric::Version> {
        let body = self.bodies.get(key)?;
        match &body.image {
            SnapshotImage::Resident(snapshot) => snapshot.version().ok(),
            SnapshotImage::Cold(material) | SnapshotImage::Pending { material, .. } => {
                Some(material.version.clone())
            }
            SnapshotImage::Opaque(_) => None,
        }
    }

    /// Mint an anchor from a caller-supplied, already-governed exact image.
    /// This performs no object I/O and does not populate a cache; Runtime can
    /// keep its cache pin while Replica alone derives the private Fabric key.
    pub fn anchor_in_resolved_image(
        key: &BodyKey,
        image: &fabric::BodySnapshot,
        path: &str,
        position: u64,
    ) -> Result<Option<fabric::Anchor>, fabric::projection::Failure> {
        image.try_anchor(&fabric_key(key), path, position).map(Some)
    }

    /// Resolve an anchor from a caller-supplied governed exact image without
    /// resolving material again or collapsing import/schema failures to drift.
    pub fn resolve_anchor_in_resolved_image(
        key: &BodyKey,
        image: &fabric::BodySnapshot,
        anchor: &fabric::Anchor,
    ) -> Result<fabric::AnchorResolution, fabric::projection::Failure> {
        image.try_resolve(&fabric_key(key), anchor)
    }

    pub fn anchor(&self, key: &BodyKey, path: &str, position: u64) -> Option<fabric::Anchor> {
        let body = self.bodies.get(key)?;
        match &body.image {
            SnapshotImage::Resident(snapshot) => snapshot.anchor(&fabric_key(key), path, position),
            SnapshotImage::Cold(_) | SnapshotImage::Pending { .. } => self
                .resolve_body_key(key)
                .ok()?
                .anchor(&fabric_key(key), path, position),
            SnapshotImage::Opaque(_) => None,
        }
    }

    pub fn resolve_anchor(
        &self,
        key: &BodyKey,
        anchor: &fabric::Anchor,
    ) -> fabric::AnchorResolution {
        self.bodies
            .get(key)
            .and_then(|body| match &body.image {
                SnapshotImage::Resident(snapshot) => {
                    Some(snapshot.resolve(&fabric_key(key), anchor))
                }
                SnapshotImage::Cold(_) | SnapshotImage::Pending { .. } => self
                    .resolve_body_key(key)
                    .ok()
                    .map(|snapshot| snapshot.resolve(&fabric_key(key), anchor)),
                SnapshotImage::Opaque(_) => None,
            })
            .unwrap_or(fabric::AnchorResolution::Drifted)
    }

    pub fn binding(&self, key: &BodyKey) -> Option<&BodyBinding> {
        self.bodies.get(key).map(|body| &body.binding)
    }

    pub fn body_stamp(&self, key: &BodyKey) -> Option<Vec<u8>> {
        self.bodies.get(key).map(|body| body.stamp.to_vec())
    }

    /// Canonical payload bytes retained by one readable Body image.
    pub fn body_payload_bytes(&self, key: &BodyKey) -> Option<u64> {
        self.bodies.get(key).map(|body| body.plaintext_size)
    }

    /// Drop pre-durability hot fallbacks after the exact candidate is committed
    /// and its Corpus has been validated. Physical cooling changes no semantic
    /// publication coordinate; later reads resolve the same signed closure.
    pub fn release_pending_body_images(&self, changed: &[BodyKey]) -> usize {
        let mut released = 0usize;
        for key in changed {
            let Some(body) = self.bodies.get(key) else {
                continue;
            };
            let SnapshotImage::Pending { hot, .. } = &body.image else {
                continue;
            };
            if hot.lock().is_ok_and(|mut hot| hot.take().is_some()) {
                released = released.saturating_add(1);
            }
        }
        released
    }

    pub fn body_keys(&self) -> Vec<BodyKey> {
        self.bodies.keys().map(|key| key.as_ref().clone()).collect()
    }

    /// Resolve one durable Body key to its stable slot in this publication.
    pub fn body_ix(&self, key: &BodyKey) -> Option<BodyIx> {
        self.bodies.body_ix(key)
    }

    /// Resolve a publication-local slot back to its durable Body key.
    pub fn body_key(&self, body: BodyIx) -> Option<&BodyKey> {
        self.bodies.slot(body).map(|slot| slot.key.as_ref())
    }

    /// Number of readable Bodies in this immutable generation, without
    /// cloning their keys or walking schema membership.
    pub fn body_count(&self) -> u64 {
        u64::try_from(self.bodies.len()).unwrap_or(u64::MAX)
    }

    /// Number of readable Bodies at one exact World/schema/version coordinate.
    ///
    /// This reads compact schema-posting metadata and never walks the primary
    /// Body directory. It is the admission-count seam used before invoking a
    /// World extractor for a publication build.
    pub fn body_count_with_schema_version(
        &self,
        world: &WorldId,
        schema: &SchemaId,
        version: u32,
    ) -> u64 {
        self.schema_bodies
            .get(&(world.clone(), schema.clone(), version))
            .map(|bodies| u64::try_from(bodies.len()).unwrap_or(u64::MAX))
            .unwrap_or(0)
    }

    /// Exact canonical readable payload bytes at one source coordinate.
    pub fn body_payload_bytes_with_schema_version(
        &self,
        world: &WorldId,
        schema: &SchemaId,
        version: u32,
    ) -> u64 {
        self.schema_payload_bytes
            .get(&(world.clone(), schema.clone(), version))
            .copied()
            .unwrap_or(0)
    }

    /// Portable commitment to the exact readable source set used by a World
    /// corpus.
    ///
    /// The digest is independent of publication-local BodyIx allocation. It
    /// commits sorted source coordinates and, for every readable source Body,
    /// its durable identity, immutable binding, and exact version stamp. A
    /// local corpus image can therefore be reused across process activations
    /// only when the portable PublicationId and this digest both match.
    pub fn source_fingerprint(&self, world: &WorldId, sources: &[(SchemaId, u32)]) -> [u8; 32] {
        fn field(hasher: &mut blake3::Hasher, value: &[u8]) {
            hasher.update(&u64::try_from(value.len()).unwrap_or(u64::MAX).to_be_bytes());
            hasher.update(value);
        }

        let mut sources = sources.to_vec();
        sources.sort();
        sources.dedup();
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"lait/read-snapshot/source-fingerprint/1\0");
        field(&mut hasher, world.as_bytes());
        hasher.update(
            &u64::try_from(sources.len())
                .unwrap_or(u64::MAX)
                .to_be_bytes(),
        );
        for (schema, version) in sources {
            field(&mut hasher, schema.as_bytes());
            hasher.update(&version.to_be_bytes());
            let members = self
                .schema_bodies
                .get(&(world.clone(), schema.clone(), version));
            hasher.update(
                &members
                    .map(|members| u64::try_from(members.len()).unwrap_or(u64::MAX))
                    .unwrap_or(0)
                    .to_be_bytes(),
            );
            if let Some(members) = members {
                for (_, body_ix) in members.iter() {
                    let Some(slot) = self.bodies.slot(*body_ix) else {
                        // A malformed in-memory directory cannot be produced
                        // by constructors, but must not alias a valid digest.
                        hasher.update(b"missing-body-slot");
                        continue;
                    };
                    hasher.update(&slot.key.body.as_bytes());
                    field(&mut hasher, slot.value.binding.schema.as_bytes());
                    hasher.update(&slot.value.binding.schema_version.to_be_bytes());
                    field(&mut hasher, slot.value.binding.encoding.as_bytes());
                    hasher.update(&[slot.value.binding.mutation_model]);
                    hasher.update(&slot.value.stamp);
                }
            }
        }
        *hasher.finalize().as_bytes()
    }

    /// Conservative retained-read-image bytes for O(1) cursor admission.
    pub fn retained_bytes_estimate(&self) -> u64 {
        self.retained_bytes_estimate
    }

    /// Body keys at one exact World/schema/version coordinate.
    pub fn body_keys_with_schema_version(
        &self,
        world: &WorldId,
        schema: &SchemaId,
        version: u32,
    ) -> Vec<BodyKey> {
        self.schema_bodies
            .get(&(world.clone(), schema.clone(), version))
            .map(|keys| {
                keys.iter()
                    .filter_map(|(_, body_ix)| self.body_key(*body_ix).cloned())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Body keys for every readable version of one World schema. The work is
    /// proportional to schema versions and returned keys, never all Bodies.
    pub fn body_keys_with_schema(&self, world: &WorldId, schema: &SchemaId) -> Vec<BodyKey> {
        self.schema_bodies
            .iter()
            .filter(|((candidate_world, candidate_schema, _), _)| {
                candidate_world == world && candidate_schema == schema
            })
            .flat_map(|(_, keys)| {
                keys.iter()
                    .filter_map(|(_, body_ix)| self.body_key(*body_ix).cloned())
            })
            .collect()
    }

    /// One canonical page across every readable version of a World schema.
    /// Each version seeks directly into its compact sorted leaves; the small
    /// set of version-local pages is merged by durable BodyKey order. No Body
    /// outside this exact immutable publication is inspected.
    pub fn body_keys_page_with_schema(
        &self,
        world: &WorldId,
        schema: &SchemaId,
        after: Option<&BodyKey>,
        limit: usize,
    ) -> Vec<BodyKey> {
        if limit == 0 {
            return Vec::new();
        }
        let mut candidates = self
            .schema_bodies
            .iter()
            .filter(|((candidate_world, candidate_schema, _), _)| {
                candidate_world == world && candidate_schema == schema
            })
            .flat_map(|(_, keys)| {
                let after_body = after.map(|key| &key.body);
                keys.keys_page_after(after_body, limit)
                    .into_iter()
                    .filter_map(|body_id| keys.get(&body_id).copied())
                    .filter_map(|body_ix| self.body_key(body_ix).cloned())
            })
            .collect::<Vec<_>>();
        candidates.sort();
        candidates.dedup();
        candidates.truncate(limit);
        candidates
    }

    pub fn content_descriptor(
        &self,
        content: &crate::content::ContentRef,
    ) -> Option<crate::content::ContentDescriptor> {
        self.content.get(&content.content_id).cloned()
    }
}

impl GenerationReader {
    fn indexed_generation(&self, root: &[u8; 32]) -> Result<Option<IndexedGeneration>, Failure> {
        let Some(store) = self.store.as_ref() else {
            return Ok(None);
        };
        let Some(value) = crate::index::lookup(
            &ReaderNodes(store),
            self.generation_index_root,
            &generation_index_key(root),
        )
        .map_err(|error| integrity_cause(Defect::Index, "look up durable generation", error))?
        else {
            return Ok(None);
        };
        let indexed: IndexedGeneration = postcard::from_bytes(&value).map_err(|error| {
            integrity_cause(Defect::Encoding, "decode durable generation index", error)
        })?;
        if &indexed.root != root
            || indexed.object.len == 0
            || indexed.footprint.reconstruction_depth == 0
            || postcard::to_stdvec(&indexed).ok().as_deref() != Some(value.as_slice())
        {
            return Err(Failure::Integrity(Defect::Encoding));
        }
        indexed.footprint.validate()?;
        Ok(Some(indexed))
    }

    /// Return authenticated admission metadata for one exact historical root.
    ///
    /// The lookup traverses only the persistent generation index. It does not
    /// read the generation delta, open a causal artifact, or reconstruct a
    /// Body, so callers can reserve the exact historical snapshot and
    /// extractor envelope before starting size-proportional work.
    pub fn generation_footprint(
        &self,
        root: &[u8; 32],
    ) -> Result<Option<GenerationFootprint>, Failure> {
        Ok(self
            .indexed_generation(root)?
            .map(|indexed| indexed.footprint))
    }

    #[cfg(any(test, feature = "scale-fixtures"))]
    #[doc(hidden)]
    pub fn generation_delta_object_for_test(
        &self,
        root: &[u8; 32],
    ) -> Result<Option<([u8; 32], u64)>, Failure> {
        Ok(self
            .indexed_generation(root)?
            .map(|indexed| (indexed.object.hash, indexed.object.len)))
    }

    #[cfg(any(test, feature = "scale-fixtures"))]
    #[doc(hidden)]
    pub fn generation_index_root_for_test(&self) -> Option<[u8; 32]> {
        self.generation_index_root.map(|root| root.hash)
    }

    fn generation_delta(&self, root: &[u8; 32]) -> Result<Option<GenerationDelta>, Failure> {
        let Some(store) = self.store.as_ref() else {
            return Ok(None);
        };
        let Some(indexed) = self.indexed_generation(root)? else {
            return Ok(None);
        };
        let bytes = store.read_object(&indexed.object).map_err(|error| {
            integrity_cause(Defect::Encoding, "read durable generation delta", error)
        })?;
        let delta: GenerationDelta = postcard::from_bytes(&bytes).map_err(|error| {
            integrity_cause(Defect::Encoding, "decode durable generation delta", error)
        })?;
        if delta.format_version != GENERATION_DELTA_FORMAT_VERSION || &delta.root != root {
            return Err(Failure::Integrity(Defect::Encoding));
        }
        Ok(Some(delta))
    }

    fn generation_artifact(&self, reference: &ArtifactRef) -> Result<Artifact, Failure> {
        let store = self.store.as_ref().ok_or(Failure::Poisoned)?;
        let object = Object {
            hash: reference.hash,
            len: reference.len,
        };
        let envelope = store.read_object(&object).map_err(|error| {
            integrity_cause(Defect::CorruptMaterial, "read generation artifact", error)
        })?;
        let epoch = mechanics::authorization::body_epoch_id(&envelope)
            .ok_or(Failure::Integrity(Defect::CorruptMaterial))?;
        if epoch != reference.epoch {
            return Err(Failure::Integrity(Defect::CorruptMaterial));
        }
        let opening = self
            .keys
            .as_ref()
            .and_then(|keys| keys.opening_key(&epoch))
            .ok_or(Failure::Integrity(Defect::MissingMaterial))?;
        open_artifact(&opening, &envelope).map_err(|_| Failure::Integrity(Defect::CorruptMaterial))
    }

    fn body_from_causal_material(
        &self,
        key: &BodyKey,
        material: &CausalMaterial,
    ) -> Result<fabric::BodySnapshot, Failure> {
        material
            .validate()
            .map_err(|_| Failure::Integrity(Defect::CorruptMaterial))?;
        let mut engine = Engine::new();
        for reference in std::iter::once(&material.checkpoint).chain(&material.delta_tail) {
            let artifact = self.generation_artifact(reference)?;
            let status = engine
                .import_artifact(&fabric_key(key), &artifact)
                .map_err(|_| Failure::Integrity(Defect::CorruptMaterial))?;
            if status.pending {
                return Err(Failure::Integrity(Defect::CorruptMaterial));
            }
        }
        let version = engine
            .version(&fabric_key(key))
            .map_err(|_| Failure::Integrity(Defect::CorruptMaterial))?;
        if version != material.version {
            return Err(Failure::Integrity(Defect::CorruptMaterial));
        }
        engine
            .body_snapshot(&fabric_key(key))
            .map_err(|error| {
                integrity_cause(
                    Defect::Encoding,
                    "project body from durable causal material",
                    error,
                )
            })?
            .ok_or(Failure::Integrity(Defect::MissingMaterial))
    }

    /// Reconstruct one exact durable generation without a Replica writer
    /// borrow. The returned snapshot is immutable and structurally shared by
    /// Runtime once installed in its generation cache.
    pub fn read_generation(&self, root: &[u8; 32]) -> Result<Option<ReadSnapshot>, Failure> {
        if self.current.root == *root {
            return Ok(Some((*self.current).clone()));
        }
        let expected_depth = match self.indexed_generation(root)? {
            Some(indexed) => indexed.footprint.reconstruction_depth,
            None => return Ok(None),
        };
        let mut cursor = *root;
        let mut seen = BTreeSet::new();
        let mut deltas = Vec::new();
        loop {
            if deltas.len()
                >= usize::try_from(expected_depth)
                    .unwrap_or(usize::MAX)
                    .min(usize::try_from(MAX_GENERATION_RECONSTRUCTION_DEPTH).unwrap_or(usize::MAX))
            {
                return Err(Failure::Integrity(Defect::Encoding));
            }
            if !seen.insert(cursor) {
                return Err(Failure::Integrity(Defect::Encoding));
            }
            let Some(delta) = self.generation_delta(&cursor)? else {
                return Ok(None);
            };
            let parent = delta.parent;
            deltas.push(delta);
            let Some(parent) = parent else {
                break;
            };
            cursor = parent;
        }
        if deltas.len() != usize::try_from(expected_depth).unwrap_or(usize::MAX) {
            return Err(Failure::Integrity(Defect::Encoding));
        }
        let frontier = deltas
            .first()
            .map(|delta| delta.frontier)
            .ok_or(Failure::Integrity(Defect::Encoding))?;
        // A hot Body may occur in many deltas between the target and its
        // baseline. Only the first occurrence is visible at the requested
        // generation. Selecting it before pinning keys avoids retaining or
        // requiring superseded key epochs and causal closures.
        let mut selected = BTreeMap::<BodyKey, &ArchivedBody>::new();
        for delta in &deltas {
            for archived in &delta.changed {
                selected.entry(archived.key.clone()).or_insert(archived);
            }
        }
        let cold_materials = selected.values().filter_map(|archived| {
            archived
                .present
                .then_some(())
                .filter(|_| archived.interpreted)
                .and_then(|_| archived.binding.as_ref())
                .and(archived.material.as_deref())
        });
        let resolver = match (&self.store, &self.keys) {
            (Some(store), Some(keys)) => Some(
                pin_body_image_resolver(store.clone(), keys, None, cold_materials)
                    .map_err(|_| Failure::Integrity(Defect::MissingMaterial))?,
            ),
            _ => None,
        };
        let mut bodies = BodyDirectory::default();
        for archived in selected.values() {
            if !archived.present {
                continue;
            }
            let binding = archived
                .binding
                .as_ref()
                .ok_or(Failure::Integrity(Defect::Encoding))?;
            let stamp = snapshot_stamp(&archived.stamp);
            let body = if !archived.interpreted {
                SnapshotBody::opaque(
                    &archived.key,
                    binding.clone(),
                    stamp,
                    archived.material.clone(),
                )
            } else {
                let material = archived
                    .material
                    .as_ref()
                    .ok_or(Failure::Integrity(Defect::MissingMaterial))?;
                SnapshotBody::cold(&archived.key, binding.clone(), stamp, material.clone())
            };
            bodies.insert(Arc::new(archived.key.clone()), body);
        }
        drop(selected);
        let mut content = imbl::OrdMap::new();
        for delta in deltas.into_iter().rev() {
            for descriptor in delta.descriptors {
                content.insert(descriptor.content_ref().content_id, descriptor);
            }
            for content_id in delta.removed_descriptors {
                content.remove(&content_id);
            }
        }
        let retained_bytes_estimate = snapshot_directory_retained_estimate(&bodies);
        Ok(Some(ReadSnapshot {
            root: *root,
            frontier,
            resolver,
            schema_bodies: schema_body_index(&bodies),
            schema_payload_bytes: schema_payload_index(&bodies),
            bodies,
            declared_content: imbl::OrdMap::new(),
            content,
            retained_bytes_estimate,
        }))
    }
}

/// One retained head's raw material: `(transaction id, canonical protected-artifact pack,
/// transaction bytes)`.
type RetainedHead = ([u8; 32], Vec<u8>, Vec<u8>);
const RECENT_HEAD_ARTIFACTS: usize = 4_096;

/// One Body a reconciliation found divergent, and the head commitments the peer
/// advertises for it.
pub type DivergentBody = (BodyKey, Vec<[u8; 32]>);

/// The canonical Engine key for a Body: `BLAKE3(domain || world || 0x00 || body)`.
fn fabric_key(key: &BodyKey) -> Key {
    let mut h = blake3::Hasher::new();
    h.update(BODY_KEY_DOMAIN);
    h.update(key.world.as_bytes());
    h.update(&[0x00]);
    h.update(&key.body.as_bytes());
    Key::from_bytes(h.finalize().as_bytes().to_vec())
}

fn immutable_key_matches(
    key: &BodyKey,
    schema: &SchemaId,
    schema_version: u32,
    encoding: &EncodingId,
    canonical_value: &[u8],
) -> bool {
    crate::body::immutable_body_id(
        &key.world,
        schema,
        schema_version,
        encoding,
        canonical_value,
    ) == key.body
}

fn is_atomic_mutation(model: u8) -> bool {
    matches!(model, MUTATION_ATOMIC | MUTATION_IMMUTABLE_ATOMIC)
}

/// Prove that an interpreted immutable descriptor contains an atomic value at
/// the one address that value is allowed to occupy. This is deliberately
/// repeated at every trust transition (Contact, opaque upgrade, and reopen),
/// rather than relying on the path that originally retained the bytes.
fn validate_immutable_proof(
    key: &BodyKey,
    descriptor: &Descriptor,
    proof: &Engine,
) -> Result<(), Failure> {
    if descriptor.mutation_model != MUTATION_IMMUTABLE_ATOMIC {
        return Ok(());
    }
    let value = proof
        .read(&fabric_key(key))
        .ok_or(Failure::ImmutableConflict)?;
    immutable_key_matches(
        key,
        &descriptor.schema,
        descriptor.schema_version,
        &descriptor.encoding,
        &value,
    )
    .then_some(())
    .ok_or(Failure::ImmutableConflict)
}

/// Advance the Replica frontier from a commit's causal evidence.
fn advance(prev: ReplicaFrontier, causal: &[u8]) -> ReplicaFrontier {
    let mut h = blake3::Hasher::new();
    h.update(FRONTIER_DOMAIN);
    h.update(&prev.root);
    h.update(causal);
    ReplicaFrontier::new(
        *h.finalize().as_bytes(),
        prev.transaction_count.saturating_add(1),
    )
}

/// Move the published coordinate without claiming a transaction.
///
/// A content commit changes the signed root — a different content index, a
/// different catalog — but accepts no Body transaction. Both halves matter. If
/// the coordinate did not move, an honest Station that ingested a file and then
/// declared it would publish three different roots at one `(signer, frontier)`
/// and every peer applying the equivocation rule would flag it. If the count
/// moved, a peer comparing frontiers would read a Body transaction that never
/// happened.
///
/// This is sound for equivocation detection precisely because it is
/// deterministic: an honest signer's root always folds forward, so two
/// different roots still sharing a coordinate remain what the rule says they
/// are.
fn advance_published(prev: ReplicaFrontier, causal: &[u8]) -> ReplicaFrontier {
    let mut h = blake3::Hasher::new();
    h.update(FRONTIER_DOMAIN);
    h.update(&prev.root);
    h.update(b"published");
    h.update(causal);
    ReplicaFrontier::new(*h.finalize().as_bytes(), prev.transaction_count)
}

/// Advance a Body's chain frontier from the transaction that wrote it.
fn advance_chain(prev: ReplicaFrontier, tx: &[u8; 16]) -> ReplicaFrontier {
    let mut h = blake3::Hasher::new();
    h.update(BODY_CHAIN_DOMAIN);
    h.update(&prev.root);
    h.update(tx);
    ReplicaFrontier::new(
        *h.finalize().as_bytes(),
        prev.transaction_count.saturating_add(1),
    )
}

/// The deterministic atomic-conflict order: height first, then root bytes.
fn chain_order(a: &ReplicaFrontier, b: &ReplicaFrontier) -> std::cmp::Ordering {
    a.transaction_count
        .cmp(&b.transaction_count)
        .then_with(|| a.root.cmp(&b.root))
}

/// A per-transaction chain seed: entropy for the per-Body chain frontier,
/// sealed into every payload so both replicas derive the same
/// `resulting_frontier` for concurrent-write resolution. It is NOT the
/// transaction id (that is the full signed-envelope digest, known only after
/// signing).
fn mint_chain_seed() -> Result<[u8; 16], Failure> {
    let mut raw = [0u8; 16];
    getrandom::fill(&mut raw).map_err(|source| {
        tracing::error!(error = %source, "OS entropy unavailable while minting Body chain seed");
        Failure::Body(crate::body::Failure::Randomness)
    })?;
    Ok(raw)
}

#[allow(dead_code)]
fn space_bytes(space: &SpaceId) -> Option<[u8; 29]> {
    <[u8; 29]>::try_from(space.as_str().as_bytes()).ok()
}

fn descriptor_hash(d: &Descriptor) -> [u8; 32] {
    #[allow(
        clippy::expect_used,
        reason = "derived serialization of this validated descriptor is infallible"
    )]
    let bytes = postcard::to_stdvec(d).expect("postcard descriptor");
    *blake3::hash(&bytes).as_bytes()
}

fn tx_commitment(bytes: &[u8]) -> [u8; 32] {
    *blake3::hash(bytes).as_bytes()
}

fn record_stamp(record: &BodyRecord) -> Vec<u8> {
    let mut stamp = record.chain.root.to_vec();
    stamp.extend_from_slice(&record.chain.transaction_count.to_be_bytes());
    let mut commitments: Vec<[u8; 32]> =
        record.heads.iter().map(|head| head.tx_commitment).collect();
    commitments.sort_unstable();
    for commitment in commitments {
        stamp.extend_from_slice(&commitment);
    }
    stamp
}

/// The canonical digest of a transaction's staged operation set — the value
/// the authorization receipt binds as `effect_operations_digest`. Order-stable
/// (operations sort by `(BodyKey, canonical op bytes)`).
fn operations_digest(ops: &[(BodyKey, Op)]) -> [u8; 32] {
    let mut items: Vec<Vec<u8>> = ops
        .iter()
        .map(|(k, op)| {
            #[allow(
                clippy::expect_used,
                reason = "derived serialization of a validated Body operation is infallible"
            )]
            postcard::to_stdvec(&(k, op)).expect("postcard op")
        })
        .collect();
    items.sort();
    let mut h = blake3::Hasher::new();
    h.update(b"lait/operations-digest/1");
    h.update(&u64::try_from(items.len()).unwrap_or(u64::MAX).to_be_bytes());
    for it in items {
        h.update(&u64::try_from(it.len()).unwrap_or(u64::MAX).to_be_bytes());
        h.update(&it);
    }
    *h.finalize().as_bytes()
}

impl PreparedAction {
    #[cfg(test)]
    pub(crate) fn context_cardinality_for_test(&self) -> (usize, usize) {
        let changed = match self.state.as_ref().expect("prepared action retains state") {
            PreparedActionState::Noop { .. } => 0,
            PreparedActionState::Mutation { data, .. } => data.new_records.len(),
        };
        (changed, self.snapshot.declaration_counts.len())
    }

    #[cfg(test)]
    pub(crate) fn simulate_rollback_poison_for_test(&self) {
        self.rollback_poisoned.store(true, Ordering::Release);
    }

    fn content_descriptor(&self, id: &[u8; 32]) -> Option<crate::content::ContentDescriptor> {
        let store = self.snapshot.durable.as_ref()?;
        let value = crate::index::lookup(
            &ReaderNodes(store),
            self.snapshot.content_index_root,
            &crate::manifest::content_index_key(id),
        )
        .ok()??;
        crate::content::ContentDescriptor::decode_canonical(&value).ok()
    }

    /// The receipt that will become authoritative if this candidate is
    /// finalized.
    pub fn receipt(&self) -> Result<&RequestReceipt, Failure> {
        match self
            .state
            .as_ref()
            .ok_or(Failure::Integrity(Defect::MissingMaterial))?
        {
            PreparedActionState::Noop { receipt } => Ok(receipt),
            PreparedActionState::Mutation { data, .. } => Ok(&data.receipt),
        }
    }

    /// The exact read coordinate the durable finalize will publish.
    pub fn candidate_root(&self) -> Result<[u8; 32], Failure> {
        match self
            .state
            .as_ref()
            .ok_or(Failure::Integrity(Defect::MissingMaterial))?
        {
            PreparedActionState::Noop { .. } => Ok(self.parent_root),
            PreparedActionState::Mutation { data, .. } => Ok(data.candidate_root),
        }
    }

    /// Conservative additional read-image bytes before freezing this
    /// candidate over `prior`.
    ///
    /// This walks only the prepared change set and its already-built causal
    /// descriptors. It never exports or inflates a Body. The price covers the
    /// cloned 256-entry Body/schema directory leaves, their persistent-vector
    /// spines, descriptor paths, and a defensive bound over newly referenced
    /// protected material. Unchanged snapshot pages remain shared.
    pub fn candidate_snapshot_delta_bytes_estimate(
        &self,
        prior: &ReadSnapshot,
    ) -> Result<u64, Failure> {
        if prior.root != self.parent_root || prior.frontier != self.parent_frontier {
            return Err(Failure::ParentManifestUnavailable);
        }
        let PreparedActionState::Mutation { data, .. } = self
            .state
            .as_ref()
            .ok_or(Failure::Integrity(Defect::MissingMaterial))?
        else {
            return Ok(0);
        };
        Ok(data.new_records.values().fold(0u64, |total, record| {
            let material = record
                .as_ref()
                .filter(|record| record.interpreted)
                .and_then(|record| record.causal.as_ref())
                .map(|material| {
                    std::iter::once(&material.checkpoint)
                        .chain(&material.delta_tail)
                        .fold(0u64, |bytes, artifact| bytes.saturating_add(artifact.len))
                        .saturating_mul(2)
                        .saturating_add(64 * 1024)
                })
                .unwrap_or(0);
            total.saturating_add(128 * 1024).saturating_add(material)
        }))
    }

    /// Freeze the prepared candidate by replacing only touched persistent-map
    /// paths. `prior` must be the Replica's current committed generation; a
    /// historical image is never silently advanced as though it were current.
    pub fn candidate_snapshot(&self, prior: &ReadSnapshot) -> Result<ReadSnapshot, Failure> {
        if prior.root != self.parent_root || prior.frontier != self.parent_frontier {
            return Err(Failure::ParentManifestUnavailable);
        }
        let PreparedActionState::Mutation { data, .. } = self
            .state
            .as_ref()
            .ok_or(Failure::Integrity(Defect::MissingMaterial))?
        else {
            return Ok(prior.clone());
        };
        let PreparedMutation {
            new_records,
            next_frontier,
            declared,
            candidate_root,
            ..
        } = data;

        let resolver = match (&self.snapshot.durable, &self.snapshot.keys) {
            (Some(store), Some(keys)) => {
                let materials = new_records.values().filter_map(|record| {
                    record
                        .as_ref()
                        .filter(|record| record.interpreted)
                        .and_then(|record| record.causal.as_deref())
                });
                Some(
                    pin_body_image_resolver(
                        store.clone(),
                        keys,
                        prior.resolver.as_deref(),
                        materials,
                    )
                    .map_err(|_| Failure::BodyKeyUnavailable)?,
                )
            }
            _ => prior.resolver.clone(),
        };

        let mut bodies = prior.bodies.clone();
        let mut schema_bodies = prior.schema_bodies.clone();
        let mut schema_payload_bytes = prior.schema_payload_bytes.clone();
        let mut retained_bytes_estimate = prior.retained_bytes_estimate;
        for (key, record) in new_records {
            let shared_key = if let Some((held, prior_body)) = prior.bodies.get_key_value(key) {
                retained_bytes_estimate = retained_bytes_estimate
                    .saturating_sub(snapshot_body_retained_estimate(prior_body));
                if prior_body.image.is_readable() {
                    remove_schema_body(&mut schema_bodies, key, &prior_body.binding);
                    adjust_schema_payload(&mut schema_payload_bytes, key, prior_body, false);
                }
                held.clone()
            } else {
                Arc::new(key.clone())
            };
            let frozen = record
                .as_ref()
                .map(|record| -> Result<SnapshotBody, Failure> {
                    let stamp = snapshot_stamp(&record_stamp(record));
                    if !record.interpreted {
                        return Ok(SnapshotBody::opaque(
                            key,
                            record.binding.clone(),
                            stamp,
                            record.causal.clone(),
                        ));
                    }
                    let body = lock_fabric(&self.fabric)
                        .body_snapshot(&fabric_key(key))
                        .map_err(Failure::Engine)?
                        .ok_or(Failure::Integrity(Defect::MissingMaterial))?;
                    if self.snapshot.durable.is_some() {
                        let material = record
                            .causal
                            .clone()
                            .ok_or(Failure::Integrity(Defect::MissingMaterial))?;
                        Ok(SnapshotBody::pending(
                            key,
                            record.binding.clone(),
                            stamp,
                            material,
                            body,
                        ))
                    } else {
                        Ok(SnapshotBody::resident(
                            key,
                            record.binding.clone(),
                            stamp,
                            body,
                        ))
                    }
                });
            match frozen.transpose()? {
                Some(body) => {
                    retained_bytes_estimate = retained_bytes_estimate
                        .saturating_add(snapshot_body_retained_estimate(&body));
                    let binding = body.binding.clone();
                    let readable = body.image.is_readable();
                    if readable {
                        adjust_schema_payload(&mut schema_payload_bytes, key, &body, true);
                    }
                    bodies.insert(shared_key, body);
                    if readable {
                        let body_ix = bodies
                            .body_ix(key)
                            .ok_or(Failure::Integrity(Defect::Encoding))?;
                        insert_schema_body(&mut schema_bodies, body_ix, key, &binding);
                    }
                }
                None => {
                    bodies.remove(key);
                }
            }
        }

        // Apply declaration refcount deltas to the persistent descriptor map.
        // This is proportional to changed declarations; no Space-wide Body or
        // content scan sits on the user-action path.
        let mut declared_content = prior.declared_content.clone();
        let mut content = prior.content.clone();
        let mut count_deltas: BTreeMap<[u8; 32], (u64, u64)> = BTreeMap::new();
        for (key, record) in new_records {
            let old = prior
                .declared_content
                .get(key)
                .map(|refs| refs.as_ref())
                .unwrap_or_default();
            let next = if record.is_none() {
                &[][..]
            } else {
                declared.get(key).map(Vec::as_slice).unwrap_or(old)
            };
            if next.is_empty() {
                declared_content.remove(key);
            } else {
                declared_content.insert(key.clone(), Arc::from(next));
            }
            for id in old {
                let delta = count_deltas.entry(*id).or_default();
                delta.0 = delta.0.saturating_add(1);
            }
            for id in next {
                let delta = count_deltas.entry(*id).or_default();
                delta.1 = delta.1.saturating_add(1);
            }
        }
        for (id, (removed, added)) in count_deltas {
            let current = self
                .snapshot
                .declaration_counts
                .get(&id)
                .copied()
                .unwrap_or(0);
            let next = current.saturating_sub(removed).saturating_add(added);
            if next == 0 {
                content.remove(&id);
            } else if !content.contains_key(&id) {
                let descriptor = self
                    .content_descriptor(&id)
                    .ok_or(Failure::Integrity(Defect::MissingMaterial))?;
                content.insert(id, descriptor);
            }
        }

        Ok(ReadSnapshot {
            root: *candidate_root,
            frontier: *next_frontier,
            resolver,
            bodies,
            schema_bodies,
            schema_payload_bytes,
            declared_content,
            content,
            retained_bytes_estimate,
        })
    }

    /// Durably publish the prepared transaction, then accept its live Fabric
    /// state. The caller may install the already-built read image only after
    /// this succeeds.
    pub fn finalize(
        mut self,
        replica: &mut Replica,
        ctx: &CommitContext<'_>,
    ) -> Result<RequestReceipt, Failure> {
        self.validate_parent(replica)?;
        let state = self
            .state
            .take()
            .ok_or(Failure::Integrity(Defect::MissingMaterial))?;
        let result = replica.finalize_prepared_action(ctx, state);
        self.in_flight.store(false, Ordering::Release);
        result
    }

    /// Durably publish and bind the already-built candidate snapshot to the
    /// new Journal root before releasing the exclusive Replica borrow.
    /// Attachment is infallible because durable truth has advanced already.
    pub fn finalize_attached(
        mut self,
        replica: &mut Replica,
        ctx: &CommitContext<'_>,
        snapshot: &ReadSnapshot,
    ) -> Result<RequestReceipt, Failure> {
        self.validate_parent(replica)?;
        let state = self
            .state
            .take()
            .ok_or(Failure::Integrity(Defect::MissingMaterial))?;
        let changed: Vec<BodyKey> = match &state {
            PreparedActionState::Mutation { data, .. } => {
                data.new_records.keys().cloned().collect()
            }
            PreparedActionState::Noop { .. } => Vec::new(),
        };
        let receipt = match replica.finalize_prepared_action(ctx, state) {
            Ok(receipt) => receipt,
            Err(error) => {
                self.in_flight.store(false, Ordering::Release);
                return Err(error);
            }
        };
        replica.attach_durable_body_image_root(snapshot);
        snapshot.release_pending_body_images(&changed);
        self.in_flight.store(false, Ordering::Release);
        Ok(receipt)
    }

    fn validate_parent(&self, replica: &Replica) -> Result<(), Failure> {
        if self.rollback_poisoned.load(Ordering::Acquire) || replica.poisoned {
            return Err(Failure::Poisoned);
        }
        if !Arc::ptr_eq(&self.in_flight, &replica.prepared_in_flight)
            || !self.in_flight.load(Ordering::Acquire)
        {
            return Err(Failure::MutationBusy);
        }
        let root = replica.current_manifest_root();
        if root != self.parent_root || replica.frontier != self.parent_frontier {
            return Err(Failure::ParentManifestUnavailable);
        }
        Ok(())
    }
}

impl Drop for PreparedAction {
    fn drop(&mut self) {
        if let Some(PreparedActionState::Mutation { fabric, .. }) = self.state.take() {
            if lock_fabric(&self.fabric).rollback(fabric).is_err() {
                self.rollback_poisoned.store(true, Ordering::Release);
                tracing::error!("prepared Replica action could not be rolled back");
            }
        }
        self.in_flight.store(false, Ordering::Release);
    }
}

impl Replica {
    fn current_manifest_root(&self) -> [u8; 32] {
        let root = self.manifest_root();
        if root == crate::transaction::NO_PARENT_ROOT {
            self.frontier.root
        } else {
            root
        }
    }

    fn mutation_available(&self) -> Result<(), Failure> {
        if self.poisoned || self.rollback_poisoned.load(Ordering::Acquire) {
            return Err(Failure::Poisoned);
        }
        if self.prepared_in_flight.load(Ordering::Acquire) {
            return Err(Failure::MutationBusy);
        }
        Ok(())
    }

    fn prepared_snapshot_context(
        &self,
        data: Option<&PreparedMutation>,
    ) -> PreparedSnapshotContext {
        let mut ids = BTreeSet::new();
        if let Some(data) = data {
            for key in data.new_records.keys() {
                ids.extend(
                    self.declared_content
                        .get(key)
                        .into_iter()
                        .flatten()
                        .copied(),
                );
                ids.extend(data.declared.get(key).into_iter().flatten().copied());
            }
        }
        let declaration_counts = ids
            .into_iter()
            .map(|id| {
                (
                    id,
                    self.declared_content_counts.get(&id).copied().unwrap_or(0),
                )
            })
            .collect();
        PreparedSnapshotContext {
            durable: self.durable.as_ref().map(Store::reader),
            keys: self.keys.clone(),
            content_index_root: self.content_index_root,
            declaration_counts,
        }
    }

    /// Build a Replica over a given Engine engine (no durability, no keys).
    fn from_engine(fabric: Engine) -> Self {
        Self {
            fabric: Arc::new(Mutex::new(fabric)),
            frontier: ReplicaFrontier::EMPTY,
            durable: None,
            // Nothing was read off a disk to build this, so nothing was
            // verified. `Replica::open` is the only thing that sets this.
            verified_at_ms: None,
            poisoned: false,
            rollback_poisoned: Arc::new(AtomicBool::new(false)),
            prepared_in_flight: Arc::new(AtomicBool::new(false)),
            keys: None,
            space: None,
            supported: SupportedSchemas::default(),
            quota: QuotaConfig::default(),
            bodies: RecordDirectory::default(),
            body_index_root: None,
            manifest_body_root: None,
            content_index_root: None,
            declared_content: BTreeMap::new(),
            declared_content_counts: BTreeMap::new(),
            declared_content_worlds: BTreeMap::new(),
            declared_content_retained_bytes: 0,
            pending_content: BTreeMap::new(),
            receipt_index_root: None,
            generation_index_root: None,
            generation_footprint: GenerationFootprint::default(),
            ownership_index_root: None,
            manifest_root_object: None,
            receipts: ReceiptDirectory::default(),
            receipt_cache: Arc::new(Mutex::new(ReceiptCache::default())),
            receipt_count: 0,
            receipt_material_bytes: 0,
            raw_material: BTreeMap::new(),
            recent_head_artifacts: BTreeMap::new(),
            recent_head_order: VecDeque::new(),
            checkpoint_jobs: Mutex::new(BTreeMap::new()),
        }
    }

    /// Build a Engine-backed Replica with **no** durable store (tests/scratch).
    pub fn loro() -> Self {
        Self::from_engine(Engine::new())
    }

    /// Construct the production mutable record directory without inflating
    /// any Body payload. Release-scale fixtures use this to retain the exact
    /// `BodyKey -> BodyRecord -> BodyHead/CausalMaterial` shape alongside the
    /// immutable publication and Corpus; it is deliberately unavailable to
    /// ordinary callers.
    #[cfg(any(test, feature = "scale-fixtures"))]
    #[allow(clippy::expect_used)]
    pub fn from_cold_body_records_for_scale(
        rows: impl IntoIterator<Item = (BodyKey, BodyBinding, fabric::Material)>,
    ) -> Self {
        let mut replica = Self::from_engine(Engine::new());
        let mut frontier_hasher = blake3::Hasher::new();
        frontier_hasher.update(b"lait/replica-scale/cold-records/1\0");
        let mut count = 0u64;
        for (key, binding, material) in rows {
            let material = Arc::new(material);
            let artifacts: smallvec::SmallVec<[ArtifactRef; 1]> =
                std::iter::once(material.checkpoint)
                    .chain(material.delta_tail.iter().copied())
                    .collect();
            let artifact_bytes = artifacts
                .iter()
                .fold(0u64, |bytes, reference| bytes.saturating_add(reference.len));
            let mut tx_hasher = blake3::Hasher::new();
            tx_hasher.update(b"lait/replica-scale/transaction/1\0");
            tx_hasher.update(key.world.as_bytes());
            tx_hasher.update(&key.body.as_bytes());
            let tx = *tx_hasher.finalize().as_bytes();
            frontier_hasher.update(&tx);
            count = count.saturating_add(1);
            replica.bodies.insert(
                key,
                BodyRecord {
                    binding,
                    chain: ReplicaFrontier::new(tx, 1),
                    heads: smallvec::smallvec![BodyHead {
                        tx,
                        descriptor_hash: tx,
                        tx_commitment: tx,
                        artifacts: None,
                        transaction: Some(Object { hash: tx, len: 512 }),
                        artifact_bytes,
                        tx_len: 512,
                    }],
                    interpreted: true,
                    causal: Some(material),
                },
            );
        }
        replica.frontier = ReplicaFrontier::new(*frontier_hasher.finalize().as_bytes(), count);
        replica.generation_footprint =
            GenerationFootprint::from_records(&replica.bodies).expect("bounded scale records");
        replica
    }

    /// Retain the ordinary durable metadata that accompanies the the record layout
    /// record mix: approximately two record Bodies per attributed operation
    /// and one content declaration per twenty Bodies. These are actual Replica
    /// directories, not fixture-side padding, so release RSS includes the same
    /// scope keys, receipt Bodies, content refs, and refcounts used in service.
    #[cfg(any(test, feature = "scale-fixtures"))]
    pub fn add_notes_record_operational_metadata_for_scale(&mut self) {
        const BODIES_PER_RECEIPT: usize = 2;
        const BODIES_PER_CONTENT_DECLARATION: usize = 20;
        let space = SpaceId::from_digest([0x91; 16]);
        let device = mechanics::actor::device_from_seed(&[0x92; 32]);
        let frontier = self.frontier;
        let manifest_root = self.current_manifest_root();
        let mut declarations = Vec::new();
        for (ordinal, (key, record)) in self.bodies.iter().enumerate() {
            if ordinal % BODIES_PER_RECEIPT == 0 {
                let transaction = record.head().map(|head| head.tx).unwrap_or([0; 32]);
                let receipt = RequestReceipt {
                    version: 2,
                    space: space.clone(),
                    world: key.world.clone(),
                    device: device.clone(),
                    request: key.body.as_bytes(),
                    payload_hash: transaction,
                    effect: Vec::new(),
                    bodies: vec![key.clone()],
                    frontier,
                    manifest_root,
                    implementation_digest: [0x93; 32],
                    extractor_schema_digest: [0x94; 32],
                    transaction,
                };
                let bytes = receipt.encode();
                self.receipt_count = self.receipt_count.saturating_add(1);
                self.receipt_material_bytes = self
                    .receipt_material_bytes
                    .saturating_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
                self.receipt_cache
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .insert(
                        receipt.scope_key(),
                        receipt,
                        Object {
                            hash: transaction,
                            len: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
                        },
                    );
            }
            if ordinal % BODIES_PER_CONTENT_DECLARATION == 0 {
                let mut hasher = blake3::Hasher::new();
                hasher.update(b"lait/replica-scale/content/1\0");
                hasher.update(key.world.as_bytes());
                hasher.update(&key.body.as_bytes());
                declarations.push((key.clone(), *hasher.finalize().as_bytes()));
            }
        }
        for (key, content) in declarations {
            self.replace_declared_content(&key, vec![content]);
        }
    }

    #[cfg(any(test, feature = "scale-fixtures"))]
    pub fn operational_metadata_counts_for_scale(&self) -> (usize, usize, usize) {
        (
            usize::try_from(self.receipt_count).unwrap_or(usize::MAX),
            self.declared_content.len(),
            self.declared_content_counts.len(),
        )
    }

    /// Freeze the exact cold publication shape from the mutable scale record
    /// directory while sharing every immutable causal descriptor Arc. The
    /// normal `read_snapshot` chooses cold images from `durable.is_some()`;
    /// this fixture-only seam models the state immediately after durable open
    /// without manufacturing an on-disk million-object store.
    #[cfg(any(test, feature = "scale-fixtures"))]
    pub fn cold_read_snapshot_for_scale(&self) -> ReadSnapshot {
        let mut builder = BodyDirectoryBuilder::default();
        for (key, record) in self.bodies.iter() {
            let Some(material) = record.causal.clone() else {
                continue;
            };
            let body = SnapshotBody::cold(
                key,
                record.binding.clone(),
                snapshot_stamp(&record_stamp(record)),
                material,
            );
            builder.push(Arc::new(key.clone()), body);
        }
        let bodies = builder.finish();
        let retained_bytes_estimate = snapshot_directory_retained_estimate(&bodies);
        ReadSnapshot {
            root: self.current_manifest_root(),
            frontier: self.frontier,
            resolver: None,
            schema_bodies: schema_body_index(&bodies),
            schema_payload_bytes: schema_payload_index(&bodies),
            bodies,
            declared_content: imbl::OrdMap::new(),
            content: imbl::OrdMap::new(),
            retained_bytes_estimate,
        }
    }

    /// Attach a mechanics-owned key source (required to seal local commits and
    /// open protected material).
    pub fn with_keys(mut self, keys: Arc<dyn BodyKeySource>) -> Self {
        self.keys = Some(keys);
        self
    }

    /// Declare the locally supported schemas (from the runtime's registry).
    /// Remote material outside this set takes the opaque branch.
    pub fn set_supported(&mut self, supported: SupportedSchemas) {
        self.supported = supported;
    }

    /// Configure the Space quotas (clamped to the protocol maxima; the
    /// configured limits persist with the next commit).
    pub fn set_quota(&mut self, quota: QuotaConfig) {
        self.quota = quota.clamped();
    }

    /// The effective quota configuration.
    pub fn quota(&self) -> &QuotaConfig {
        &self.quota
    }

    /// The material-ledger usage: canonical material bytes (protected
    /// envelopes + distinct transaction records + receipts) and Body count.
    pub fn usage(&self) -> (u64, u64) {
        let mut bytes = self.receipt_material_bytes;
        let mut tx_seen: std::collections::BTreeMap<[u8; 32], u64> = BTreeMap::new();
        for record in self.bodies.values() {
            for head in &record.heads {
                bytes = bytes.saturating_add(head.artifact_bytes);
                tx_seen.entry(head.tx).or_insert(head.tx_len);
            }
        }
        for len in tx_seen.values() {
            bytes = bytes.saturating_add(*len);
        }
        if self.durable.is_none() {
            for (receipt, _) in self.receipts.values() {
                bytes =
                    bytes.saturating_add(u64::try_from(receipt.encode().len()).unwrap_or(u64::MAX));
            }
        }
        (bytes, u64::try_from(self.bodies.len()).unwrap_or(u64::MAX))
    }

    /// The retained-unknown-World usage for one World: (bytes, bodies).
    pub fn opaque_usage(&self, world: &WorldId) -> (u64, u64) {
        let mut bytes: u64 = 0;
        let mut count: u64 = 0;
        for (key, record) in self.bodies.iter() {
            if !record.interpreted && &key.world == world {
                bytes = bytes.saturating_add(record.protected_total());
                count = count.saturating_add(1);
            }
        }
        (bytes, count)
    }

    /// Open the durable Replica at a journaled store root: run crash recovery,
    /// verify and load the canonical object graph (signed transactions,
    /// protected artifact references, receipts, manifest), and import only
    /// collaborative writer state. Every interpreted durable Body stays as a
    /// compact causal closure until a governed exact reader or mutation
    /// resolves one;
    /// a Body without local key material is retained opaquely. Missing or
    /// corrupt objects fail integrity validation without heuristic repair.
    pub fn open(
        root: impl Into<std::path::PathBuf>,
        keys: Arc<dyn BodyKeySource>,
    ) -> Result<Self, Failure> {
        let store = match Store::open(root) {
            Ok(s) => s,
            Err(journal::Failure::Integrity(defect)) => {
                return Err(Failure::Integrity(Defect::Store(defect)));
            }
            Err(e) => return Err(Failure::Durability(e)),
        };
        let mut engine = Engine::new();
        engine.use_external_collaborative_images();
        let mut replica = Self::from_engine(engine).with_keys(keys.clone());
        let Some(meta_bytes) = store
            .caller_meta()
            .map_err(|_| Failure::Integrity(Defect::Encoding))?
        else {
            // A store with no commit point has no object graph, but the pass
            // still ran: `Store::open` above drove `recover`, which validates
            // the required index and re-hashes every object it names. Over an
            // empty required set that succeeds trivially — and trivially
            // succeeding is still the check having been made, so it is stamped
            // like any other. Reporting a fresh store as never-verified would
            // leave the surface saying "unknown" until the first commit.
            replica.durable = Some(store);
            replica.verified_at_ms = Some(mechanics::wallclock::now_millis());
            return Ok(replica);
        };
        let (meta, receipt_ledger_complete) = decode_store_meta(&meta_bytes)?;
        replica.frontier = meta.frontier;
        replica.space = meta.space.clone();
        replica.quota = meta.quota.clamped();
        replica.body_index_root = meta.body_index_root;
        replica.manifest_body_root = meta.manifest_body_root;
        replica.content_index_root = meta.content_index_root;
        replica.receipt_index_root = meta.receipt_index_root;
        replica.receipt_count = meta.receipt_count;
        replica.receipt_material_bytes = meta.receipt_material_bytes;
        replica.generation_index_root = meta.generation_index_root;
        replica.ownership_index_root = meta.ownership_index_root;
        replica.manifest_root_object = meta.manifest_root;

        // Stream the catalogs rather than decoding one giant vector. Body
        // records retain only authenticated causal coordinates; protected
        // payloads are neither decrypted nor installed into the writer during
        // ordinary recovery, regardless of mutation model.
        let mut indexed_bodies: Vec<IndexedBody> = Vec::new();
        let mut decode_failure = false;
        crate::index::stream(&StoreNodes(&store), meta.body_index_root, &mut |entry| {
            if decode_failure {
                return;
            }
            match postcard::from_bytes::<IndexedBody>(&entry.value) {
                Ok(body) => indexed_bodies.push(body),
                Err(_) => decode_failure = true,
            }
        })
        .map_err(|_| Failure::Integrity(Defect::Index))?;
        if decode_failure {
            return Err(Failure::Integrity(Defect::Encoding));
        }

        let mut prior_receipt_material = Vec::new();
        if receipt_ledger_complete {
            if meta.receipt_index_root.map_or(0, |root| root.count) != meta.receipt_count {
                return Err(Failure::Integrity(Defect::Index));
            }
        } else {
            // One-time v2 representation migration. Current stores carry the
            // exact ledger in StoreMeta and never walk this index at open.
            let mut receipt_bytes = 0u64;
            let mut receipt_count = 0u64;
            let mut receipt_failure = None;
            crate::index::stream(&StoreNodes(&store), meta.receipt_index_root, &mut |entry| {
                if receipt_failure.is_some() {
                    return;
                }
                let result = (|| {
                    let indexed: IndexedReceipt = postcard::from_bytes(&entry.value)
                        .map_err(|_| Failure::Integrity(Defect::Encoding))?;
                    if postcard::to_stdvec(&indexed).ok().as_deref() != Some(entry.value.as_slice())
                        || receipt_index_key(&indexed.scope) != entry.key
                    {
                        return Err(Failure::Integrity(Defect::Index));
                    }
                    let bytes = store
                        .read_required_object_bounded(&indexed.object, MAX_RECEIPT_OBJECT_BYTES)
                        .map_err(|_| Failure::Integrity(Defect::Encoding))?;
                    let receipt = RequestReceipt::decode_canonical(&bytes)
                        .map_err(|_| Failure::Integrity(Defect::Encoding))?;
                    if receipt.scope_key() != indexed.scope {
                        return Err(Failure::Integrity(Defect::Index));
                    }
                    receipt_bytes = receipt_bytes
                        .saturating_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
                    receipt_count = receipt_count.saturating_add(1);
                    prior_receipt_material.push((indexed.object, bytes));
                    Ok(())
                })();
                if let Err(failure) = result {
                    receipt_failure = Some(failure);
                }
            })
            .map_err(|_| Failure::Integrity(Defect::Index))?;
            if let Some(failure) = receipt_failure {
                return Err(failure);
            }
            replica.receipt_count = receipt_count;
            replica.receipt_material_bytes = receipt_bytes;
        }

        for IndexedBody { key, mut record } in indexed_bodies {
            if record.heads.is_empty() {
                return Err(Failure::Integrity(Defect::MissingMaterial));
            }
            // Verify every constituent signed head while deferring all Body
            // artifact I/O/decryption to the publication-pinned resolver.
            let mut descriptors: Vec<Descriptor> = Vec::new();
            for head in &record.heads {
                let Some(tx_ref) = head.transaction else {
                    return Err(Failure::Integrity(Defect::MissingMaterial));
                };
                // The transaction record must decode and verify structurally.
                let tx_bytes = store
                    .read_object(&tx_ref)
                    .map_err(|_| Failure::Integrity(Defect::Encoding))?;
                let tx = Transaction::decode_canonical(&tx_bytes)
                    .map_err(|_| Failure::Integrity(Defect::Encoding))?;
                tx.verify()
                    .map_err(|_| Failure::Integrity(Defect::Encoding))?;
                if tx.id() != head.tx || tx_commitment(&tx_bytes) != head.tx_commitment {
                    return Err(Failure::Integrity(Defect::CorruptMaterial));
                }
                let descriptor = tx
                    .core
                    .descriptors
                    .iter()
                    .find(|descriptor| descriptor.key() == key)
                    .cloned()
                    .ok_or(Failure::Integrity(Defect::MissingMaterial))?;
                if descriptor_hash(&descriptor) != head.descriptor_hash
                    || descriptor.mutation_model != record.binding.mutation_model
                    || descriptor.resulting_frontier != record.chain
                        && is_atomic_mutation(record.binding.mutation_model)
                {
                    return Err(Failure::Integrity(Defect::CorruptMaterial));
                }
                if !descriptor
                    .artifact_refs()
                    .copied()
                    .eq(record.artifacts(head).copied())
                {
                    return Err(Failure::Integrity(Defect::CorruptMaterial));
                }
                descriptors.push(descriptor);
            }
            // A Body retained opaquely stays opaque at reopen: interpreting it
            // later requires explicit revalidation through the incorporation
            // path, never a silent flip on restart. A Body that WAS
            // interpreted must open again — if its epoch key has since gone
            // away it degrades to opaque (retained, unread) rather than
            // failing the whole store.
            let mut degraded = !record.interpreted;
            if record.interpreted
                && descriptors.iter().any(|descriptor| {
                    descriptor
                        .artifact_refs()
                        .any(|reference| keys.opening_key(&reference.epoch).is_none())
                })
            {
                degraded = true;
            }
            if record.interpreted {
                let material = record
                    .causal
                    .as_deref()
                    .ok_or(Failure::Integrity(Defect::MissingMaterial))?;
                material
                    .validate()
                    .map_err(|_| Failure::Integrity(Defect::CorruptMaterial))?;
                for reference in std::iter::once(&material.checkpoint).chain(&material.delta_tail) {
                    if keys.opening_key(&reference.epoch).is_none() {
                        degraded = true;
                        break;
                    }
                }
                // Exact model, canonical export size, final Version, and
                // immutable addressing are verified on first governed resolve.
            }
            if degraded {
                record.interpreted = false;
                // Durable opaque forwarding/revalidation reads the exact
                // signed refs on demand. Keeping packs here would eagerly read
                // and duplicate every unavailable protected payload at open.
            }
            replica.bodies.insert(key, record);
        }
        let mut recovered_footprint = GenerationFootprint::from_records(&replica.bodies)?;
        // Declarations live in the published catalog, so reopening recovers
        // them from the same place a peer would read them.
        let mut declared_failure = false;
        crate::index::stream(&StoreNodes(&store), meta.manifest_body_root, &mut |entry| {
            if declared_failure {
                return;
            }
            match crate::manifest::ManifestEntry::decode_canonical(&entry.value) {
                Ok(published) if !published.content_refs.is_empty() => {
                    replica.replace_declared_content(&published.key, published.content_refs);
                }
                Ok(_) => {}
                Err(_) => declared_failure = true,
            }
        })
        .map_err(|_| Failure::Integrity(Defect::Index))?;
        if declared_failure {
            return Err(Failure::Integrity(Defect::Encoding));
        }

        if let (Some(generation_root), Some(manifest)) =
            (replica.generation_index_root, replica.manifest_root_object)
        {
            let value = crate::index::lookup(
                &StoreNodes(&store),
                Some(generation_root),
                &generation_index_key(&manifest.hash),
            )
            .map_err(|error| {
                integrity_cause(Defect::Index, "look up current generation footprint", error)
            })?
            .ok_or(Failure::Integrity(Defect::Index))?;
            let indexed: IndexedGeneration = postcard::from_bytes(&value).map_err(|error| {
                integrity_cause(
                    Defect::Encoding,
                    "decode current generation footprint",
                    error,
                )
            })?;
            indexed.footprint.validate()?;
            if indexed.root != manifest.hash
                || indexed.object.len == 0
                || indexed.footprint.reconstruction_depth == 0
                || postcard::to_stdvec(&indexed).ok().as_deref() != Some(value.as_slice())
                || indexed.footprint.body_count != recovered_footprint.body_count
                || indexed.footprint.snapshot_retained_bytes
                    != recovered_footprint.snapshot_retained_bytes
            {
                return Err(Failure::Integrity(Defect::Index));
            }
            // Source aggregates describe readability at publication time.
            // Missing opening keys may make the reopened writer's live image a
            // strict subset; `recovered_footprint` is therefore the correct
            // base for its next commit, while exact historical readers retain
            // the authenticated published aggregate in this index entry.
            recovered_footprint.reconstruction_depth = indexed.footprint.reconstruction_depth;
            recovered_footprint.reconstruction_delta_bytes =
                indexed.footprint.reconstruction_delta_bytes;
            recovered_footprint.reconstruction_transient_bytes =
                indexed.footprint.reconstruction_transient_bytes;
        }
        replica.generation_footprint = recovered_footprint;

        replica.durable = Some(store);
        // This stamp covers eager control/index/transaction verification and
        // signed Body descriptors. Deferred payload failures remain typed per
        // Body and never retroactively poison unrelated placement.
        replica.verified_at_ms = Some(mechanics::wallclock::now_millis());
        // A version-2 indexed store has no ancestry index. Establish its
        // current committed state as generation zero before returning it to a
        // writer. That makes the pre-commit coordinate a Spec revision records
        // immediately queryable, while changing no World fact or Manifest.
        replica.persist_generation_baseline(&prior_receipt_material)?;
        Ok(replica)
    }

    fn persist_generation_baseline(
        &mut self,
        prior_receipt_material: &[(Object, Vec<u8>)],
    ) -> Result<(), Failure> {
        use crate::index::{self, IndexChange, NodeSink};

        if self.generation_index_root.is_some() || self.manifest_root_object.is_none() {
            return Ok(());
        }
        let root = self.manifest_root();
        let mut records = RecordDirectory::default();
        let mut added = Vec::new();
        for (key, held) in self.bodies.iter() {
            let mut record = held.clone();
            if !record.interpreted {
                record.replace_causal(None)?;
                records.insert(key.clone(), record);
                continue;
            }
            let prior = self.bodies.get(key).and_then(|body| body.causal.as_deref());
            let (material, artifacts) = self.next_causal_material(key, &record, prior, &added)?;
            record.replace_causal(Some(Arc::new(material)))?;
            added.extend(artifacts);
            records.insert(key.clone(), record);
        }
        let changed = records
            .iter()
            .map(|(key, record)| ArchivedBody {
                key: key.clone(),
                present: true,
                interpreted: record.interpreted,
                binding: Some(record.binding.clone()),
                stamp: record_stamp(record),
                material: record.causal.clone(),
            })
            .collect();
        let descriptors = self.snapshot_content().values().cloned().collect();
        let delta = GenerationDelta {
            format_version: GENERATION_DELTA_FORMAT_VERSION,
            root,
            parent: None,
            frontier: self.frontier,
            changed,
            descriptors,
            removed_descriptors: Vec::new(),
        };
        let delta_bytes = postcard::to_stdvec(&delta).map_err(|error| {
            integrity_cause(Defect::Encoding, "encode generation baseline", error)
        })?;
        let delta_ref = object_ref(&delta_bytes);
        let mut generation_footprint = GenerationFootprint::from_records(&records)?;
        generation_footprint.record_generation_delta(
            None,
            &delta,
            u64::try_from(delta_bytes.len()).unwrap_or(u64::MAX),
        )?;
        let indexed = IndexedGeneration {
            root,
            object: delta_ref,
            footprint: generation_footprint.clone(),
        };
        let indexed_bytes = postcard::to_stdvec(&indexed).map_err(|error| {
            integrity_cause(Defect::Encoding, "encode generation baseline index", error)
        })?;
        let mut sink = NodeSink::default();
        let (body_index_root, generation_index_root) = {
            let store = self.durable.as_ref().ok_or(Failure::Poisoned)?;
            let body_changes = records
                .iter()
                .map(|(key, record)| {
                    let indexed = IndexedBody {
                        key: key.clone(),
                        record: record.clone(),
                    };
                    let value = postcard::to_stdvec(&indexed)
                        .map_err(|_| Failure::Integrity(Defect::Encoding))?;
                    Ok(IndexChange {
                        key: body_index_key(key),
                        value: Some(value),
                    })
                })
                .collect::<Result<Vec<_>, Failure>>()?;
            let body_index_root = index::apply(
                &StoreNodes(store),
                self.body_index_root,
                body_changes,
                &mut sink,
            )
            .map_err(|error| {
                integrity_cause(Defect::Index, "apply generation baseline bodies", error)
            })?;
            let generation_index_root = index::apply(
                &StoreNodes(store),
                None,
                vec![IndexChange {
                    key: generation_index_key(&root),
                    value: Some(indexed_bytes),
                }],
                &mut sink,
            )
            .map_err(|error| {
                integrity_cause(Defect::Index, "apply generation baseline index", error)
            })?;
            (body_index_root, generation_index_root)
        };
        let mut ownership_changes = BTreeMap::<[u8; 32], (Object, OwnedObjectClass, i64)>::new();
        for record in records.values() {
            Self::adjust_ownership(
                &mut ownership_changes,
                Self::record_owned_objects(record)?,
                1,
            )?;
        }
        for (object, bytes) in prior_receipt_material {
            if object_ref(bytes) != *object {
                return Err(Failure::Integrity(Defect::CorruptMaterial));
            }
            added.push(bytes.clone());
            let mut owned = BTreeMap::new();
            Self::insert_owned(&mut owned, *object, OwnedObjectClass::DeferredReceipt)?;
            Self::adjust_ownership(&mut ownership_changes, owned, 1)?;
        }
        if let Some(manifest) = self.manifest_root_object {
            let mut owned = BTreeMap::new();
            Self::insert_owned(&mut owned, manifest, OwnedObjectClass::Eager)?;
            Self::adjust_ownership(&mut ownership_changes, owned, 1)?;
        }
        let mut generation_owned = BTreeMap::new();
        Self::insert_owned(&mut generation_owned, delta_ref, OwnedObjectClass::Eager)?;
        for archived in &delta.changed {
            for (hash, owned) in Self::material_owned_objects(archived.material.as_deref())? {
                match generation_owned.get(&hash) {
                    Some(held) if held == &owned => {}
                    Some(_) => return Err(Failure::Integrity(Defect::CorruptMaterial)),
                    None => {
                        generation_owned.insert(hash, owned);
                    }
                }
            }
        }
        Self::adjust_ownership(&mut ownership_changes, generation_owned, 1)?;
        let ownership_index_root = {
            let store = self.durable.as_ref().ok_or(Failure::Poisoned)?;
            let (root, eager_removed, deferred_removed) =
                Self::apply_ownership_changes(store, None, ownership_changes, &mut sink)?;
            if !eager_removed.is_empty() || !deferred_removed.is_empty() {
                return Err(Failure::Integrity(Defect::Index));
            }
            root
        };
        let meta = StoreMeta {
            format_version: STORE_META_FORMAT_VERSION,
            space: self.space.clone(),
            frontier: self.frontier,
            quota: self.quota,
            body_index_root,
            manifest_body_root: self.manifest_body_root,
            content_index_root: self.content_index_root,
            receipt_index_root: self.receipt_index_root,
            receipt_count: self.receipt_count,
            receipt_material_bytes: self.receipt_material_bytes,
            generation_index_root,
            ownership_index_root,
            manifest_root: self.manifest_root_object,
        };
        let meta_bytes = postcard::to_stdvec(&meta).map_err(|error| {
            integrity_cause(
                Defect::Encoding,
                "encode generation baseline store metadata",
                error,
            )
        })?;
        let roots: Vec<([u8; 32], u64)> = body_index_root
            .into_iter()
            .chain(self.manifest_body_root)
            .chain(self.content_index_root)
            .chain(self.receipt_index_root)
            .chain(generation_index_root)
            .map(|root| (root.hash, root.count))
            .collect();
        let lazy_roots: Vec<([u8; 32], u64)> = ownership_index_root
            .into_iter()
            .map(|root| (root.hash, root.count))
            .collect();
        added.push(delta_bytes);
        let deferred_hashes: BTreeSet<[u8; 32]> = records
            .values()
            .flat_map(|record| {
                record.causal.iter().flat_map(|material| {
                    std::iter::once(material.checkpoint.hash)
                        .chain(material.delta_tail.iter().map(|artifact| artifact.hash))
                })
            })
            .chain(prior_receipt_material.iter().map(|(object, _)| object.hash))
            .collect();
        let (deferred_added, added): (Vec<Vec<u8>>, Vec<Vec<u8>>) = added
            .into_iter()
            .partition(|bytes| deferred_hashes.contains(&object_ref(bytes).hash));
        let store = self.durable.as_mut().ok_or(Failure::Poisoned)?;
        store
            .commit_classified(
                &added,
                &[],
                journal::Deferred {
                    added: &deferred_added,
                    removed: &[],
                },
                journal::Index {
                    roots: &roots,
                    lazy_roots: &lazy_roots,
                    nodes: &sink.written,
                },
                meta_bytes,
            )
            .map_err(|failure| match failure {
                journal::Failure::OutcomeUnknown => Failure::OutcomeUnknown,
                journal::Failure::Integrity(defect) => Failure::Integrity(Defect::Store(defect)),
                other => Failure::Durability(other),
            })?;
        self.bodies = records;
        self.body_index_root = body_index_root;
        self.generation_index_root = generation_index_root;
        self.generation_footprint = generation_footprint;
        self.ownership_index_root = ownership_index_root;
        Ok(())
    }

    fn insert_owned(
        out: &mut BTreeMap<[u8; 32], (Object, OwnedObjectClass)>,
        object: Object,
        class: OwnedObjectClass,
    ) -> Result<(), Failure> {
        match out.get(&object.hash) {
            Some((held, held_class)) if held == &object && held_class == &class => Ok(()),
            Some(_) => Err(Failure::Integrity(Defect::CorruptMaterial)),
            None => {
                out.insert(object.hash, (object, class));
                Ok(())
            }
        }
    }

    /// One live Body record is one owner however often a closure repeats a
    /// content address across peer-head and local causal coordinates.
    fn record_owned_objects(
        record: &BodyRecord,
    ) -> Result<BTreeMap<[u8; 32], (Object, OwnedObjectClass)>, Failure> {
        let mut out = BTreeMap::new();
        for head in &record.heads {
            if let Some(r) = head.transaction {
                Self::insert_owned(&mut out, r, OwnedObjectClass::Eager)?;
            }
            for reference in record.artifacts(head) {
                Self::insert_owned(
                    &mut out,
                    artifact_object(reference),
                    OwnedObjectClass::DeferredArtifact {
                        epoch: reference.epoch,
                    },
                )?;
            }
        }
        if let Some(material) = &record.causal {
            for reference in std::iter::once(&material.checkpoint).chain(&material.delta_tail) {
                Self::insert_owned(
                    &mut out,
                    artifact_object(reference),
                    OwnedObjectClass::DeferredArtifact {
                        epoch: reference.epoch,
                    },
                )?;
            }
        }
        Ok(out)
    }

    fn material_owned_objects(
        material: Option<&CausalMaterial>,
    ) -> Result<BTreeMap<[u8; 32], (Object, OwnedObjectClass)>, Failure> {
        let mut out = BTreeMap::new();
        if let Some(material) = material {
            for reference in std::iter::once(&material.checkpoint).chain(&material.delta_tail) {
                Self::insert_owned(
                    &mut out,
                    artifact_object(reference),
                    OwnedObjectClass::DeferredArtifact {
                        epoch: reference.epoch,
                    },
                )?;
            }
        }
        Ok(out)
    }

    fn adjust_ownership(
        changes: &mut BTreeMap<[u8; 32], (Object, OwnedObjectClass, i64)>,
        objects: BTreeMap<[u8; 32], (Object, OwnedObjectClass)>,
        amount: i64,
    ) -> Result<(), Failure> {
        for (hash, (object, class)) in objects {
            match changes.get_mut(&hash) {
                Some((held, held_class, delta)) if *held == object && *held_class == class => {
                    *delta = delta
                        .checked_add(amount)
                        .ok_or(Failure::Integrity(Defect::Index))?;
                }
                Some(_) => return Err(Failure::Integrity(Defect::CorruptMaterial)),
                None => {
                    changes.insert(hash, (object, class, amount));
                }
            }
        }
        Ok(())
    }

    fn apply_ownership_changes(
        store: &Store,
        prior_root: Option<IndexRef>,
        changes: BTreeMap<[u8; 32], (Object, OwnedObjectClass, i64)>,
        sink: &mut crate::index::NodeSink,
    ) -> Result<(Option<IndexRef>, Vec<[u8; 32]>, Vec<[u8; 32]>), Failure> {
        let mut index_changes = Vec::with_capacity(changes.len());
        let mut eager_removed = Vec::new();
        let mut deferred_removed = Vec::new();
        for (hash, (object, class, delta)) in changes {
            if delta == 0 {
                continue;
            }
            let prior =
                crate::index::lookup(&StoreNodes(store), prior_root, &ownership_index_key(&hash))
                    .map_err(|_| Failure::Integrity(Defect::Index))?
                    .map(|bytes| {
                        let indexed: IndexedOwnership = postcard::from_bytes(&bytes)
                            .map_err(|_| Failure::Integrity(Defect::Encoding))?;
                        if indexed.object.hash != hash
                            || indexed.owners == 0
                            || postcard::to_stdvec(&indexed)
                                .map_err(|_| Failure::Integrity(Defect::Encoding))?
                                != bytes
                        {
                            return Err(Failure::Integrity(Defect::Index));
                        }
                        Ok(indexed)
                    })
                    .transpose()?;
            if let Some(prior) = &prior {
                if prior.object != object || prior.class != class {
                    return Err(Failure::Integrity(Defect::CorruptMaterial));
                }
            }
            let before = prior.as_ref().map_or(0, |entry| entry.owners);
            let after = if delta > 0 {
                before
                    .checked_add(
                        u64::try_from(delta).map_err(|_| Failure::Integrity(Defect::Index))?,
                    )
                    .ok_or(Failure::Integrity(Defect::Index))?
            } else {
                before
                    .checked_sub(delta.unsigned_abs())
                    .ok_or(Failure::Integrity(Defect::Index))?
            };
            let value = if after == 0 {
                match class {
                    OwnedObjectClass::Eager => eager_removed.push(hash),
                    OwnedObjectClass::DeferredArtifact { .. }
                    | OwnedObjectClass::DeferredReceipt => deferred_removed.push(hash),
                }
                None
            } else {
                Some(
                    postcard::to_stdvec(&IndexedOwnership {
                        object,
                        class,
                        owners: after,
                    })
                    .map_err(|_| Failure::Integrity(Defect::Encoding))?,
                )
            };
            index_changes.push(crate::index::IndexChange {
                key: ownership_index_key(&hash),
                value,
            });
        }
        let root = crate::index::apply(&StoreNodes(store), prior_root, index_changes, sink)
            .map_err(|_| Failure::Integrity(Defect::Index))?;
        Ok((root, eager_removed, deferred_removed))
    }

    /// What the signed manifest advertises for one Body, as distinct from the
    /// local record, which also names objects no peer should learn about.
    ///
    /// Returns an error rather than `None`. `None` is the encoding of "delete
    /// this Body from the catalog", so swallowing a bounds failure here would
    /// quietly drop a live Body out of the signed advertisement while the body
    /// index kept it — two catalogs disagreeing, and no one told.
    fn manifest_entry(
        key: &BodyKey,
        record: &BodyRecord,
        content_refs: Vec<[u8; 32]>,
    ) -> Result<ManifestEntry, Failure> {
        ManifestEntry::declaring(
            key.clone(),
            record
                .heads
                .iter()
                .map(|h| ManifestHead {
                    descriptor_hash: h.descriptor_hash,
                    transaction_commitment: h.tx_commitment,
                })
                .collect(),
            content_refs,
        )
        .map_err(|_| Failure::Integrity(Defect::Encoding))
    }

    /// Select an already-authorized key for a derived causal artifact. New
    /// local work normally uses the current sealing epoch. Incorporation can
    /// legitimately happen while no current sealing epoch is held, so it may
    /// derive under the authorized epoch of the interpreted head it just
    /// opened instead.
    fn artifact_sealing_key(
        &self,
        record: &BodyRecord,
        pending_objects: &[Vec<u8>],
    ) -> Result<AuthorizedBodyKey, Failure> {
        let keys = self.keys.as_ref().ok_or(Failure::BodyKeyUnavailable)?;
        if let Some(key) = keys.sealing_key() {
            return Ok(key);
        }
        let head = record.head()?;
        let reference = record
            .artifacts(head)
            .next()
            .copied()
            .ok_or(Failure::Integrity(Defect::MissingMaterial))?;
        let pending = pending_objects
            .iter()
            .find(|bytes| object_ref(bytes).hash == reference.hash);
        let owned;
        let envelope = match pending {
            Some(bytes) => bytes.as_slice(),
            None => {
                owned = self
                    .durable
                    .as_ref()
                    .ok_or(Failure::Poisoned)?
                    .read_object(&artifact_object(&reference))
                    .map_err(|_| Failure::Integrity(Defect::CorruptMaterial))?;
                owned.as_slice()
            }
        };
        let epoch = mechanics::authorization::body_epoch_id(envelope)
            .ok_or(Failure::Integrity(Defect::CorruptMaterial))?;
        keys.opening_key(&epoch).ok_or(Failure::BodyKeyUnavailable)
    }

    fn protected_artifact(
        &self,
        artifact: &Artifact,
        record: &BodyRecord,
        pending_objects: &[Vec<u8>],
    ) -> Result<(ArtifactRef, Vec<u8>), Failure> {
        let key = self.artifact_sealing_key(record, pending_objects)?;
        let envelope = seal_artifact(artifact, &key).map_err(|failure| match failure {
            BodyInvalid::BodyTooLarge => Failure::OpLimit,
            _ => Failure::Integrity(Defect::CorruptMaterial),
        })?;
        let reference = object_ref(&envelope);
        Ok((
            ArtifactRef {
                hash: reference.hash,
                len: reference.len,
                epoch: *key.epoch_id(),
            },
            envelope,
        ))
    }

    fn schedule_checkpoint_if_hot(&self, key: &BodyKey) {
        let Some(record) = self.bodies.get(key) else {
            return;
        };
        if record.binding.mutation_model != MUTATION_COLLABORATIVE {
            return;
        }
        let Some(base) = record.causal.as_ref() else {
            return;
        };
        // A folded concurrent head set may carry one constituent descriptor
        // while Fabric already contains the union. Such a descriptor is not a
        // coordinate for trimming a prefix of the union. Wait for the next
        // local commit to collapse the set and publish a Material whose
        // version exactly names Fabric's full state.
        if record.heads.len() != 1
            || !matches!(
                lock_fabric(&self.fabric).version(&fabric_key(key)),
                Ok(version) if version == base.version
            )
        {
            return;
        }
        let policy = CheckpointPolicy::default();
        if !policy.should_prepare(
            base.delta_tail.len(),
            usize::try_from(base.tail_bytes()).unwrap_or(usize::MAX),
        ) {
            return;
        }
        if self
            .checkpoint_jobs
            .lock()
            .is_ok_and(|jobs| jobs.contains_key(key))
        {
            return;
        }
        let executor = checkpoint_executor();
        let Some(permit) = executor.try_reserve() else {
            return;
        };
        let Ok(seed) = lock_fabric(&self.fabric).checkpoint_seed(&fabric_key(key)) else {
            return;
        };
        let Ok(sealing_key) = self.artifact_sealing_key(record, &[]) else {
            return;
        };
        let sealing_epoch = *sealing_key.epoch_id();
        let base = base.clone();
        let (sender, receiver) = mpsc::sync_channel(1);
        let work: CheckpointWork = Box::new(move || {
            let prepared = seed.export().ok().and_then(|artifact| {
                let envelope = seal_artifact(&artifact, &sealing_key).ok()?;
                let object = object_ref(&envelope);
                Some((
                    ArtifactRef {
                        hash: object.hash,
                        len: object.len,
                        epoch: sealing_epoch,
                    },
                    envelope,
                ))
            });
            let _ = sender.send(prepared);
        });
        if permit.submit(work).is_ok() {
            if let Ok(mut jobs) = self.checkpoint_jobs.lock() {
                jobs.entry(key.clone()).or_insert(PreparedCheckpoint {
                    base,
                    receiver,
                    #[cfg(test)]
                    _held_sender: None,
                });
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn stall_checkpoint_for_test(&self, key: &BodyKey) {
        let base = self
            .bodies
            .get(key)
            .and_then(|record| record.causal.clone())
            .expect("collaborative test Body has causal material");
        let (sender, receiver) = mpsc::sync_channel(1);
        self.checkpoint_jobs
            .lock()
            .expect("checkpoint jobs")
            .insert(
                key.clone(),
                PreparedCheckpoint {
                    base,
                    receiver,
                    _held_sender: Some(sender),
                },
            );
    }

    fn take_ready_checkpoint(
        &self,
        key: &BodyKey,
        prior: &CausalMaterial,
    ) -> Option<(ArtifactRef, Vec<u8>, usize)> {
        let mut jobs = self.checkpoint_jobs.lock().ok()?;
        let outcome = jobs.get(key)?.receiver.try_recv();
        let ready = match outcome {
            Ok(ready) => ready,
            Err(mpsc::TryRecvError::Empty) => return None,
            Err(mpsc::TryRecvError::Disconnected) => None,
        };
        let job = jobs.remove(key)?;
        let (reference, envelope) = ready?;
        if prior.checkpoint != job.base.checkpoint
            || !prior.delta_tail.starts_with(&job.base.delta_tail)
        {
            return None;
        }
        Some((reference, envelope, job.base.delta_tail.len()))
    }

    /// Build the next bounded causal descriptor for an interpreted Body. A hot
    /// Body appends one protected delta while a full checkpoint is prepared at
    /// the soft watermark. A ready checkpoint replaces its covered prefix.
    /// Crossing the target still appends one bounded delta. Even at the
    /// emergency protocol ceiling this path never serializes the Body: it
    /// returns fast, retryable checkpoint backpressure until the reserved
    /// worker result can be installed. No retention frontier moves and no
    /// concurrent work becomes inadmissible.
    fn next_causal_material(
        &self,
        key: &BodyKey,
        record: &BodyRecord,
        prior: Option<&CausalMaterial>,
        pending_objects: &[Vec<u8>],
    ) -> Result<(CausalMaterial, Vec<Vec<u8>>), Failure> {
        let engine_key = fabric_key(key);
        let checkpoint = |this: &Self| -> Result<(CausalMaterial, Vec<Vec<u8>>), Failure> {
            let artifact = lock_fabric(&this.fabric)
                .export_checkpoint(&engine_key, &CausalVersion::empty())
                .map_err(|_| Failure::Integrity(Defect::CorruptMaterial))?;
            let version = artifact
                .result()
                .cloned()
                .unwrap_or_else(CausalVersion::empty);
            let plaintext_size = match &artifact {
                Artifact::Replace { bytes, .. }
                | Artifact::Checkpoint { bytes, .. }
                | Artifact::Archive { bytes, .. } => u64::try_from(bytes.len()).unwrap_or(u64::MAX),
                Artifact::Delta { bytes, .. } => u64::try_from(bytes.len()).unwrap_or(u64::MAX),
            }
            .min(fabric::MAX_MATERIAL_PLAINTEXT_BYTES);
            let (checkpoint, envelope) =
                this.protected_artifact(&artifact, record, pending_objects)?;
            let material = CausalMaterial {
                format_version: CAUSAL_FORMAT_VERSION,
                checkpoint,
                delta_tail: Vec::new(),
                history_root: prior.and_then(|material| material.history_root),
                history_count: prior.map_or(0, |material| material.history_count),
                version,
                plaintext_size,
            };
            material
                .validate()
                .map_err(|_| Failure::Integrity(Defect::CorruptMaterial))?;
            Ok((material, vec![envelope]))
        };

        if record.binding.mutation_model != MUTATION_COLLABORATIVE {
            return checkpoint(self);
        }
        let Some(prior) = prior else {
            return checkpoint(self);
        };
        prior
            .validate()
            .map_err(|_| Failure::Integrity(Defect::CorruptMaterial))?;
        let artifact = lock_fabric(&self.fabric)
            .export_delta(&engine_key, &prior.version)
            .map_err(|_| Failure::Integrity(Defect::CorruptMaterial))?;
        let result = artifact
            .result()
            .cloned()
            .ok_or(Failure::Integrity(Defect::CorruptMaterial))?;
        let (reference, envelope) = self.protected_artifact(&artifact, record, pending_objects)?;
        let policy = CheckpointPolicy::default();
        if let Some((checkpoint, checkpoint_envelope, covered_tail)) =
            self.take_ready_checkpoint(key, prior)
        {
            let mut delta_tail = prior
                .delta_tail
                .get(covered_tail..)
                .ok_or(Failure::Integrity(Defect::CorruptMaterial))?
                .to_vec();
            delta_tail.push(reference);
            let plaintext_size = checkpoint
                .len
                .saturating_add(delta_tail.iter().map(|reference| reference.len).sum())
                .min(fabric::MAX_MATERIAL_PLAINTEXT_BYTES);
            let material = CausalMaterial {
                format_version: CAUSAL_FORMAT_VERSION,
                checkpoint,
                delta_tail,
                history_root: prior.history_root,
                history_count: prior.history_count,
                version: result,
                plaintext_size,
            };
            material
                .validate()
                .map_err(|_| Failure::Integrity(Defect::CorruptMaterial))?;
            return Ok((material, vec![checkpoint_envelope, envelope]));
        }
        // The target is maintenance policy, not permission for a latency
        // cliff. A stalled worker gets sixteen target intervals of explicit
        // protocol headroom. At the emergency envelope, refuse cheaply and
        // leave the already-published Material (which itself is the durable
        // pending-checkpoint intent) untouched.
        let next_tail_deltas = prior.delta_tail.len().saturating_add(1);
        let next_tail_bytes = usize::try_from(prior.tail_bytes())
            .unwrap_or(usize::MAX)
            .saturating_add(usize::try_from(reference.len).unwrap_or(usize::MAX));
        if !policy.admits(next_tail_deltas, next_tail_bytes) {
            return Err(Failure::CheckpointBackpressure);
        }
        let mut material = prior.clone();
        material.delta_tail.push(reference);
        material.version = result;
        material.plaintext_size = material
            .checkpoint
            .len
            .saturating_add(
                material
                    .delta_tail
                    .iter()
                    .map(|reference| reference.len)
                    .sum(),
            )
            .min(fabric::MAX_MATERIAL_PLAINTEXT_BYTES);
        material
            .validate()
            .map_err(|_| Failure::Integrity(Defect::CorruptMaterial))?;
        Ok((material, vec![envelope]))
    }

    /// The single durable write.
    ///
    /// Everything proportional to the store size is gone from here: the
    /// catalogs update by delta, the journal is told what changed rather than
    /// what to keep, and the signed manifest commits to index roots. What this
    /// writes is the changed Bodies, their objects, the nodes on the paths they
    /// touch, and one root.
    #[allow(clippy::too_many_arguments)]
    fn persist(
        &mut self,
        ctx: Option<&CommitContext<'_>>,
        changed: &mut BTreeMap<BodyKey, Option<BodyRecord>>,
        declared: &BTreeMap<BodyKey, Vec<[u8; 32]>>,
        descriptors: &[crate::content::ContentDescriptor],
        new_receipt: Option<&RequestReceipt>,
        mut new_objects: Vec<Vec<u8>>,
        next_frontier: ReplicaFrontier,
    ) -> Result<(), Failure> {
        use crate::index::{self, IndexChange, NodeSink};

        let mut sink = NodeSink::default();
        let mut removed: Vec<[u8; 32]> = Vec::new();
        let mut manifest_changes: Vec<IndexChange> = Vec::with_capacity(changed.len());
        let mut ownership_changes = BTreeMap::<[u8; 32], (Object, OwnedObjectClass, i64)>::new();

        // Materialize only genuine Body-state changes. Declaration-only
        // publications carry a Body through this map as an indexing change,
        // but must not manufacture an empty causal delta. The derived artifact
        // is protected and content-addressed; the Body record and generation
        // record carry only Fabric's bounded Material descriptor.
        if ctx.is_some() {
            let changed_keys: Vec<BodyKey> = changed.keys().cloned().collect();
            for key in changed_keys {
                let prior = self.bodies.get(&key);
                let Some(Some(record)) = changed.get(&key) else {
                    continue;
                };
                let state_changed = prior.map(record_stamp) != Some(record_stamp(record))
                    || prior.is_some_and(|old| old.interpreted != record.interpreted);
                if !record.interpreted || !state_changed {
                    continue;
                }
                let prepared = record.causal.as_ref().is_some_and(|material| {
                    material.validate().is_ok()
                        && lock_fabric(&self.fabric)
                            .version(&fabric_key(&key))
                            .is_ok_and(|version| version == material.version)
                        && (record.binding.mutation_model == MUTATION_COLLABORATIVE
                            || prior.and_then(|old| old.causal.as_ref()) != Some(material))
                });
                if prepared {
                    continue;
                }
                let (material, artifacts) = self.next_causal_material(
                    &key,
                    record,
                    prior.and_then(|old| old.causal.as_deref()),
                    &new_objects,
                )?;
                if let Some(Some(record)) = changed.get_mut(&key) {
                    record.replace_causal(Some(Arc::new(material)))?;
                }
                new_objects.extend(artifacts);
            }
        }
        for record in changed.values_mut().flatten() {
            record.compact_singleton_closure();
        }
        let mut next_generation_footprint = ctx
            .is_some()
            .then(|| {
                self.generation_footprint
                    .after_changes(&self.bodies, changed)
            })
            .transpose()?;

        // 1. Body catalog: one index change per touched Body, plus a refcount
        //    pass that decides which objects genuinely stopped being needed.
        //    One signed transaction record covers every Body in its batch, so
        //    "the objects this Body used to name" is the wrong removal set.
        let mut body_changes: Vec<IndexChange> = Vec::with_capacity(changed.len());
        for (key, record) in changed.iter() {
            let prior = self
                .bodies
                .get(key)
                .map(Self::record_owned_objects)
                .transpose()?
                .unwrap_or_default();
            let now = record
                .as_ref()
                .map(Self::record_owned_objects)
                .transpose()?
                .unwrap_or_default();
            Self::adjust_ownership(&mut ownership_changes, prior, -1)?;
            Self::adjust_ownership(&mut ownership_changes, now, 1)?;
            let value = match record {
                None => None,
                Some(record) => {
                    let entry = IndexedBody {
                        key: key.clone(),
                        record: record.clone(),
                    };
                    Some(
                        postcard::to_stdvec(&entry)
                            .map_err(|_| Failure::Integrity(Defect::Encoding))?,
                    )
                }
            };
            body_changes.push(IndexChange {
                key: body_index_key(key),
                value,
            });
            let refs = declared
                .get(key)
                .cloned()
                .unwrap_or_else(|| self.declared_content.get(key).cloned().unwrap_or_default());
            let advertised = match record {
                None => None,
                Some(r) => Some(Self::manifest_entry(key, r, refs.clone())?.encode()),
            };
            manifest_changes.push(IndexChange {
                key: body_index_key(key),
                value: advertised,
            });
            self.replace_declared_content(
                key,
                record
                    .as_ref()
                    .is_some()
                    .then_some(refs)
                    .unwrap_or_default(),
            );
        }

        // Content descriptors committed by this transaction. A descriptor is
        // required material on every full Replica; its chunks are not.
        let mut content_changes: Vec<IndexChange> = Vec::with_capacity(descriptors.len());
        for descriptor in descriptors {
            descriptor
                .validate()
                .map_err(|_| Failure::Illegitimate(Invalid::Encoding))?;
            content_changes.push(IndexChange {
                key: crate::manifest::content_index_key(descriptor.content_ref().as_bytes()),
                value: Some(descriptor.encode()),
            });
        }

        let (body_index_root, manifest_body_root, content_index_root) = {
            let store = self.durable.as_ref().ok_or(Failure::Poisoned)?;
            let nodes = StoreNodes(store);
            let body_index_root =
                index::apply(&nodes, self.body_index_root, body_changes, &mut sink)
                    .map_err(|_| Failure::Integrity(Defect::Index))?;
            let manifest_body_root =
                index::apply(&nodes, self.manifest_body_root, manifest_changes, &mut sink)
                    .map_err(|_| Failure::Integrity(Defect::Index))?;
            let content_index_root =
                index::apply(&nodes, self.content_index_root, content_changes, &mut sink)
                    .map_err(|_| Failure::Integrity(Defect::Index))?;
            (body_index_root, manifest_body_root, content_index_root)
        };

        // 2. Receipt catalog.
        let mut receipt_index_root = self.receipt_index_root;
        let mut next_receipt_count = self.receipt_count;
        let mut next_receipt_material_bytes = self.receipt_material_bytes;
        let mut new_receipt_object = None;
        if let Some(receipt) = new_receipt {
            let bytes = validate_receipt_for_storage(receipt)?;
            next_receipt_count = next_receipt_count
                .checked_add(1)
                .ok_or(Failure::QuotaExceeded)?;
            next_receipt_material_bytes = next_receipt_material_bytes
                .checked_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX))
                .ok_or(Failure::QuotaExceeded)?;
            let reference = object_ref(&bytes);
            new_receipt_object = Some(reference);
            new_objects.push(bytes);
            let mut owned = BTreeMap::new();
            Self::insert_owned(&mut owned, reference, OwnedObjectClass::DeferredReceipt)?;
            Self::adjust_ownership(&mut ownership_changes, owned, 1)?;
            let entry = IndexedReceipt {
                scope: receipt.scope_key(),
                object: reference,
            };
            let value =
                postcard::to_stdvec(&entry).map_err(|_| Failure::Integrity(Defect::Encoding))?;
            let store = self.durable.as_ref().ok_or(Failure::Poisoned)?;
            let nodes = StoreNodes(store);
            receipt_index_root = index::apply(
                &nodes,
                receipt_index_root,
                vec![IndexChange {
                    key: receipt_index_key(&receipt.scope_key()),
                    value: Some(value),
                }],
                &mut sink,
            )
            .map_err(|_| Failure::Integrity(Defect::Index))?;
        }

        // 3. The signed root over the catalogs. A commit with no attribution
        //    context is a receipt-only replay and republishes nothing.
        let parent_generation = self
            .generation_index_root
            .and(self.manifest_root_object.map(|object| object.hash));
        let manifest_root_object = match ctx {
            None => self.manifest_root_object,
            Some(ctx) => {
                let root = ManifestRoot::sign_with(
                    ctx.space,
                    next_frontier,
                    manifest_body_root,
                    content_index_root,
                    ctx.authority_frontier.clone(),
                    ctx.signer,
                )
                .ok_or_else(|| Failure::Illegitimate("sign manifest root".into()))?;
                let bytes = root.encode();
                let reference = object_ref(&bytes);
                new_objects.push(bytes);
                let mut next_root = BTreeMap::new();
                Self::insert_owned(&mut next_root, reference, OwnedObjectClass::Eager)?;
                Self::adjust_ownership(&mut ownership_changes, next_root, 1)?;
                if let Some(prior) = self.manifest_root_object {
                    if prior.hash != reference.hash {
                        let mut prior_root = BTreeMap::new();
                        Self::insert_owned(&mut prior_root, prior, OwnedObjectClass::Eager)?;
                        Self::adjust_ownership(&mut ownership_changes, prior_root, -1)?;
                    }
                }
                Some(reference)
            }
        };

        // 4. Durable read generation: one immutable delta object plus one
        // index path. Delta size is O(changed Bodies), not O(World), and old
        // delta objects remain required so an exact historical read survives
        // ordinary journal sweeping and process restart.
        let mut generation_index_root = self.generation_index_root;
        if ctx.is_some() {
            if let Some(root_object) = manifest_root_object {
                let archive_keys: BTreeSet<BodyKey> = if self.generation_index_root.is_none() {
                    self.bodies.keys().chain(changed.keys()).cloned().collect()
                } else {
                    changed.keys().cloned().collect()
                };
                let mut archived = Vec::with_capacity(archive_keys.len());
                for key in archive_keys {
                    let next = match changed.get(&key) {
                        Some(Some(record)) => Some(record),
                        Some(None) => None,
                        None => self.bodies.get(&key),
                    };
                    let binding = next
                        .map(|record| record.binding.clone())
                        .or_else(|| self.bodies.get(&key).map(|record| record.binding.clone()));
                    let stamp = next.map(record_stamp).unwrap_or_default();
                    let material = next.and_then(|record| record.causal.clone());
                    archived.push(ArchivedBody {
                        key,
                        present: next.is_some(),
                        interpreted: next.is_some_and(|record| record.interpreted),
                        binding,
                        stamp,
                        material,
                    });
                }
                let delta = GenerationDelta {
                    format_version: GENERATION_DELTA_FORMAT_VERSION,
                    root: root_object.hash,
                    parent: parent_generation,
                    frontier: next_frontier,
                    changed: archived,
                    descriptors: descriptors.to_vec(),
                    removed_descriptors: Vec::new(),
                };
                let bytes = postcard::to_stdvec(&delta).map_err(|error| {
                    integrity_cause(Defect::Encoding, "encode committed generation delta", error)
                })?;
                let delta_len = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
                let reference = object_ref(&bytes);
                new_objects.push(bytes);
                let mut generation_objects = BTreeMap::new();
                Self::insert_owned(&mut generation_objects, reference, OwnedObjectClass::Eager)?;
                for archived in &delta.changed {
                    for (hash, owned) in Self::material_owned_objects(archived.material.as_deref())?
                    {
                        match generation_objects.get(&hash) {
                            Some(held) if held == &owned => {}
                            Some(_) => {
                                return Err(Failure::Integrity(Defect::CorruptMaterial));
                            }
                            None => {
                                generation_objects.insert(hash, owned);
                            }
                        }
                    }
                }
                Self::adjust_ownership(&mut ownership_changes, generation_objects, 1)?;
                let mut footprint = next_generation_footprint
                    .clone()
                    .ok_or(Failure::Integrity(Defect::Index))?;
                footprint.record_generation_delta(
                    parent_generation.map(|_| &self.generation_footprint),
                    &delta,
                    delta_len,
                )?;
                next_generation_footprint = Some(footprint.clone());
                let indexed = IndexedGeneration {
                    root: root_object.hash,
                    object: reference,
                    footprint,
                };
                let value = postcard::to_stdvec(&indexed).map_err(|error| {
                    integrity_cause(Defect::Encoding, "encode committed generation index", error)
                })?;
                let store = self.durable.as_ref().ok_or(Failure::Poisoned)?;
                generation_index_root = index::apply(
                    &StoreNodes(store),
                    generation_index_root,
                    vec![IndexChange {
                        key: generation_index_key(&root_object.hash),
                        value: Some(value),
                    }],
                    &mut sink,
                )
                .map_err(|error| {
                    integrity_cause(Defect::Index, "apply committed generation index", error)
                })?;
            }
        }

        let (ownership_index_root, ownership_eager_removed, deferred_removed) = {
            let store = self.durable.as_ref().ok_or(Failure::Poisoned)?;
            Self::apply_ownership_changes(
                store,
                self.ownership_index_root,
                ownership_changes,
                &mut sink,
            )?
        };
        removed.extend(ownership_eager_removed);

        let meta = StoreMeta {
            format_version: STORE_META_FORMAT_VERSION,
            space: self.space.clone(),
            frontier: next_frontier,
            quota: self.quota,
            body_index_root,
            manifest_body_root,
            content_index_root,
            receipt_index_root,
            receipt_count: next_receipt_count,
            receipt_material_bytes: next_receipt_material_bytes,
            generation_index_root,
            ownership_index_root,
            manifest_root: manifest_root_object,
        };
        let meta_bytes =
            postcard::to_stdvec(&meta).map_err(|_| Failure::Integrity(Defect::Encoding))?;

        // Index nodes are handed over separately, not folded into `added`. An
        // entry in the required set is a promise that never expires, so a node
        // admitted there would outlive every rewrite that superseded it and the
        // store would grow with the number of commits rather than with what it
        // holds. As caller-index nodes they live by reachability instead.
        let mut added = new_objects;
        let mut seen = std::collections::BTreeSet::new();
        added.retain(|b| seen.insert(object_ref(b).hash));
        let mut deferred_hashes: BTreeSet<[u8; 32]> = changed
            .values()
            .flatten()
            .flat_map(|record| {
                record
                    .heads
                    .iter()
                    .flat_map(|head| record.artifacts(head).map(|artifact| artifact.hash))
                    .chain(record.causal.iter().flat_map(|material| {
                        std::iter::once(material.checkpoint.hash)
                            .chain(material.delta_tail.iter().map(|artifact| artifact.hash))
                    }))
            })
            .collect();
        if let Some(reference) = new_receipt_object {
            deferred_hashes.insert(reference.hash);
        }
        let (deferred_added, added): (Vec<Vec<u8>>, Vec<Vec<u8>>) = added
            .into_iter()
            .partition(|bytes| deferred_hashes.contains(&object_ref(bytes).hash));
        let mut index_nodes = sink.written;
        index_nodes.retain(|b| seen.insert(object_ref(b).hash));
        // An object written by this commit is not also collectable by it.
        removed.retain(|h| !seen.contains(h));
        removed.sort();
        removed.dedup();

        let roots: Vec<([u8; 32], u64)> = body_index_root
            .into_iter()
            .chain(manifest_body_root)
            .chain(content_index_root)
            .chain(receipt_index_root)
            .chain(generation_index_root)
            .map(|root| (root.hash, root.count))
            .collect();
        let lazy_roots: Vec<([u8; 32], u64)> = ownership_index_root
            .into_iter()
            .map(|root| (root.hash, root.count))
            .collect();

        let store = self.durable.as_mut().ok_or(Failure::Poisoned)?;
        let caller_index = journal::Index {
            roots: &roots,
            lazy_roots: &lazy_roots,
            nodes: &index_nodes,
        };
        match store.commit_classified(
            &added,
            &removed,
            journal::Deferred {
                added: &deferred_added,
                removed: &deferred_removed,
            },
            caller_index,
            meta_bytes,
        ) {
            Ok(_) => {}
            Err(journal::Failure::OutcomeUnknown) => {
                self.poisoned = true;
                return Err(Failure::OutcomeUnknown);
            }
            Err(e) => {
                self.poisoned = true;
                return Err(Failure::Durability(e));
            }
        }
        self.body_index_root = body_index_root;
        self.manifest_body_root = manifest_body_root;
        self.content_index_root = content_index_root;
        self.receipt_index_root = receipt_index_root;
        self.receipt_count = next_receipt_count;
        self.receipt_material_bytes = next_receipt_material_bytes;
        self.generation_index_root = generation_index_root;
        if let Some(footprint) = next_generation_footprint {
            self.generation_footprint = footprint;
        }
        self.ownership_index_root = ownership_index_root;
        self.manifest_root_object = manifest_root_object;
        Ok(())
    }

    /// Commit content descriptors, making the content nameable by a Body.
    ///
    /// The descriptor becomes required material on every full Replica; the
    /// chunks stay residency. That asymmetry is the content plane: a World can
    /// name a gigabyte without every peer downloading a gigabyte.
    pub fn commit_content(
        &mut self,
        ctx: &CommitContext<'_>,
        descriptors: &[crate::content::ContentDescriptor],
    ) -> Result<Vec<crate::content::ContentRef>, Failure> {
        self.mutation_available()?;
        let mut causal = Vec::with_capacity(descriptors.len().saturating_mul(32));
        for descriptor in descriptors {
            causal.extend_from_slice(descriptor.content_ref().as_bytes());
        }
        let frontier = advance_published(self.frontier, &causal);
        let mut changed = BTreeMap::new();
        self.persist(
            Some(ctx),
            &mut changed,
            &BTreeMap::new(),
            descriptors,
            None,
            Vec::new(),
            frontier,
        )?;
        self.frontier = frontier;
        Ok(descriptors.iter().map(|d| d.content_ref()).collect())
    }

    /// Hold committed content against the sweep until `until`, because a Body
    /// that will declare it is still being assembled.
    ///
    /// The caller supplies the deadline rather than the Replica reading a
    /// clock, for the same reason it supplies a [`CommitContext`]: what time it
    /// is is the caller's business, and a store that reads clocks is a store
    /// whose behaviour cannot be stated in a test.
    ///
    /// A hold is not a declaration. It stops the content being collected; it
    /// does not put it in an advertisement, because a peer receiving a
    /// descriptor no Body names would adopt catalog it has no reason to keep
    /// and its own sweep would have to undo.
    pub fn hold_content(
        &mut self,
        content: &crate::content::ContentRef,
        until: std::time::Instant,
    ) {
        self.pending_content.insert(*content.as_bytes(), until);
    }

    /// Drop a hold before its deadline — an upload the caller abandoned, or one
    /// whose Body arrived by another route.
    pub fn release_content_hold(&mut self, content: &crate::content::ContentRef) {
        self.pending_content.remove(content.as_bytes());
    }

    /// Record which content each named Body references.
    ///
    /// The World supplies this when it stages a transaction; Replica validates
    /// it and signs it into the root. Validation is deliberately shallow — it
    /// checks bounds, order, uniqueness, and that every named descriptor is
    /// actually committed — because anything deeper would mean decoding the
    /// product bytes the declaration describes, which is the boundary this
    /// exists to respect.
    ///
    /// F5 folds this into the staging call so a Body and its declaration commit
    /// together. It is separate here so F2 can prove the reachability rule
    /// without waiting on the World surface.
    pub fn declare_content(
        &mut self,
        ctx: &CommitContext<'_>,
        declarations: BTreeMap<BodyKey, Vec<crate::content::ContentRef>>,
    ) -> Result<(), Failure> {
        self.mutation_available()?;
        let mut declared: BTreeMap<BodyKey, Vec<[u8; 32]>> = BTreeMap::new();
        let mut changed: BTreeMap<BodyKey, Option<BodyRecord>> = BTreeMap::new();
        for (key, refs) in declarations {
            if refs.len() > crate::manifest::MAX_CONTENT_REFS_PER_BODY {
                return Err(Failure::Illegitimate(
                    "declared content references exceed the per-Body bound".into(),
                ));
            }
            let Some(record) = self.bodies.get(&key) else {
                return Err(Failure::Illegitimate(
                    "a declaration names a Body this Replica does not hold".into(),
                ));
            };
            for reference in &refs {
                if self.content_descriptor(reference).is_none() {
                    return Err(Failure::Illegitimate(
                        "a declaration names content with no committed descriptor".into(),
                    ));
                }
            }
            let mut ids: Vec<[u8; 32]> = refs.iter().map(|r| *r.as_bytes()).collect();
            ids.sort();
            ids.dedup();
            declared.insert(key.clone(), ids);
            changed.insert(key, Some(record.clone()));
        }
        let mut causal = Vec::new();
        for (key, ids) in &declared {
            causal.extend_from_slice(&body_index_key(key));
            for id in ids {
                causal.extend_from_slice(id);
            }
        }
        let frontier = advance_published(self.frontier, &causal);
        self.persist(
            Some(ctx),
            &mut changed,
            &declared,
            &[],
            None,
            Vec::new(),
            frontier,
        )?;
        self.frontier = frontier;
        // The Body that the hold was waiting for has arrived, so the hold has
        // done its job. Released here rather than by the caller, because a
        // caller that had to remember would eventually not.
        for ids in declared.values() {
            for id in ids {
                self.pending_content.remove(id);
            }
        }
        Ok(())
    }

    /// A Body's causal position, in lait's own head-set terms.
    pub fn body_version(&self, key: &BodyKey) -> Option<fabric::Version> {
        lock_fabric(&self.fabric)
            .version(&fabric_key(key))
            .ok()
            .or_else(|| {
                self.bodies
                    .get(key)
                    .filter(|record| record.interpreted)
                    .and_then(|record| record.causal.as_ref())
                    .map(|material| material.version.clone())
            })
    }

    /// Take an anchor at a position inside a collaborative value.
    pub fn anchor(&self, key: &BodyKey, path: &str, position: u64) -> Option<fabric::Anchor> {
        lock_fabric(&self.fabric)
            .anchor(&fabric_key(key), path, position)
            .ok()
    }

    /// Resolve an anchor. Total, and never mutates the Body.
    pub fn resolve_anchor(
        &self,
        key: &BodyKey,
        anchor: &fabric::Anchor,
    ) -> fabric::AnchorResolution {
        lock_fabric(&self.fabric).resolve(&fabric_key(key), anchor)
    }

    /// Drop a Body's declaration without republishing — what a tombstone does
    /// to the content it used to hold. Reachability is over *live* Bodies, so
    /// forgetting the declaration is what makes the content collectable.
    pub fn forget_declaration(&mut self, key: &BodyKey) {
        self.replace_declared_content(key, Vec::new());
    }

    /// The content one Body declares.
    pub fn declared_content(&self, key: &BodyKey) -> Vec<crate::content::ContentRef> {
        self.declared_content
            .get(key)
            .map(|refs| {
                refs.iter()
                    .map(|id| crate::content::ContentRef { content_id: *id })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// A committed content descriptor, if this Replica holds one.
    pub fn content_descriptor(
        &self,
        content: &crate::content::ContentRef,
    ) -> Option<crate::content::ContentDescriptor> {
        let store = self.durable.as_ref()?;
        let value = crate::index::lookup(
            &StoreNodes(store),
            self.content_index_root,
            &crate::manifest::content_index_key(content.as_bytes()),
        )
        .ok()??;
        crate::content::ContentDescriptor::decode_canonical(&value).ok()
    }

    /// Every content id some live Body declares. The reachability rule, stated
    /// as the pure function it is.
    ///
    /// This is the *advertisement* question — what a peer may be told about —
    /// and it deliberately excludes held content. See [`Self::retained_content`]
    /// for the deletion question, which is not the same one.
    fn reachable_content(&self) -> std::collections::BTreeSet<[u8; 32]> {
        self.declared_content
            .iter()
            .filter(|(key, _)| self.bodies.contains_key(key))
            .flat_map(|(_, refs)| refs.iter().copied())
            .collect()
    }

    /// Every content id this Replica must not collect: what live Bodies declare,
    /// plus what an unexpired hold is still waiting to declare.
    ///
    /// Wider than [`Self::reachable_content`] on purpose. "May I show this to a
    /// peer" and "may I delete this" are different questions, and answering
    /// them with one set forces a choice between deleting an upload mid-attach
    /// and pushing an undeclared descriptor at every peer.
    fn retained_content(&self, now: std::time::Instant) -> std::collections::BTreeSet<[u8; 32]> {
        let mut retained = self.reachable_content();
        retained.extend(
            self.pending_content
                .iter()
                .filter(|(_, until)| **until > now)
                .map(|(id, _)| *id),
        );
        retained
    }

    /// Drop every content descriptor nothing keeps, and release the residency
    /// behind it.
    ///
    /// "Nothing keeps" is wider than "no live Body declares": content under an
    /// unexpired hold is retained too, because an upload waiting for the Body
    /// that will name it is not garbage yet. See [`Self::retained_content`].
    ///
    /// Reachability is **derived, never maintained**: no reference count is
    /// stored, because counts do not converge across independently committing
    /// replicas while a pure function of the converged Body set does. The sweep
    /// streams the catalog exactly as the journal's own sweep does, so it is
    /// periodic and quota-driven, never a per-commit cost.
    ///
    /// What this is not: erasure. A peer that has not yet converged on the
    /// tombstones keeps the descriptor until it does, and a peer that copied
    /// the bytes out was never bound by this at all. That is the same promise
    /// Body tombstones already make, and content does not get a stronger one.
    pub fn sweep_unreferenced_content(
        &mut self,
        ctx: &CommitContext<'_>,
        cache: Option<&crate::cache::Residency>,
    ) -> Result<Vec<crate::content::ContentRef>, Failure> {
        self.sweep_unreferenced_content_at(ctx, cache, std::time::Instant::now())
    }

    /// [`Self::sweep_unreferenced_content`], at a caller-supplied instant.
    ///
    /// A hold expires, and what happens when it does is a real question — the
    /// hold exists to buy an upload a window before its Body arrives, so the
    /// interesting case is the sweep that runs one moment after the window
    /// closes. Reaching that case by waiting is not practical, and `Replica`
    /// deliberately has no tokio dependency, so the clock arrives the way it
    /// already does everywhere else in this crate: as a parameter.
    /// `retained_content` has always taken one; this is the caller that used to
    /// mint it privately.
    pub fn sweep_unreferenced_content_at(
        &mut self,
        ctx: &CommitContext<'_>,
        cache: Option<&crate::cache::Residency>,
        now: std::time::Instant,
    ) -> Result<Vec<crate::content::ContentRef>, Failure> {
        use crate::index::{self, IndexChange, NodeSink};

        self.pending_content.retain(|_, until| *until > now);
        let reachable = self.retained_content(now);
        let mut unreferenced: Vec<crate::content::ContentDescriptor> = Vec::new();
        {
            let store = self.durable.as_ref().ok_or(Failure::Poisoned)?;
            let mut failure: Option<String> = None;
            index::stream(&StoreNodes(store), self.content_index_root, &mut |entry| {
                if failure.is_some() {
                    return;
                }
                match crate::content::ContentDescriptor::decode_canonical(&entry.value) {
                    Ok(descriptor) => {
                        if !reachable.contains(descriptor.content_ref().as_bytes()) {
                            unreferenced.push(descriptor);
                        }
                    }
                    Err(e) => failure = Some(format!("content entry: {e}")),
                }
            })
            .map_err(|_| Failure::Integrity(Defect::Index))?;
            if let Some(reason) = failure {
                tracing::warn!(%reason, "content index entry is not canonical");
                return Err(Failure::Integrity(Defect::Encoding));
            }
        }
        if unreferenced.is_empty() {
            return Ok(Vec::new());
        }

        let mut sink = NodeSink::default();
        let changes: Vec<IndexChange> = unreferenced
            .iter()
            .map(|d| IndexChange {
                key: crate::manifest::content_index_key(d.content_ref().as_bytes()),
                value: None,
            })
            .collect();
        let content_index_root = {
            let store = self.durable.as_ref().ok_or(Failure::Poisoned)?;
            index::apply(
                &StoreNodes(store),
                self.content_index_root,
                changes,
                &mut sink,
            )
            .map_err(|_| Failure::Integrity(Defect::Index))?
        };

        // Republish under the shrunken catalog before touching residency, so a
        // crash between the two leaves bytes without a descriptor — reclaimable
        // garbage — rather than a descriptor whose bytes are gone.
        let mut causal = Vec::with_capacity(unreferenced.len().saturating_mul(32));
        for descriptor in &unreferenced {
            causal.extend_from_slice(descriptor.content_ref().as_bytes());
        }
        let frontier = advance_published(self.frontier, &causal);
        let root = ManifestRoot::sign_with(
            ctx.space,
            frontier,
            self.manifest_body_root,
            content_index_root,
            ctx.authority_frontier.clone(),
            ctx.signer,
        )
        .ok_or_else(|| Failure::Illegitimate("sign manifest root".into()))?;
        let root_bytes = root.encode();
        let root_ref = object_ref(&root_bytes);
        let parent_generation = self
            .generation_index_root
            .and(self.manifest_root_object.map(|object| object.hash));
        let mut ownership_changes = BTreeMap::<[u8; 32], (Object, OwnedObjectClass, i64)>::new();
        let mut root_owned = BTreeMap::new();
        Self::insert_owned(&mut root_owned, root_ref, OwnedObjectClass::Eager)?;
        Self::adjust_ownership(&mut ownership_changes, root_owned, 1)?;
        if let Some(prior) = self.manifest_root_object {
            if prior.hash != root_ref.hash {
                let mut prior_owned = BTreeMap::new();
                Self::insert_owned(&mut prior_owned, prior, OwnedObjectClass::Eager)?;
                Self::adjust_ownership(&mut ownership_changes, prior_owned, -1)?;
            }
        }

        let archived = if self.generation_index_root.is_none() {
            self.bodies
                .iter()
                .map(|(key, record)| ArchivedBody {
                    key: key.clone(),
                    present: true,
                    interpreted: record.interpreted,
                    binding: Some(record.binding.clone()),
                    stamp: record_stamp(record),
                    material: record.causal.clone(),
                })
                .collect()
        } else {
            Vec::new()
        };
        let delta = GenerationDelta {
            format_version: GENERATION_DELTA_FORMAT_VERSION,
            root: root_ref.hash,
            parent: parent_generation,
            frontier,
            changed: archived,
            descriptors: Vec::new(),
            removed_descriptors: unreferenced
                .iter()
                .map(|descriptor| descriptor.content_ref().content_id)
                .collect(),
        };
        let delta_bytes = postcard::to_stdvec(&delta).map_err(|error| {
            integrity_cause(
                Defect::Encoding,
                "encode content-sweep generation delta",
                error,
            )
        })?;
        let delta_ref = object_ref(&delta_bytes);
        let mut generation_owned = BTreeMap::new();
        Self::insert_owned(&mut generation_owned, delta_ref, OwnedObjectClass::Eager)?;
        for archived in &delta.changed {
            for (hash, owned) in Self::material_owned_objects(archived.material.as_deref())? {
                match generation_owned.get(&hash) {
                    Some(held) if held == &owned => {}
                    Some(_) => return Err(Failure::Integrity(Defect::CorruptMaterial)),
                    None => {
                        generation_owned.insert(hash, owned);
                    }
                }
            }
        }
        Self::adjust_ownership(&mut ownership_changes, generation_owned, 1)?;
        let mut generation_footprint = self.generation_footprint.clone();
        generation_footprint.record_generation_delta(
            parent_generation.map(|_| &self.generation_footprint),
            &delta,
            u64::try_from(delta_bytes.len()).unwrap_or(u64::MAX),
        )?;
        let indexed = IndexedGeneration {
            root: root_ref.hash,
            object: delta_ref,
            footprint: generation_footprint.clone(),
        };
        let indexed_bytes = postcard::to_stdvec(&indexed).map_err(|error| {
            integrity_cause(
                Defect::Encoding,
                "encode content-sweep generation index",
                error,
            )
        })?;
        let generation_index_root = {
            let store = self.durable.as_ref().ok_or(Failure::Poisoned)?;
            index::apply(
                &StoreNodes(store),
                self.generation_index_root,
                vec![IndexChange {
                    key: generation_index_key(&root_ref.hash),
                    value: Some(indexed_bytes),
                }],
                &mut sink,
            )
            .map_err(|error| {
                integrity_cause(Defect::Index, "apply content-sweep generation index", error)
            })?
        };

        let (ownership_index_root, removed, deferred_removed) = {
            let store = self.durable.as_ref().ok_or(Failure::Poisoned)?;
            Self::apply_ownership_changes(
                store,
                self.ownership_index_root,
                ownership_changes,
                &mut sink,
            )?
        };

        let meta = StoreMeta {
            format_version: STORE_META_FORMAT_VERSION,
            space: self.space.clone(),
            frontier,
            quota: self.quota,
            body_index_root: self.body_index_root,
            manifest_body_root: self.manifest_body_root,
            content_index_root,
            receipt_index_root: self.receipt_index_root,
            receipt_count: self.receipt_count,
            receipt_material_bytes: self.receipt_material_bytes,
            generation_index_root,
            ownership_index_root,
            manifest_root: Some(root_ref),
        };
        let meta_bytes =
            postcard::to_stdvec(&meta).map_err(|_| Failure::Integrity(Defect::Encoding))?;
        let added = vec![root_bytes, delta_bytes];
        let index_nodes = sink.written;
        let roots: Vec<([u8; 32], u64)> = self
            .body_index_root
            .into_iter()
            .chain(self.manifest_body_root)
            .chain(content_index_root)
            .chain(self.receipt_index_root)
            .chain(generation_index_root)
            .map(|root| (root.hash, root.count))
            .collect();
        let lazy_roots: Vec<([u8; 32], u64)> = ownership_index_root
            .into_iter()
            .map(|root| (root.hash, root.count))
            .collect();

        let store = self.durable.as_mut().ok_or(Failure::Poisoned)?;
        let caller_index = journal::Index {
            roots: &roots,
            lazy_roots: &lazy_roots,
            nodes: &index_nodes,
        };
        match store.commit_classified(
            &added,
            &removed,
            journal::Deferred {
                added: &[],
                removed: &deferred_removed,
            },
            caller_index,
            meta_bytes,
        ) {
            Ok(_) => {}
            Err(journal::Failure::OutcomeUnknown) => {
                self.poisoned = true;
                return Err(Failure::OutcomeUnknown);
            }
            Err(e) => {
                self.poisoned = true;
                return Err(Failure::Durability(e));
            }
        }
        self.content_index_root = content_index_root;
        self.generation_index_root = generation_index_root;
        self.ownership_index_root = ownership_index_root;
        self.manifest_root_object = Some(root_ref);
        self.frontier = frontier;
        self.generation_footprint = generation_footprint;

        // Residency last, and releasing is all this does — the cache's own
        // sweep decides when the bytes actually go, under its own quota. A
        // crash between the two leaves bytes nothing points at, which is
        // reclaimable garbage; the other order would leave a descriptor whose
        // bytes are already gone.
        if let Some(cache) = cache {
            for descriptor in &unreferenced {
                let _ = cache.release_content(&descriptor.content_nonce);
            }
        }
        Ok(unreferenced.iter().map(|d| d.content_ref()).collect())
    }

    /// Test seam: attach a fault injector to the underlying journaled store
    /// (see [`journal::FAULT_POINTS`]). No effect without a durable
    /// store.
    #[cfg(any(test, feature = "fault-injection"))]
    pub fn with_store_fault_injector(mut self, injector: Box<dyn Fn(&str) -> bool + Send>) -> Self {
        if let Some(store) = self.durable.take() {
            self.durable = Some(store.with_fault_injector(injector));
        }
        self
    }

    /// The current semantic frontier.
    pub fn frontier(&self) -> ReplicaFrontier {
        self.frontier
    }

    /// The current committed Manifest root's content address, or
    /// [`crate::transaction::NO_PARENT_ROOT`] before any durable commit — the
    /// parent a local submit authors against.
    pub fn manifest_root(&self) -> [u8; 32] {
        self.manifest_root_object
            .map(|r| r.hash)
            .unwrap_or(crate::transaction::NO_PARENT_ROOT)
    }

    /// A Body's immutable schema binding, if the Body exists.
    pub fn binding(&self, key: &BodyKey) -> Option<&BodyBinding> {
        self.bodies.get(key).map(|r| &r.binding)
    }

    /// Whether a Body is retained opaquely (present but uninterpretable —
    /// unknown World/schema or missing key material).
    pub fn is_opaque(&self, key: &BodyKey) -> bool {
        self.bodies.get(key).is_some_and(|r| !r.interpreted)
    }

    /// A Body's version stamp: its chain frontier plus every head's
    /// transaction commitment. Equal stamps guarantee byte-equivalent Bodies
    /// (a chain never repeats across distinct states, and the head set pins
    /// the constituent material exactly).
    pub fn body_stamp(&self, key: &BodyKey) -> Option<Vec<u8>> {
        let record = self.bodies.get(key)?;
        Some(record_stamp(record))
    }

    /// Every Body currently present (interpreted or opaque).
    pub fn body_keys(&self) -> Vec<BodyKey> {
        self.bodies.keys().cloned().collect()
    }

    /// How many Bodies this Replica holds, interpreted and opaque alike.
    ///
    /// The count on its own, because the two existing ways to get it both make
    /// a caller pay for something else: [`Self::body_keys`] clones every key to
    /// return a vector nobody reads, and [`Self::usage`] walks every head to
    /// sum the quota ledger's bytes. This is the index's own length, which the
    /// map already knows.
    ///
    /// Opaque Bodies are counted. They occupy the store exactly like the rest
    /// — retained, sealed, unread — and a footprint that excluded them would
    /// answer for less than the disk is actually holding.
    pub fn body_count(&self) -> u64 {
        u64::try_from(self.bodies.len()).unwrap_or(u64::MAX)
    }

    /// Physical upper estimate for the mutable Body record directory retained
    /// by this Replica, excluding immutable read generations and Corpora.
    /// Maintained on record replacement/removal, so Station admission does not
    /// scan the Space before a user action.
    pub const fn mutable_body_records_retained_bytes_estimate(&self) -> u64 {
        self.bodies.retained_bytes_estimate()
    }

    /// O(1) physical upper estimate for the long-lived mutable catalogs that
    /// coexist with immutable read publications. Payload objects, read images,
    /// and Corpora are deliberately excluded; receipt and declaration indexes
    /// are included because a record-shaped World otherwise appears to fit
    /// while a second million-entry metadata universe remains unpriced.
    pub fn mutable_retained_bytes_estimate(&self) -> u64 {
        self.bodies
            .retained_bytes_estimate()
            .saturating_add(self.receipts.retained_bytes_estimate())
            .saturating_add(
                self.receipt_cache
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .retained_bytes_estimate(),
            )
            .saturating_add(self.declared_content_retained_bytes)
    }

    /// When this Replica's durable material was last verified end to end, in
    /// milliseconds since the unix epoch.
    ///
    /// The verification is [`Replica::open`] itself: the journal re-reads every
    /// required object and re-derives its content address, then this crate
    /// verifies every signed transaction and opens every sealed envelope it
    /// holds a key for, failing without heuristic repair. There is no other
    /// whole-store pass and no public way to re-run one, so on a live Station
    /// this reads as the moment the Orbit was placed.
    ///
    /// `None` means no verification has ever run against this Replica — true of
    /// any non-durable one — and is reported as absent rather than as a zero or
    /// an epoch timestamp. A surface that drew "never checked" as `1970` or as
    /// `0` would be inventing an observation.
    pub fn verified_at_ms(&self) -> Option<u64> {
        self.verified_at_ms
    }

    fn freeze_body(&self, key: &BodyKey) -> Option<SnapshotBody> {
        let record = self.bodies.get(key)?;
        if !record.interpreted {
            return Some(SnapshotBody::opaque(
                key,
                record.binding.clone(),
                snapshot_stamp(&self.body_stamp(key)?),
                record.causal.clone(),
            ));
        }
        let stamp = snapshot_stamp(&self.body_stamp(key)?);
        if self.durable.is_some() {
            return Some(SnapshotBody::cold(
                key,
                record.binding.clone(),
                stamp,
                record.causal.clone()?,
            ));
        }
        let body = lock_fabric(&self.fabric)
            .body_snapshot(&fabric_key(key))
            .ok()
            .flatten()?;
        Some(SnapshotBody::resident(
            key,
            record.binding.clone(),
            stamp,
            body,
        ))
    }

    /// Durable Atomic state is a writer working set, not a second semantic
    /// store. The signed closure in `BodyRecord` and publication resolver own
    /// long-lived presence; after publication the Engine may release its Arc.
    fn release_durable_atomic_writer_image(&mut self, key: &BodyKey) {
        if self.durable.is_none()
            || !self.bodies.get(key).is_some_and(|record| {
                record.interpreted
                    && matches!(
                        record.binding.mutation_model,
                        MUTATION_ATOMIC | MUTATION_IMMUTABLE_ATOMIC
                    )
                    && record.causal.is_some()
            })
        {
            return;
        }
        lock_fabric(&self.fabric).release_atomic_image(&fabric_key(key));
    }

    fn replace_declared_content(&mut self, key: &BodyKey, refs: Vec<[u8; 32]>) {
        if let Some(prior) = self.declared_content.remove(key) {
            self.declared_content_retained_bytes = self
                .declared_content_retained_bytes
                .saturating_sub(declared_body_retained_estimate(&prior));
            for content in prior {
                match self.declared_content_counts.get_mut(&content) {
                    Some(count) if *count > 1 => *count = count.saturating_sub(1),
                    Some(_) => {
                        self.declared_content_counts.remove(&content);
                        self.declared_content_retained_bytes = self
                            .declared_content_retained_bytes
                            .saturating_sub(declared_count_retained_estimate());
                    }
                    None => {}
                }
                self.release_declaring_world(&content, &key.world);
            }
        }
        if refs.is_empty() {
            return;
        }
        for content in &refs {
            let count = self
                .declared_content_counts
                .entry(*content)
                .or_insert_with(|| {
                    self.declared_content_retained_bytes = self
                        .declared_content_retained_bytes
                        .saturating_add(declared_count_retained_estimate());
                    0
                });
            *count = count.saturating_add(1);
            let worlds = self.declared_content_worlds.entry(*content).or_default();
            match worlds.get_mut(&key.world) {
                Some(declaring) => *declaring = declaring.saturating_add(1),
                None => {
                    worlds.insert(key.world.clone(), 1);
                    self.declared_content_retained_bytes = self
                        .declared_content_retained_bytes
                        .saturating_add(declared_world_retained_estimate());
                }
            }
        }
        self.declared_content_retained_bytes = self
            .declared_content_retained_bytes
            .saturating_add(declared_body_retained_estimate(&refs));
        self.declared_content.insert(key.clone(), refs);
    }

    /// Drop one of `world`'s declarations of `content`, and drop the World
    /// itself when that was its last.
    fn release_declaring_world(&mut self, content: &[u8; 32], world: &WorldId) {
        let Some(worlds) = self.declared_content_worlds.get_mut(content) else {
            return;
        };
        let drop_world = match worlds.get_mut(world) {
            Some(declaring) if *declaring > 1 => {
                *declaring = declaring.saturating_sub(1);
                false
            }
            Some(_) => true,
            None => false,
        };
        if drop_world {
            worlds.remove(world);
            self.declared_content_retained_bytes = self
                .declared_content_retained_bytes
                .saturating_sub(declared_world_retained_estimate());
        }
        if worlds.is_empty() {
            self.declared_content_worlds.remove(content);
        }
    }

    /// The Worlds whose live Bodies declare `content`, sorted and unique.
    ///
    /// Empty means nothing declares these bytes — the same answer reachability
    /// acts on, and so no resource to authorize against.
    pub fn declaring_worlds(&self, content: &crate::content::ContentRef) -> Vec<WorldId> {
        self.declared_content_worlds
            .get(content.as_bytes())
            .map(|worlds| worlds.keys().cloned().collect())
            .unwrap_or_default()
    }

    /// The content a reader through this snapshot can see.
    ///
    /// Declared content, and content still under a hold. The hold is the
    /// window between committing bytes and a Body naming them, and a World
    /// deciding whether to name them is the thing that happens inside it --
    /// so a snapshot that showed only declared content could never answer
    /// "does this exist" for the one content anybody is about to declare.
    /// Every such pre-check refused, and what a person saw was that their
    /// request was invalid.
    ///
    /// Bounded by the number of open holds, which a TTL keeps small, rather
    /// than by everything ever committed.
    fn snapshot_content(&self) -> imbl::OrdMap<[u8; 32], crate::content::ContentDescriptor> {
        self.declared_content
            .values()
            .flatten()
            .copied()
            .chain(self.pending_content.keys().copied())
            .filter_map(|content_id| {
                let reference = crate::content::ContentRef { content_id };
                self.content_descriptor(&reference)
                    .map(|descriptor| (content_id, descriptor))
            })
            .collect()
    }

    /// Freeze the current committed state into a thread-safe read generation.
    /// This full form is used at activation and after an incorporation whose
    /// changed set is not locally known.
    pub fn read_snapshot(&self) -> ReadSnapshot {
        let mut builder = BodyDirectoryBuilder::default();
        for key in self.bodies.keys() {
            if let Some(body) = self.freeze_body(key) {
                builder.push(Arc::new(key.clone()), body);
            }
        }
        let bodies = builder.finish();
        let manifest = self.manifest_root();
        let root = if manifest == crate::transaction::NO_PARENT_ROOT {
            self.frontier.root
        } else {
            manifest
        };
        let retained_bytes_estimate = snapshot_directory_retained_estimate(&bodies);
        let resolver = match (&self.durable, &self.keys) {
            (Some(store), Some(keys)) => pin_body_image_resolver(
                store.reader(),
                keys,
                None,
                bodies
                    .iter()
                    .filter(|(_, body)| body.image.is_readable())
                    .filter_map(|(_, body)| body.image.material().map(Arc::as_ref)),
            )
            .ok(),
            _ => None,
        };
        ReadSnapshot {
            root,
            frontier: self.frontier,
            resolver,
            schema_bodies: schema_body_index(&bodies),
            schema_payload_bytes: schema_payload_index(&bodies),
            bodies,
            declared_content: self
                .declared_content
                .iter()
                .filter(|(_, refs)| !refs.is_empty())
                .map(|(key, refs)| (key.clone(), Arc::from(refs.as_slice())))
                .collect(),
            content: self.snapshot_content(),
            retained_bytes_estimate,
        }
    }

    /// Rebind a prepared local snapshot to the Journal root made
    /// authoritative by `PreparedAction::finalize`.
    ///
    /// Candidate construction happens before durability so its changed Atomic
    /// images remain resident and its resolver initially pins the prior root.
    /// Finalize then calls this O(1) seam before publication install. The
    /// shared resolver Arc is also held by the already-built Corpus, avoiding
    /// an O(all Bodies) image/pin rebuild after the commit point.
    pub fn attach_durable_body_image_root(&self, snapshot: &ReadSnapshot) {
        let Some(resolver) = &snapshot.resolver else {
            return;
        };
        let Some(store) = self.durable.as_ref() else {
            return;
        };
        resolver.attach_durable_root(store.reader());
    }

    /// Publish the next read generation by replacing only touched Body paths.
    /// Persistent-map structural sharing makes this O(changed log N), rather
    /// than cloning the World or scanning every Issue after every edit.
    pub fn advance_read_snapshot(&self, prior: &ReadSnapshot, changed: &[BodyKey]) -> ReadSnapshot {
        let mut bodies = prior.bodies.clone();
        let mut schema_bodies = prior.schema_bodies.clone();
        let mut schema_payload_bytes = prior.schema_payload_bytes.clone();
        let mut retained_bytes_estimate = prior.retained_bytes_estimate;
        let mut declared_content = prior.declared_content.clone();
        let mut content = prior.content.clone();
        let mut touched_content = BTreeSet::new();
        let resolver = match (&self.durable, &self.keys) {
            (Some(store), Some(keys)) => pin_body_image_resolver(
                store.reader(),
                keys,
                prior.resolver.as_deref(),
                changed.iter().filter_map(|key| {
                    self.bodies
                        .get(key)
                        .filter(|record| record.interpreted)
                        .and_then(|record| record.causal.as_deref())
                }),
            )
            .ok()
            .or_else(|| prior.resolver.clone()),
            _ => prior.resolver.clone(),
        };
        let mut unique: BTreeSet<&BodyKey> = BTreeSet::new();
        for key in changed {
            if !unique.insert(key) {
                continue;
            }
            let shared_key = if let Some((held, prior_body)) = prior.bodies.get_key_value(key) {
                retained_bytes_estimate = retained_bytes_estimate
                    .saturating_sub(snapshot_body_retained_estimate(prior_body));
                if prior_body.image.is_readable() {
                    remove_schema_body(&mut schema_bodies, key, &prior_body.binding);
                    adjust_schema_payload(&mut schema_payload_bytes, key, prior_body, false);
                }
                held.clone()
            } else {
                Arc::new(key.clone())
            };
            match self.freeze_body(key) {
                Some(body) => {
                    retained_bytes_estimate = retained_bytes_estimate
                        .saturating_add(snapshot_body_retained_estimate(&body));
                    let binding = body.binding.clone();
                    let readable = body.image.is_readable();
                    if readable {
                        adjust_schema_payload(&mut schema_payload_bytes, key, &body, true);
                    }
                    bodies.insert(shared_key, body);
                    if readable {
                        if let Some(body_ix) = bodies.body_ix(key) {
                            insert_schema_body(&mut schema_bodies, body_ix, key, &binding);
                        }
                    }
                }
                None => {
                    bodies.remove(key);
                }
            }
            if let Some(prior_refs) = declared_content.remove(key) {
                touched_content.extend(prior_refs.iter().copied());
            }
            if let Some(next_refs) = self
                .declared_content
                .get(key)
                .filter(|refs| !refs.is_empty())
            {
                touched_content.extend(next_refs.iter().copied());
                declared_content.insert(key.clone(), Arc::from(next_refs.as_slice()));
            }
        }
        for content_id in touched_content {
            if self
                .declared_content_counts
                .get(&content_id)
                .copied()
                .unwrap_or(0)
                == 0
            {
                content.remove(&content_id);
                continue;
            }
            let reference = crate::content::ContentRef { content_id };
            match self.content_descriptor(&reference) {
                Some(descriptor) => {
                    content.insert(content_id, descriptor);
                }
                None => {
                    // A committed declaration cannot name absent content. If
                    // a damaged store reaches this point, keeping a prior
                    // descriptor would be the more dangerous lie.
                    content.remove(&content_id);
                }
            }
        }
        let manifest = self.manifest_root();
        let root = if manifest == crate::transaction::NO_PARENT_ROOT {
            self.frontier.root
        } else {
            manifest
        };
        ReadSnapshot {
            root,
            frontier: self.frontier,
            resolver,
            bodies,
            schema_bodies,
            schema_payload_bytes,
            declared_content,
            content,
            retained_bytes_estimate,
        }
    }

    /// Advance only the Replica publication coordinate while sharing the
    /// complete interpreted Body and reachable-content image.
    ///
    /// This is the content-plane path for descriptor commits and sweeps which
    /// move the signed Manifest/frontier but do not change a Body declaration.
    /// It is O(1): Worlds receive a new exact coordinate over the same facts.
    pub fn advance_read_snapshot_metadata(&self, prior: &ReadSnapshot) -> ReadSnapshot {
        let manifest = self.manifest_root();
        let root = if manifest == crate::transaction::NO_PARENT_ROOT {
            self.frontier.root
        } else {
            manifest
        };
        ReadSnapshot {
            root,
            frontier: self.frontier,
            resolver: prior.resolver.clone(),
            bodies: prior.bodies.clone(),
            schema_bodies: prior.schema_bodies.clone(),
            schema_payload_bytes: prior.schema_payload_bytes.clone(),
            declared_content: prior.declared_content.clone(),
            // Carry the prior image forward and add any content held since it
            // was taken. This is the path a content ingest republishes
            // through, so without it a just-written content is invisible to
            // the very submit that was going to declare it. O(open holds),
            // not O(content).
            content: {
                let mut content = prior.content.clone();
                for content_id in self.pending_content.keys() {
                    if content.contains_key(content_id) {
                        continue;
                    }
                    let reference = crate::content::ContentRef {
                        content_id: *content_id,
                    };
                    if let Some(descriptor) = self.content_descriptor(&reference) {
                        content.insert(*content_id, descriptor);
                    }
                }
                content
            },
            retained_bytes_estimate: prior.retained_bytes_estimate,
        }
    }

    fn generation_delta(&self, root: &[u8; 32]) -> Result<Option<GenerationDelta>, Failure> {
        let Some(store) = self.durable.as_ref() else {
            return Ok(None);
        };
        let Some(value) = crate::index::lookup(
            &StoreNodes(store),
            self.generation_index_root,
            &generation_index_key(root),
        )
        .map_err(|error| integrity_cause(Defect::Index, "look up durable generation", error))?
        else {
            return Ok(None);
        };
        let indexed: IndexedGeneration = postcard::from_bytes(&value).map_err(|error| {
            integrity_cause(Defect::Encoding, "decode durable generation index", error)
        })?;
        if &indexed.root != root {
            return Err(Failure::Integrity(Defect::Encoding));
        }
        let bytes = store.read_object(&indexed.object).map_err(|error| {
            integrity_cause(Defect::Encoding, "read durable generation delta", error)
        })?;
        let delta: GenerationDelta = postcard::from_bytes(&bytes).map_err(|error| {
            integrity_cause(Defect::Encoding, "decode durable generation delta", error)
        })?;
        if delta.format_version != GENERATION_DELTA_FORMAT_VERSION || &delta.root != root {
            return Err(Failure::Integrity(Defect::Encoding));
        }
        Ok(Some(delta))
    }

    fn generation_artifact(&self, reference: &ArtifactRef) -> Result<Artifact, Failure> {
        let store = self.durable.as_ref().ok_or(Failure::Poisoned)?;
        let object = Object {
            hash: reference.hash,
            len: reference.len,
        };
        let envelope = store.read_object(&object).map_err(|error| {
            integrity_cause(Defect::CorruptMaterial, "read generation artifact", error)
        })?;
        let epoch = mechanics::authorization::body_epoch_id(&envelope)
            .ok_or(Failure::Integrity(Defect::CorruptMaterial))?;
        if epoch != reference.epoch {
            return Err(Failure::Integrity(Defect::CorruptMaterial));
        }
        let opening = self
            .keys
            .as_ref()
            .and_then(|keys| keys.opening_key(&epoch))
            .ok_or(Failure::Integrity(Defect::MissingMaterial))?;
        open_artifact(&opening, &envelope).map_err(|_| Failure::Integrity(Defect::CorruptMaterial))
    }

    fn body_from_causal_material(
        &self,
        key: &BodyKey,
        material: &CausalMaterial,
    ) -> Result<fabric::BodySnapshot, Failure> {
        material
            .validate()
            .map_err(|_| Failure::Integrity(Defect::CorruptMaterial))?;
        let mut engine = Engine::new();
        for reference in std::iter::once(&material.checkpoint).chain(&material.delta_tail) {
            let artifact = self.generation_artifact(reference)?;
            let status = engine
                .import_artifact(&fabric_key(key), &artifact)
                .map_err(|_| Failure::Integrity(Defect::CorruptMaterial))?;
            if status.pending {
                return Err(Failure::Integrity(Defect::CorruptMaterial));
            }
        }
        let version = engine
            .version(&fabric_key(key))
            .map_err(|_| Failure::Integrity(Defect::CorruptMaterial))?;
        if version != material.version {
            return Err(Failure::Integrity(Defect::CorruptMaterial));
        }
        engine
            .body_snapshot(&fabric_key(key))
            .map_err(|error| {
                integrity_cause(
                    Defect::Encoding,
                    "project body from durable causal material",
                    error,
                )
            })?
            .ok_or(Failure::Integrity(Defect::MissingMaterial))
    }

    /// Pin an immutable generation reader at this Replica commit point.
    /// `current` is Runtime's already-published O(1) read image; passing it in
    /// avoids an accidental all-Body freeze merely to create the handle.
    pub fn generation_reader(&self, current: Arc<ReadSnapshot>) -> GenerationReader {
        GenerationReader {
            store: self.durable.as_ref().map(Store::reader),
            keys: self.keys.clone(),
            generation_index_root: self.generation_index_root,
            current,
        }
    }

    /// Pin an immutable durable receipt reader at this commit point.
    /// Returns `None` for scratch/non-durable Replicas, whose receipt directory
    /// remains in memory and preserves the preexisting behavior.
    pub fn receipt_reader(&self) -> Option<ReceiptReader> {
        let store = self.durable.as_ref()?;
        Some(ReceiptReader {
            store: store.reader(),
            receipt_index_root: self.receipt_index_root,
            cache: Arc::clone(&self.receipt_cache),
            sequence: store.manifest().map_or(0, |manifest| manifest.sequence),
            footprint: ReceiptFootprint {
                count: self.receipt_count,
                material_bytes: self.receipt_material_bytes,
                cache_upper_bound: HOT_RECEIPT_CACHE_BYTES,
                cold_lookup_transient_upper_bound: MAX_RECEIPT_OBJECT_BYTES
                    .saturating_mul(2)
                    .saturating_add(u64::try_from(crate::index::MAX_NODE_BYTES).unwrap_or(u64::MAX))
                    .saturating_add(1024 * 1024),
            },
        })
    }

    /// Reconstruct an exact durable generation. The cold cost is proportional
    /// to the deltas on its ancestry; Runtime caches the resulting immutable
    /// snapshot, so every subsequent query is a normal shared read.
    pub fn read_generation(&self, root: &[u8; 32]) -> Result<Option<ReadSnapshot>, Failure> {
        let manifest = self.manifest_root();
        let current_root = if manifest == crate::transaction::NO_PARENT_ROOT {
            self.frontier.root
        } else {
            manifest
        };
        if current_root == *root {
            return Ok(Some(self.read_snapshot()));
        }
        let mut cursor = *root;
        let mut seen = BTreeSet::new();
        let mut deltas = Vec::new();
        loop {
            if deltas.len()
                >= usize::try_from(MAX_GENERATION_RECONSTRUCTION_DEPTH).unwrap_or(usize::MAX)
            {
                return Err(Failure::Integrity(Defect::Encoding));
            }
            if !seen.insert(cursor) {
                return Err(Failure::Integrity(Defect::Encoding));
            }
            let Some(delta) = self.generation_delta(&cursor)? else {
                return Ok(None);
            };
            let parent = delta.parent;
            deltas.push(delta);
            let Some(parent) = parent else {
                break;
            };
            cursor = parent;
        }
        let frontier = deltas
            .first()
            .map(|delta| delta.frontier)
            .ok_or(Failure::Integrity(Defect::Encoding))?;
        // Resolve the exact visible archive entry for each Body before key
        // pinning. Superseded generations must not make a historical read
        // depend on an otherwise unreachable retired epoch.
        let mut selected = BTreeMap::<BodyKey, &ArchivedBody>::new();
        for delta in &deltas {
            for archived in &delta.changed {
                selected.entry(archived.key.clone()).or_insert(archived);
            }
        }
        let resolver = match (&self.durable, &self.keys) {
            (Some(store), Some(keys)) => Some(
                pin_body_image_resolver(
                    store.reader(),
                    keys,
                    None,
                    selected.values().filter_map(|archived| {
                        archived
                            .present
                            .then_some(())
                            .filter(|_| archived.interpreted)
                            .and_then(|_| archived.binding.as_ref())
                            .and(archived.material.as_deref())
                    }),
                )
                .map_err(|_| Failure::Integrity(Defect::MissingMaterial))?,
            ),
            _ => None,
        };
        let mut bodies = BodyDirectory::default();
        for archived in selected.values() {
            if !archived.present {
                continue;
            }
            let binding = archived
                .binding
                .as_ref()
                .ok_or(Failure::Integrity(Defect::Encoding))?;
            let stamp = snapshot_stamp(&archived.stamp);
            let body = if !archived.interpreted {
                SnapshotBody::opaque(
                    &archived.key,
                    binding.clone(),
                    stamp,
                    archived.material.clone(),
                )
            } else {
                let material = archived
                    .material
                    .as_ref()
                    .ok_or(Failure::Integrity(Defect::MissingMaterial))?;
                SnapshotBody::cold(&archived.key, binding.clone(), stamp, material.clone())
            };
            bodies.insert(Arc::new(archived.key.clone()), body);
        }
        drop(selected);
        let mut content = imbl::OrdMap::new();
        for delta in deltas.into_iter().rev() {
            for descriptor in delta.descriptors {
                content.insert(descriptor.content_ref().content_id, descriptor);
            }
            for content_id in delta.removed_descriptors {
                content.remove(&content_id);
            }
        }
        let retained_bytes_estimate = snapshot_directory_retained_estimate(&bodies);
        Ok(Some(ReadSnapshot {
            root: *root,
            frontier,
            resolver,
            schema_bodies: schema_body_index(&bodies),
            schema_payload_bytes: schema_payload_index(&bodies),
            bodies,
            // Historical snapshots are immutable and are never accepted as a
            // base for `advance_read_snapshot`; the root/frontier guard on the
            // prepared path enforces that. Descriptor reachability is already
            // reconstructed in `content`, so no read loses information.
            declared_content: imbl::OrdMap::new(),
            content,
            retained_bytes_estimate,
        }))
    }

    /// Every durable generation coordinate, deterministically ordered by
    /// semantic frontier and then id.
    pub fn read_generations(&self) -> Result<Vec<ReadGeneration>, Failure> {
        let Some(store) = self.durable.as_ref() else {
            return Ok(vec![ReadGeneration {
                root: if self.manifest_root() == crate::transaction::NO_PARENT_ROOT {
                    self.frontier.root
                } else {
                    self.manifest_root()
                },
                parent: None,
                frontier: self.frontier,
            }]);
        };
        let mut indexed = Vec::new();
        let mut decode_failure = None;
        crate::index::stream(
            &StoreNodes(store),
            self.generation_index_root,
            &mut |entry| match postcard::from_bytes::<IndexedGeneration>(&entry.value) {
                Ok(value) => indexed.push(value),
                Err(error) => decode_failure = Some(format!("{error:?}")),
            },
        )
        .map_err(|error| {
            integrity_cause(Defect::Index, "stream durable generation index", error)
        })?;
        if let Some(reason) = decode_failure {
            return Err(Failure::IntegrityCause {
                defect: Defect::Encoding,
                operation: "decode durable generation catalog",
                reason,
            });
        }
        let mut result = Vec::with_capacity(indexed.len());
        for generation in indexed {
            let Some(delta) = self.generation_delta(&generation.root)? else {
                return Err(Failure::Integrity(Defect::MissingMaterial));
            };
            result.push(ReadGeneration {
                root: delta.root,
                parent: delta.parent,
                frontier: delta.frontier,
            });
        }
        result.sort_by(|left, right| {
            left.frontier
                .transaction_count
                .cmp(&right.frontier.transaction_count)
                .then_with(|| left.root.cmp(&right.root))
        });
        Ok(result)
    }

    /// Look up a request in the persistent-idempotency scope
    /// `(Space, World, Device, RequestId)`. An identical payload hash returns
    /// the original receipt — the caller must **not** reapply; a different
    /// payload hash under the same scope is a typed conflict; an unknown scope
    /// is `None` (commit may proceed).
    pub fn lookup_action(
        &self,
        space: &SpaceId,
        world: &WorldId,
        device: &mechanics::ids::DeviceId,
        request: &[u8; 16],
        payload_hash: &[u8; 32],
    ) -> Result<Option<RequestReceipt>, Failure> {
        let key = crate::receipt::scope_key(space, world, device, request);
        if let Some(reader) = self.receipt_reader() {
            return reader.lookup_action(space, world, device, request, payload_hash);
        }
        match self.receipts.get(&key).map(|held| &held.0) {
            None => Ok(None),
            Some(receipt) if &receipt.payload_hash == payload_hash => Ok(Some(receipt.clone())),
            Some(_) => Err(Failure::RequestIdConflict),
        }
    }

    /// Commit staged operations **without** durable attribution. Valid only on
    /// a non-durable Replica (tests/scratch): a durable store requires the
    /// signed-transaction path ([`Replica::commit_action`] or
    /// [`Replica::incorporate`]).
    pub fn commit(
        &mut self,
        request_label: &str,
        ops: &[(BodyKey, Op)],
    ) -> Result<ReplicaFrontier, Failure> {
        if self.durable.is_some() {
            return Err(Failure::Illegitimate(
                "a durable Replica commits only signed, attributed transactions".into(),
            ));
        }
        self.mutation_available()?;
        let receipt = self.apply_ops(request_label, ops)?;
        // Track minimal body records so bindings/tombstones behave uniformly.
        self.update_records_unattributed(ops)?;
        self.frontier = advance(self.frontier, receipt.causal().as_bytes());
        Ok(self.frontier)
    }

    /// Prepare a request under its persistent-idempotency scope without
    /// publishing it. Identical replay returns the original receipt without
    /// opening a candidate. A fresh request returns an owned
    /// [`PreparedAction`]; callers release the Replica writer while deriving
    /// its immutable publication and later reacquire it for exact-parent
    /// validation and durable finalize.
    #[allow(clippy::too_many_arguments)]
    pub fn prepare_action(
        &mut self,
        ctx: &CommitContext<'_>,
        auth: &CommitAuthorization<'_>,
        interpretation: crate::receipt::Interpretation,
        world: &WorldId,
        device: &mechanics::ids::DeviceId,
        request: &[u8; 16],
        payload_hash: &[u8; 32],
        effect: Vec<u8>,
        bodies: Vec<BodyKey>,
        request_label: &str,
        ops: &[(BodyKey, Op)],
        bindings: &[(BodyKey, BodyBinding)],
        content_refs: &[(BodyKey, Vec<crate::content::ContentRef>)],
    ) -> Result<PreparedActionOutcome, Failure> {
        self.prepare_action_inner(
            ctx,
            auth,
            interpretation,
            world,
            device,
            request,
            payload_hash,
            effect,
            bodies,
            request_label,
            ops,
            bindings,
            content_refs,
            None,
        )
    }

    /// Prepare after an owned immutable reader proved this exact scope absent.
    /// Validation is fixed-size and performs no journal/index/object I/O. A
    /// different current sequence/root or scope is [`Failure::ReceiptCheckStale`].
    #[allow(clippy::too_many_arguments)]
    pub fn prepare_action_checked(
        &mut self,
        ctx: &CommitContext<'_>,
        auth: &CommitAuthorization<'_>,
        interpretation: crate::receipt::Interpretation,
        world: &WorldId,
        device: &mechanics::ids::DeviceId,
        request: &[u8; 16],
        payload_hash: &[u8; 32],
        effect: Vec<u8>,
        bodies: Vec<BodyKey>,
        request_label: &str,
        ops: &[(BodyKey, Op)],
        bindings: &[(BodyKey, BodyBinding)],
        content_refs: &[(BodyKey, Vec<crate::content::ContentRef>)],
        absence: ReceiptAbsence,
    ) -> Result<PreparedActionOutcome, Failure> {
        self.prepare_action_inner(
            ctx,
            auth,
            interpretation,
            world,
            device,
            request,
            payload_hash,
            effect,
            bodies,
            request_label,
            ops,
            bindings,
            content_refs,
            Some(absence),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn prepare_action_inner(
        &mut self,
        ctx: &CommitContext<'_>,
        auth: &CommitAuthorization<'_>,
        interpretation: crate::receipt::Interpretation,
        world: &WorldId,
        device: &mechanics::ids::DeviceId,
        request: &[u8; 16],
        payload_hash: &[u8; 32],
        effect: Vec<u8>,
        bodies: Vec<BodyKey>,
        request_label: &str,
        ops: &[(BodyKey, Op)],
        bindings: &[(BodyKey, BodyBinding)],
        content_refs: &[(BodyKey, Vec<crate::content::ContentRef>)],
        absence: Option<ReceiptAbsence>,
    ) -> Result<PreparedActionOutcome, Failure> {
        self.mutation_available()?;
        if let Some(absence) = absence {
            let current_sequence = self
                .durable
                .as_ref()
                .and_then(|store| store.manifest())
                .map_or(0, |manifest| manifest.sequence);
            let scope = crate::receipt::scope_key(ctx.space, world, device, request);
            if self.durable.is_none()
                || absence.sequence != current_sequence
                || absence.receipt_index_root != self.receipt_index_root
                || absence.scope != scope
            {
                return Err(Failure::ReceiptCheckStale);
            }
        } else if let Some(receipt) =
            self.lookup_action(ctx.space, world, device, request, payload_hash)?
        {
            return Ok(PreparedActionOutcome::Replayed(receipt));
        }
        if effect.len() > crate::receipt::MAX_EFFECT_BYTES {
            return Err(Failure::EffectTooLarge);
        }
        let claim = PreparationClaim::acquire(&self.prepared_in_flight)?;
        let parent_root = self.current_manifest_root();
        let parent_frontier = self.frontier;
        // An idempotent no-op: no operations, nothing applied, the frontier
        // does not advance — but the receipt is still recorded durably so an
        // identical retry replays instead of re-running the World.
        if ops.is_empty() {
            let receipt = RequestReceipt {
                version: 2,
                space: ctx.space.clone(),
                world: world.clone(),
                device: device.clone(),
                request: *request,
                payload_hash: *payload_hash,
                effect,
                bodies,
                frontier: self.frontier,
                manifest_root: auth.parent_manifest_root,
                implementation_digest: interpretation.implementation_digest,
                extractor_schema_digest: interpretation.extractor_schema_digest,
                transaction: [0u8; 32],
            };
            let receipt_bytes = validate_receipt_for_storage(&receipt)?;
            if self
                .usage()
                .0
                .saturating_add(u64::try_from(receipt_bytes.len()).unwrap_or(u64::MAX))
                > self.quota.max_space_bytes
            {
                return Err(Failure::QuotaExceeded);
            }
            let snapshot = self.prepared_snapshot_context(None);
            return Ok(PreparedActionOutcome::Prepared(PreparedAction {
                fabric: Arc::clone(&self.fabric),
                in_flight: claim.transfer(),
                rollback_poisoned: Arc::clone(&self.rollback_poisoned),
                parent_root,
                parent_frontier,
                snapshot,
                state: Some(PreparedActionState::Noop { receipt }),
            }));
        }
        // Space pinning: one store, one Space.
        match &self.space {
            None => self.space = Some(ctx.space.clone()),
            Some(space) if space == ctx.space => {}
            Some(_) => {
                return Err(Failure::Illegitimate(
                    "commit addressed to a different Space".into(),
                ));
            }
        }
        // Validate schema-binding immutability BEFORE anything is applied.
        let bindings: BTreeMap<&BodyKey, &BodyBinding> =
            bindings.iter().map(|(k, b)| (k, b)).collect();
        let mut touched: Vec<BodyKey> = ops.iter().map(|(k, _)| k.clone()).collect();
        touched.sort();
        touched.dedup();
        for key in &touched {
            match (self.bodies.get(key), bindings.get(key)) {
                (Some(record), Some(declared)) if &&record.binding != declared => {
                    return Err(Failure::SchemaMismatch);
                }
                (None, None) => {
                    return Err(Failure::SchemaMismatch);
                }
                _ => {}
            }
        }
        // Create-once atomic values carry their own convergence proof in the
        // address. Validate it before opening a Fabric candidate, and prohibit
        // every operation that could turn the Body into mutable state. A
        // second transaction with byte-identical material is harmless; a
        // different value cannot target the same Body key.
        let mut immutable_values: BTreeMap<&BodyKey, &[u8]> = BTreeMap::new();
        for (key, op) in ops {
            let binding = bindings
                .get(key)
                .copied()
                .or_else(|| self.bodies.get(key).map(|record| &record.binding))
                .ok_or(Failure::SchemaMismatch)?;
            if binding.mutation_model != MUTATION_IMMUTABLE_ATOMIC {
                continue;
            }
            let Op::ReplaceAtomic { value } = op else {
                return Err(Failure::ImmutableConflict);
            };
            if !immutable_key_matches(
                key,
                &binding.schema,
                binding.schema_version,
                &binding.encoding,
                value,
            ) {
                return Err(Failure::ImmutableConflict);
            }
            if let Some(prior) = immutable_values.insert(key, value) {
                if prior != value {
                    return Err(Failure::ImmutableConflict);
                }
            }
            if let Some(record) = self.bodies.get(key) {
                // The canonical value is committed into the immutable Body
                // address. Once both the existing binding and proposed value
                // validate against that address, retaining/inflating a second
                // plaintext copy merely to compare it is unnecessary. An
                // opaque record remains ineligible until explicit revalidation.
                if !record.interpreted {
                    return Err(Failure::ImmutableConflict);
                }
            }
        }
        // The World's content declaration, validated before anything applies.
        // Shallow on purpose — bounds, order, and that every named descriptor
        // is committed — because anything deeper would mean decoding the
        // product bytes it describes, which is the boundary it exists to
        // respect.
        let mut declared: BTreeMap<BodyKey, Vec<[u8; 32]>> = BTreeMap::new();
        for (key, refs) in content_refs {
            if refs.len() > crate::manifest::MAX_CONTENT_REFS_PER_BODY {
                return Err(Failure::QuotaExceeded);
            }
            for reference in refs {
                if self.content_descriptor(reference).is_none() {
                    return Err(Failure::Illegitimate(
                        "a declaration names content with no committed descriptor".into(),
                    ));
                }
            }
            let mut ids: Vec<[u8; 32]> = refs.iter().map(|r| *r.as_bytes()).collect();
            ids.sort();
            ids.dedup();
            declared.insert(key.clone(), ids);
        }

        // Body-count quota, reserved under the writer BEFORE anything applies.
        let new_bodies = u64::try_from(
            touched
                .iter()
                .filter(|k| !self.bodies.contains_key(*k))
                .count(),
        )
        .unwrap_or(u64::MAX);
        if u64::try_from(self.bodies.len())
            .unwrap_or(u64::MAX)
            .saturating_add(new_bodies)
            > self.quota.max_space_bodies
        {
            return Err(Failure::QuotaExceeded);
        }
        // A durable commit needs sealing material before the engine moves; a
        // non-durable Replica with keys still seals (so its material can be
        // exported), and one without keys commits locally-only.
        let sealing = match self.keys.as_ref().and_then(|k| k.sealing_key()) {
            Some(key) => Some(key),
            None if self.durable.is_some() => return Err(Failure::BodyKeyUnavailable),
            None => None,
        };

        let fabric = self.prepare_ops(request_label, ops)?;
        let assembled = (|| -> Result<PreparedMutation, Failure> {
            let next_frontier = advance(self.frontier, fabric.receipt().causal().as_bytes());
            let chain_seed = mint_chain_seed()?;

            // Build per-Body chain advances and records for every touched Body.
            let mut new_records: BTreeMap<BodyKey, Option<BodyRecord>> = BTreeMap::new();
            let mut sealed: Vec<(BodyKey, Vec<u8>, CausalMaterial)> = Vec::new();
            let mut new_artifacts = Vec::new();
            for key in &touched {
                // Drop the Engine mutex guard before the arm calls
                // `next_causal_material`, which resolves/export deltas through
                // the same shared writer. A `match` scrutinee temporary lives
                // through its arms and would otherwise self-deadlock.
                let export = { lock_fabric(&self.fabric).export_body(&fabric_key(key)) };
                match export {
                    None => {
                        new_records.insert(key.clone(), None);
                    }
                    Some(_) => {
                        let base = self
                            .bodies
                            .get(key)
                            .map(|record| record.chain)
                            .unwrap_or(ReplicaFrontier::EMPTY);
                        let binding = match bindings.get(key) {
                            Some(binding) => (*binding).clone(),
                            None => self
                                .bodies
                                .get(key)
                                .map(|record| record.binding.clone())
                                .ok_or(Failure::SchemaMismatch)?,
                        };
                        let mut record = BodyRecord {
                            binding,
                            chain: advance_chain(base, &chain_seed),
                            heads: smallvec::smallvec![BodyHead {
                                tx: [0u8; 32],
                                descriptor_hash: [0u8; 32],
                                tx_commitment: [0u8; 32],
                                artifacts: Some(Vec::new().into_boxed_slice()),
                                transaction: None,
                                artifact_bytes: 0,
                                tx_len: 0,
                            }],
                            causal: self
                                .bodies
                                .get(key)
                                .and_then(|record| record.causal.clone()),
                            interpreted: true,
                        };
                        if sealing.is_some() {
                            let prior =
                                self.bodies.get(key).and_then(|body| body.causal.as_deref());
                            let (material, artifacts) =
                                self.next_causal_material(key, &record, prior, &new_artifacts)?;
                            let pack = encode_artifact_pack(&artifacts)?;
                            new_artifacts.extend(artifacts);
                            record.causal = Some(Arc::new(material.clone()));
                            sealed.push((key.clone(), pack, material));
                        }
                        new_records.insert(key.clone(), Some(record));
                    }
                }
            }

            let transaction = if sealing.is_some() {
                let mut descriptors = Vec::new();
                for (key, _, material) in &sealed {
                    let record = new_records
                        .get(key)
                        .and_then(Option::as_ref)
                        .ok_or(Failure::Integrity(Defect::MissingMaterial))?;
                    descriptors.push(Descriptor {
                        world: key.world.clone(),
                        body: key.body.clone(),
                        schema: record.binding.schema.clone(),
                        schema_version: record.binding.schema_version,
                        encoding: record.binding.encoding.clone(),
                        mutation_model: record.binding.mutation_model,
                        base_frontier: self
                            .bodies
                            .get(key)
                            .map(|body| body.chain)
                            .unwrap_or(ReplicaFrontier::EMPTY),
                        resulting_frontier: record.chain,
                        material: material.clone(),
                    });
                }
                descriptors.sort_by_key(Descriptor::key);
                let tx = Transaction::sign_with(
                    SignRequest {
                        space: ctx.space,
                        parent_manifest_root: auth.parent_manifest_root,
                        replica_frontier: next_frontier,
                        authority_frontier: ctx.authority_frontier.clone(),
                        actor: auth.actor,
                        operation: *request,
                        intent_digest: auth.intent_digest,
                        operations_digest: operations_digest(ops),
                        demand: auth.demand.clone(),
                        descriptors,
                    },
                    ctx.signer,
                    |core| auth.authorizer.authorize(core),
                )
                .map_err(Failure::Unauthorized)?;
                if tx.encode().len() > crate::transaction::MAX_TRANSACTION {
                    return Err(Failure::OpLimit);
                }
                let tx_id = tx.id();
                for key in &touched {
                    if let Some(Some(record)) = new_records.get_mut(key) {
                        record.head_mut()?.tx = tx_id;
                    }
                }
                Some(tx)
            } else {
                None
            };

            let mut receipt = RequestReceipt {
                version: 2,
                space: ctx.space.clone(),
                world: world.clone(),
                device: device.clone(),
                request: *request,
                payload_hash: *payload_hash,
                effect,
                bodies,
                frontier: next_frontier,
                manifest_root: [0u8; 32],
                implementation_digest: interpretation.implementation_digest,
                extractor_schema_digest: interpretation.extractor_schema_digest,
                transaction: transaction
                    .as_ref()
                    .map(Transaction::id)
                    .unwrap_or([0u8; 32]),
            };
            let receipt_bytes = validate_receipt_for_storage(&receipt)?;

            if let Some(tx) = &transaction {
                Self::populate_local_record_refs(
                    tx,
                    &sealed,
                    &mut new_records,
                    self.durable.is_some(),
                )?;
                let (mut projected, _) = self.usage();
                for (key, pack, _) in &sealed {
                    let artifact_len = new_records
                        .get(key)
                        .and_then(Option::as_ref)
                        .map(BodyRecord::protected_total)
                        .ok_or(Failure::Integrity(Defect::MissingMaterial))?;
                    if artifact_len > self.quota.max_body_bytes
                        || u64::try_from(pack.len()).unwrap_or(u64::MAX) > self.quota.max_body_bytes
                    {
                        return Err(Failure::QuotaExceeded);
                    }
                    projected = projected.saturating_add(artifact_len);
                    if let Some(old) = self.bodies.get(key) {
                        projected = projected.saturating_sub(old.protected_total());
                    }
                }
                projected =
                    projected.saturating_add(u64::try_from(tx.encode().len()).unwrap_or(u64::MAX));
                projected = projected
                    .saturating_add(u64::try_from(receipt_bytes.len()).unwrap_or(u64::MAX));
                if projected > self.quota.max_space_bytes {
                    return Err(Failure::QuotaExceeded);
                }
            } else if self
                .usage()
                .0
                .saturating_add(u64::try_from(receipt_bytes.len()).unwrap_or(u64::MAX))
                > self.quota.max_space_bytes
            {
                return Err(Failure::QuotaExceeded);
            }

            let candidate_root =
                self.preview_manifest_root(ctx, &new_records, &declared, next_frontier)?;
            receipt.manifest_root = candidate_root;
            validate_receipt_for_storage(&receipt)?;
            Ok(PreparedMutation {
                new_records,
                sealed,
                transaction,
                receipt,
                next_frontier,
                declared,
                candidate_root,
                manifest_space: ctx.space.clone(),
                manifest_authority_frontier: ctx.authority_frontier.clone(),
                manifest_signer: ctx.signer.signer_key(),
            })
        })();

        match assembled {
            Ok(data) => {
                let snapshot = self.prepared_snapshot_context(Some(&data));
                Ok(PreparedActionOutcome::Prepared(PreparedAction {
                    fabric: Arc::clone(&self.fabric),
                    in_flight: claim.transfer(),
                    rollback_poisoned: Arc::clone(&self.rollback_poisoned),
                    parent_root,
                    parent_frontier,
                    snapshot,
                    state: Some(PreparedActionState::Mutation { fabric, data }),
                }))
            }
            Err(error) => {
                if lock_fabric(&self.fabric).rollback(fabric).is_err() {
                    self.poisoned = true;
                    Err(Failure::OutcomeUnknown)
                } else {
                    Err(error)
                }
            }
        }
    }

    /// Preserve the original one-call surface for callers that do not need to
    /// validate a derived read generation before durability.
    #[allow(clippy::too_many_arguments)]
    pub fn commit_action(
        &mut self,
        ctx: &CommitContext<'_>,
        auth: &CommitAuthorization<'_>,
        world: &WorldId,
        device: &mechanics::ids::DeviceId,
        request: &[u8; 16],
        payload_hash: &[u8; 32],
        effect: Vec<u8>,
        bodies: Vec<BodyKey>,
        request_label: &str,
        ops: &[(BodyKey, Op)],
        bindings: &[(BodyKey, BodyBinding)],
        content_refs: &[(BodyKey, Vec<crate::content::ContentRef>)],
    ) -> Result<ActionOutcome, Failure> {
        match self.prepare_action(
            ctx,
            auth,
            crate::receipt::Interpretation::UNSPECIFIED,
            world,
            device,
            request,
            payload_hash,
            effect,
            bodies,
            request_label,
            ops,
            bindings,
            content_refs,
        )? {
            PreparedActionOutcome::Replayed(receipt) => Ok(ActionOutcome::Replayed(receipt)),
            PreparedActionOutcome::Prepared(prepared) => {
                prepared.finalize(self, ctx).map(ActionOutcome::Committed)
            }
        }
    }

    /// Incorporate remote material through the Convergence pipeline. The signed
    /// [`Transaction`] is verified — structure, signature, **and signer
    /// standing at its referenced authority frontier through mechanics** — and
    /// every provided payload must match its descriptor's ciphertext
    /// commitment **before** any byte reaches the engine. Supported, openable
    /// material becomes exact per-Body Engine changes; unsupported-but-
    /// legitimate material is retained opaquely, byte-identically. Never
    /// reachable from a World or an ordinary Session. Durability before
    /// acknowledgment applies exactly as for a local commit.
    pub fn incorporate(
        &mut self,
        ctx: &CommitContext<'_>,
        tx: &Transaction,
        payloads: &[(BodyKey, Vec<u8>)],
        authority: &dyn AuthoritySource,
    ) -> Result<ConvergenceOutcome, Failure> {
        self.mutation_available()?;
        // `export_material_excluding` is receiver-relative and may omit
        // content-addressed artifacts which this Replica declared through an
        // older retained head. Reconstruct each descriptor's complete signed
        // closure before entering the one incorporation pipeline, exactly as
        // Contact validation does. A false declaration can therefore only
        // make the claimant fail to complete the closure; it cannot admit
        // incomplete causal material.
        let mut complete = Vec::with_capacity(payloads.len());
        for (key, delivery) in payloads {
            let descriptor = tx
                .core
                .descriptors
                .iter()
                .find(|descriptor| &descriptor.key() == key)
                .ok_or(Failure::Illegitimate(Invalid::IncompleteMaterial))?;
            complete.push((
                key.clone(),
                self.complete_artifact_delivery(key, descriptor, delivery)?,
            ));
        }
        self.incorporate_units(
            ctx,
            &[(tx.clone(), complete)],
            &BTreeMap::new(),
            &BTreeMap::new(),
            &[],
            authority,
        )
    }

    /// The one Convergence adoption path: incorporate a set of validated
    /// transaction units **atomically**. Every unit is verified, classified,
    /// and quota-projected against the complete resulting state first; the
    /// engine then applies; and the durable store performs exactly **one**
    /// journal commit installing every object and the replacement Manifest —
    /// a failure in transaction N never leaves transactions 0..N-1 committed
    /// under an error result, and a crash at any staging or journal boundary
    /// exposes the complete old or the complete new root.
    /// The declarations rule 8 validated that this bundle would otherwise drop.
    ///
    /// Rule 8 ([`Replica::validate_contact`]) checks the content ids of **every**
    /// entry in the advertised manifest and the sealed bundle carries their
    /// descriptors — but incorporation only ever reached [`Replica::persist`]
    /// for Bodies the bundle *changed*. A declaration for a Body we already hold
    /// at the advertised head was therefore validated, sealed, and then dropped
    /// on the floor. Its trigger — "the receiver already holds this head" — only
    /// ever becomes more true, so it never repaired: two honest, fully converged
    /// peers ended up with published roots that could never agree.
    ///
    /// Returns the `changed`-shaped map `persist` wants, each key mapped to the
    /// record it *already* has. The refcount pass then nets to zero (prior and
    /// now are the same object set) and only the declaration, the manifest entry
    /// and the content catalog move.
    fn declarations_left_behind(
        &self,
        bundle_declared: &BTreeMap<BodyKey, Vec<[u8; 32]>>,
        bundle_declaration_heads: &BTreeMap<BodyKey, Vec<ManifestHead>>,
        bundle_descriptors: &[crate::content::ContentDescriptor],
    ) -> BTreeMap<BodyKey, Option<BodyRecord>> {
        let mut pending: BTreeMap<BodyKey, Option<BodyRecord>> = BTreeMap::new();
        for (key, refs) in bundle_declared {
            let Some(record) = self.bodies.get(key) else {
                continue;
            };
            let local_heads: Vec<ManifestHead> = record
                .heads
                .iter()
                .map(|head| ManifestHead {
                    descriptor_hash: head.descriptor_hash,
                    transaction_commitment: head.tx_commitment,
                })
                .collect();
            if bundle_declaration_heads.get(key) != Some(&local_heads) {
                continue;
            }
            let held = self.declared_content.get(key).map(Vec::as_slice);
            if held != Some(refs.as_slice()) {
                pending.insert(key.clone(), Some(record.clone()));
            }
        }
        // A descriptor the catalog is missing has to land even when the
        // declaration itself already matches — that gap is this defect's own
        // residue on any replica that ran the dropping version.
        if pending.is_empty()
            && bundle_descriptors
                .iter()
                .any(|d| self.content_descriptor(&d.content_ref()).is_none())
        {
            for key in bundle_declared.keys() {
                if let Some(record) = self.bodies.get(key) {
                    let local_heads: Vec<ManifestHead> = record
                        .heads
                        .iter()
                        .map(|head| ManifestHead {
                            descriptor_hash: head.descriptor_hash,
                            transaction_commitment: head.tx_commitment,
                        })
                        .collect();
                    if bundle_declaration_heads.get(key) != Some(&local_heads) {
                        continue;
                    }
                    pending.insert(key.clone(), Some(record.clone()));
                }
            }
        }
        pending
    }

    /// Adopt [`Replica::declarations_left_behind`] on a path that is about to
    /// return without changing a single Body. One journal commit, no Body
    /// records altered, frontier untouched — the bundle was already validated.
    fn adopt_declarations_only(
        &mut self,
        ctx: &CommitContext<'_>,
        bundle_declared: &BTreeMap<BodyKey, Vec<[u8; 32]>>,
        bundle_declaration_heads: &BTreeMap<BodyKey, Vec<ManifestHead>>,
        bundle_descriptors: &[crate::content::ContentDescriptor],
    ) -> Result<(), Failure> {
        if self.durable.is_none() {
            return Ok(());
        }
        let mut pending = self.declarations_left_behind(
            bundle_declared,
            bundle_declaration_heads,
            bundle_descriptors,
        );
        if pending.is_empty() {
            return Ok(());
        }
        let mut causal = Vec::new();
        for (key, refs) in bundle_declared {
            if pending.contains_key(key) {
                causal.extend_from_slice(&body_index_key(key));
                for reference in refs {
                    causal.extend_from_slice(reference);
                }
            }
        }
        let frontier = advance_published(self.frontier, &causal);
        self.persist(
            Some(ctx),
            &mut pending,
            bundle_declared,
            bundle_descriptors,
            None,
            Vec::new(),
            frontier,
        )?;
        self.frontier = frontier;
        Ok(())
    }

    /// Revalidate a complete opaque head set from the bytes already retained
    /// locally. These transactions were authorized and commitment-checked when
    /// they first landed, so this is deliberately not another incorporation:
    /// it adds no transaction units, consumes no quota, and does not advance
    /// the published frontier. Either every retained head can now be opened and
    /// interpreted, or the record remains opaque. Returns the upgraded Body
    /// keys and the number of engine changes accepted while importing them.
    fn upgrade_retained_opaque(
        &mut self,
        ctx: &CommitContext<'_>,
    ) -> Result<(Vec<BodyKey>, u32), Failure> {
        // A key arrives through the authority section, independently of which
        // Body payloads the peer needed to resend. Revisit every retained Body:
        // limiting this to the current bundle's units leaves already-held
        // material opaque forever after an authority-only admission pass.
        let keys: Vec<BodyKey> = if self.durable.is_some() {
            self.bodies
                .iter()
                .filter(|(_, record)| !record.interpreted)
                .map(|(key, _)| key.clone())
                .collect()
        } else {
            self.raw_material.keys().cloned().collect()
        };
        let mut upgraded_keys = Vec::new();
        let mut accepted = 0u32;
        for key in &keys {
            let Some(record) = self.bodies.get(key).cloned() else {
                continue;
            };
            if record.interpreted {
                continue;
            }
            let retained = if let Some(retained) = self.raw_material.get(key).cloned() {
                retained
            } else {
                let Some(store) = self.durable.as_ref() else {
                    continue;
                };
                let mut retained = Vec::with_capacity(record.heads.len());
                for head in &record.heads {
                    let transaction = head
                        .transaction
                        .ok_or(Failure::Integrity(Defect::MissingMaterial))?;
                    let transaction_bytes = store
                        .read_object(&transaction)
                        .map_err(|_| Failure::Integrity(Defect::MissingMaterial))?;
                    let signed = Transaction::decode_canonical(&transaction_bytes)
                        .map_err(|_| Failure::Integrity(Defect::Encoding))?;
                    let descriptor = signed
                        .core
                        .descriptors
                        .iter()
                        .find(|descriptor| descriptor.key() == *key)
                        .ok_or(Failure::Integrity(Defect::MissingMaterial))?;
                    let envelopes = record
                        .artifacts(head)
                        .map(|reference| {
                            store
                                .read_object(&artifact_object(reference))
                                .map_err(|_| Failure::Integrity(Defect::MissingMaterial))
                        })
                        .collect::<Result<Vec<_>, _>>()?;
                    retained.push((
                        head.tx,
                        encode_artifact_pack(&envelopes)?,
                        transaction_bytes,
                    ));
                    // The signed descriptor and stored head must still name
                    // the same exact closure before any protected byte opens.
                    if descriptor_hash(descriptor) != head.descriptor_hash {
                        return Err(Failure::Integrity(Defect::CorruptMaterial));
                    }
                }
                retained
            };
            let Some((encoding, model)) = self.supported.lookup(
                &key.world,
                &record.binding.schema,
                record.binding.schema_version,
            ) else {
                continue;
            };
            if encoding != &record.binding.encoding {
                continue;
            }

            let mut opened = Vec::with_capacity(retained.len());
            for (tx_id, pack, transaction_bytes) in &retained {
                let transaction =
                    Transaction::decode_canonical(transaction_bytes).map_err(|error| {
                        Failure::IllegitimateContact {
                            kind: Invalid::Encoding,
                            reason: format!("retained transaction is not canonical: {error}"),
                        }
                    })?;
                let Some(descriptor) = transaction
                    .core
                    .descriptors
                    .iter()
                    .find(|descriptor| descriptor.key() == *key)
                else {
                    return Err(Failure::Integrity(Defect::MissingMaterial));
                };
                let envelopes = decode_artifact_pack(descriptor, pack)?;
                if descriptor.mutation_model != *model {
                    opened.clear();
                    break;
                }
                let mut artifacts = Vec::with_capacity(envelopes.len());
                for envelope in &envelopes {
                    let Some(epoch) = mechanics::authorization::body_epoch_id(envelope) else {
                        opened.clear();
                        break;
                    };
                    let Some(opening) =
                        self.keys.as_ref().and_then(|keys| keys.opening_key(&epoch))
                    else {
                        opened.clear();
                        break;
                    };
                    artifacts.push(open_artifact(&opening, envelope).map_err(|error| {
                        Failure::IllegitimateContact {
                            kind: Invalid::IncompleteMaterial,
                            reason: format!("retained Body artifact could not be opened: {error}"),
                        }
                    })?);
                }
                if artifacts.len() != envelopes.len() {
                    break;
                }
                let mut proof = Engine::new();
                for artifact in &artifacts {
                    proof
                        .import_artifact(&fabric_key(key), artifact)
                        .map_err(|_| {
                            Failure::Engine(EngineFailure::Invalid(fabric::commit::Invalid::Import))
                        })?;
                }
                if proof.version(&fabric_key(key)).map_err(|_| {
                    Failure::Engine(EngineFailure::Invalid(fabric::commit::Invalid::Import))
                })? != descriptor.material.version
                {
                    return Err(Failure::Integrity(Defect::CorruptMaterial));
                }
                validate_immutable_proof(key, descriptor, &proof)?;
                opened.push((*tx_id, descriptor.clone(), artifacts));
            }
            if opened.len() != retained.len() {
                continue;
            }
            opened.sort_by_key(|(tx_id, _, _)| *tx_id);
            let mut chain: Option<ReplicaFrontier> = None;
            for (_, descriptor, _) in &opened {
                chain = Some(match (descriptor.mutation_model, chain) {
                    (_, None) => descriptor.resulting_frontier,
                    (MUTATION_ATOMIC | MUTATION_IMMUTABLE_ATOMIC, Some(current)) => {
                        if chain_order(&descriptor.resulting_frontier, &current).is_gt() {
                            descriptor.resulting_frontier
                        } else {
                            current
                        }
                    }
                    (MUTATION_COLLABORATIVE, Some(current)) => {
                        combine_chains(&current, &descriptor.resulting_frontier)
                    }
                    _ => return Err(Failure::Integrity(Defect::CorruptMaterial)),
                });
            }
            for (_, descriptor, artifacts) in &opened {
                if is_atomic_mutation(descriptor.mutation_model)
                    && Some(descriptor.resulting_frontier) != chain
                {
                    continue;
                }
                let mut applied = false;
                for artifact in artifacts {
                    applied |= lock_fabric(&self.fabric)
                        .import_artifact(&fabric_key(key), artifact)
                        .map_err(|_| {
                            Failure::Engine(EngineFailure::Invalid(fabric::commit::Invalid::Import))
                        })?
                        .applied;
                }
                if applied {
                    accepted = accepted.saturating_add(1);
                }
            }
            let mut upgraded = record;
            upgraded.interpreted = true;
            if let Some(chain) = chain {
                upgraded.chain = chain;
            }
            let causal = if opened.len() == 1 {
                opened
                    .first()
                    .map(|(_, descriptor, _)| Arc::new(descriptor.material.clone()))
            } else {
                None
            };
            upgraded.replace_causal(causal)?;
            if self.durable.is_some() {
                let mut changed = BTreeMap::from([(key.clone(), Some(upgraded.clone()))]);
                self.persist(
                    Some(ctx),
                    &mut changed,
                    &BTreeMap::new(),
                    &[],
                    None,
                    Vec::new(),
                    self.frontier,
                )?;
                upgraded = changed
                    .remove(key)
                    .flatten()
                    .ok_or(Failure::Integrity(Defect::MissingMaterial))?;
            }
            self.bodies.insert(key.clone(), upgraded);
            // A non-durable Replica still needs the bytes in order to re-serve
            // the heads it advertises. Durable stores can read their objects.
            if self.durable.is_some() {
                self.raw_material.remove(key);
            }
            upgraded_keys.push(key.clone());
        }
        Ok((upgraded_keys, accepted))
    }

    #[allow(clippy::too_many_arguments)]
    fn incorporate_units(
        &mut self,
        ctx: &CommitContext<'_>,
        units: &[IncorporationUnit],
        bundle_declared: &BTreeMap<BodyKey, Vec<[u8; 32]>>,
        bundle_declaration_heads: &BTreeMap<BodyKey, Vec<ManifestHead>>,
        bundle_descriptors: &[crate::content::ContentDescriptor],
        authority: &dyn AuthoritySource,
    ) -> Result<ConvergenceOutcome, Failure> {
        if self.poisoned {
            return Err(Failure::Poisoned);
        }
        if units.len() > crate::convergence::MAX_TRANSACTION_CHANGES {
            return Err(Failure::QuotaExceeded);
        }
        let previous = self.frontier;
        let mut outcome = ConvergenceOutcome::unchanged(previous);
        let (upgraded, accepted) = self.upgrade_retained_opaque(ctx)?;
        outcome.accepted = accepted;
        outcome.bodies = upgraded;
        if units.is_empty() {
            // No Body material, but the manifest may still declare content for
            // Bodies we already hold — validated by rule 8 and dropped here.
            self.adopt_declarations_only(
                ctx,
                bundle_declared,
                bundle_declaration_heads,
                bundle_descriptors,
            )?;
            return Ok(outcome);
        }

        // ---- Phase 1: legitimacy for EVERY transaction, before anything. ----
        let mut tx_space: Option<SpaceId> = None;
        for (tx, _) in units {
            tx.verify_authorized(authority)
                .map_err(|_| Failure::Illegitimate(Invalid::Signature))?;
            let space = std::str::from_utf8(&tx.core.space)
                .ok()
                .and_then(SpaceId::parse)
                .ok_or_else(|| Failure::Illegitimate("space id".into()))?;
            match (&tx_space, &self.space) {
                (Some(prev), _) if prev != &space => {
                    return Err(Failure::Illegitimate(
                        "transactions address different Spaces".into(),
                    ));
                }
                (_, Some(bound)) if bound != &space => {
                    return Err(Failure::Illegitimate(
                        "transaction addressed to a different Space".into(),
                    ));
                }
                _ => tx_space = Some(space),
            }
        }

        // ---- Phase 2: resolve packs to signed closures; bounds; references. --
        type ResolvedPack<'a> = (usize, [u8; 32], &'a Descriptor, &'a [u8], Vec<Vec<u8>>);
        let mut resolved: Vec<ResolvedPack<'_>> = Vec::new();
        for (idx, (tx, payloads)) in units.iter().enumerate() {
            let tx_id = tx.id();
            for (key, payload) in payloads {
                if payload.len() > MAX_BODY_BYTES {
                    return Err(Failure::Illegitimate(
                        "payload exceeds the Body maximum".into(),
                    ));
                }
                let descriptor = tx
                    .core
                    .descriptors
                    .iter()
                    .find(|d| &d.key() == key)
                    .ok_or_else(|| {
                        Failure::Illegitimate("payload without a matching descriptor".into())
                    })?;
                let artifacts = decode_artifact_pack(descriptor, payload)?;
                resolved.push((idx, tx_id, descriptor, payload, artifacts));
            }
        }
        // Classification writes a per-Body overlay, so transaction-id order
        // must not decide the meaning of a mixed bundle. Process locally
        // interpretable heads first and opaque heads second; the latter then
        // joins the staged record and conservatively leaves the whole Body
        // opaque, preserving every head until all can be interpreted.
        resolved.sort_by(
            |(_, left_tx, left, _, left_artifacts), (_, right_tx, right, _, right_artifacts)| {
                let rank = |descriptor: &Descriptor, artifacts: &[Vec<u8>]| {
                    let supported = self
                        .supported
                        .lookup(
                            &descriptor.key().world,
                            &descriptor.schema,
                            descriptor.schema_version,
                        )
                        .is_some();
                    let openable = artifacts.iter().all(|envelope| {
                        mechanics::authorization::body_epoch_id(envelope)
                            .and_then(|epoch| self.keys.as_ref()?.opening_key(&epoch))
                            .is_some()
                    });
                    u8::from(!(supported && openable))
                };
                left.key()
                    .cmp(&right.key())
                    .then_with(|| rank(left, left_artifacts).cmp(&rank(right, right_artifacts)))
                    .then_with(|| left_tx.cmp(right_tx))
            },
        );

        // ---- Phase 3: classification over an overlay of the current index. --
        // Each planned change carries everything the engine + persist phases
        // need; the overlay makes successive same-Body writes within one
        // bundle classify against the staged (not the committed) state.
        struct Planned {
            unit: usize,
            key: BodyKey,
            pack: Vec<u8>,
            /// `Some` when every artifact opens and is locally interpreted;
            /// `None` for the opaque branch.
            artifacts: Option<Vec<Artifact>>,
            material: CausalMaterial,
            record: BodyRecord,
            /// A concurrent collaborative merge: the staged head JOINS the
            /// existing record's head set (the union is the state) instead of
            /// replacing it — replacing would advertise a single-author
            /// envelope as if it contained the merge, which restart and every
            /// downstream Contact would then silently lose.
            merge_append: bool,
        }
        let mut planned: Vec<Planned> = Vec::new();
        // Overlay: the latest staged (chain, interpreted) per key.
        let mut overlay: BTreeMap<BodyKey, (ReplicaFrontier, bool)> = BTreeMap::new();
        for (unit, _, descriptor, pack, envelopes) in &resolved {
            let key = descriptor.key();
            let transaction = units
                .get(*unit)
                .map(|(transaction, _)| transaction)
                .ok_or(Failure::Illegitimate(Invalid::IncompleteMaterial))?;
            let transaction_bytes = transaction.encode();
            // Immutable schema binding across replicas too.
            if let Some(record) = self.bodies.get(&key) {
                if record.binding.schema != descriptor.schema
                    || record.binding.schema_version != descriptor.schema_version
                    || record.binding.encoding != descriptor.encoding
                    || record.binding.mutation_model != descriptor.mutation_model
                {
                    outcome.rejected = outcome.rejected.saturating_add(1);
                    continue;
                }
            }
            let current_chain = overlay
                .get(&key)
                .map(|(chain, _)| *chain)
                .or_else(|| self.bodies.get(&key).map(|r| r.chain));
            let was_opaque = overlay
                .get(&key)
                .map(|(_, interpreted)| !interpreted)
                .unwrap_or_else(|| self.bodies.get(&key).is_some_and(|r| !r.interpreted));
            let supported =
                self.supported
                    .lookup(&key.world, &descriptor.schema, descriptor.schema_version);
            let mut opened = supported.map(|_| Vec::with_capacity(envelopes.len()));
            let mut invalid_artifact = false;
            if let Some(artifacts) = &mut opened {
                for envelope in envelopes {
                    let Some(epoch) = mechanics::authorization::body_epoch_id(envelope) else {
                        opened = None;
                        break;
                    };
                    let Some(opening) =
                        self.keys.as_ref().and_then(|keys| keys.opening_key(&epoch))
                    else {
                        opened = None;
                        break;
                    };
                    match open_artifact(&opening, envelope) {
                        Ok(artifact) => artifacts.push(artifact),
                        Err(_) => {
                            outcome.rejected = outcome.rejected.saturating_add(1);
                            invalid_artifact = true;
                            opened = None;
                            break;
                        }
                    }
                }
            }
            if invalid_artifact {
                continue;
            }
            match (supported, opened) {
                (Some((encoding, model)), Some(artifacts)) => {
                    if encoding != &descriptor.encoding {
                        outcome.rejected = outcome.rejected.saturating_add(1);
                        continue;
                    }
                    // A head an INTERPRETED record already carries (same
                    // transaction commitment) is known material regardless of
                    // chain bookkeeping. An opaque record's known head must
                    // still fall through: re-receiving it with the schema and
                    // key epoch now available IS the upgrade/revalidation
                    // path.
                    let staged_commitment = tx_commitment(&transaction_bytes);
                    if self
                        .bodies
                        .get(&key)
                        .is_some_and(|r| r.interpreted && r.has_commitment(&staged_commitment))
                    {
                        outcome.unchanged = outcome.unchanged.saturating_add(1);
                        continue;
                    }
                    if descriptor.mutation_model != *model {
                        outcome.rejected = outcome.rejected.saturating_add(1);
                        continue;
                    }
                    let mut proof = Engine::new();
                    for artifact in &artifacts {
                        let _ =
                            proof
                                .import_artifact(&fabric_key(&key), artifact)
                                .map_err(|_| {
                                    Failure::Engine(EngineFailure::Invalid(
                                        fabric::commit::Invalid::Import,
                                    ))
                                })?;
                    }
                    if proof.version(&fabric_key(&key)).map_err(|_| {
                        Failure::Engine(EngineFailure::Invalid(fabric::commit::Invalid::Import))
                    })? != descriptor.material.version
                    {
                        outcome.rejected = outcome.rejected.saturating_add(1);
                        continue;
                    }
                    validate_immutable_proof(&key, descriptor, &proof)?;
                    // Material retained opaquely upgrades to interpreted the
                    // first time a supported schema AND its key epoch are both
                    // available — this IS the revalidation path.
                    let apply = was_opaque
                        || match (descriptor.mutation_model, current_chain) {
                            // Fresh body: apply.
                            (_, None) => true,
                            // Already known (chain equality): unchanged.
                            (_, Some(chain)) if chain == descriptor.resulting_frontier => false,
                            // Descends our current chain: apply.
                            (_, Some(chain)) if chain == descriptor.base_frontier => true,
                            // Concurrent atomic: the deterministic maximum wins.
                            (MUTATION_ATOMIC | MUTATION_IMMUTABLE_ATOMIC, Some(chain)) => {
                                chain_order(&descriptor.resulting_frontier, &chain)
                                    == std::cmp::Ordering::Greater
                            }
                            // Concurrent collaborative: the engine merges causally.
                            (MUTATION_COLLABORATIVE, Some(_)) => true,
                            _ => false,
                        };
                    if !apply {
                        outcome.unchanged = outcome.unchanged.saturating_add(1);
                        continue;
                    }
                    // Fast-forward (fresh body, opaque upgrade, or a payload
                    // descending our chain): the incoming envelope CONTAINS
                    // our state, so its head REPLACES the set. A concurrent
                    // collaborative payload does not — it joins the set.
                    let fast_forward = was_opaque
                        || match (descriptor.mutation_model, current_chain) {
                            (_, None) => true,
                            (_, Some(chain)) if chain == descriptor.base_frontier => true,
                            (MUTATION_ATOMIC | MUTATION_IMMUTABLE_ATOMIC, Some(_)) => true,
                            (MUTATION_COLLABORATIVE, Some(_)) => false,
                            _ => false,
                        };
                    let chain = match descriptor.mutation_model {
                        MUTATION_ATOMIC | MUTATION_IMMUTABLE_ATOMIC => {
                            descriptor.resulting_frontier
                        }
                        MUTATION_COLLABORATIVE => match current_chain {
                            None => descriptor.resulting_frontier,
                            Some(chain) => combine_chains(&chain, &descriptor.resulting_frontier),
                        },
                        _ => return Err(Failure::Illegitimate(Invalid::Encoding)),
                    };
                    overlay.insert(key.clone(), (chain, true));
                    planned.push(Planned {
                        unit: *unit,
                        key: key.clone(),
                        pack: pack.to_vec(),
                        artifacts: Some(artifacts),
                        material: descriptor.material.clone(),
                        record: BodyRecord {
                            binding: BodyBinding {
                                schema: descriptor.schema.clone(),
                                schema_version: descriptor.schema_version,
                                encoding: descriptor.encoding.clone(),
                                mutation_model: *model,
                            },
                            chain,
                            heads: smallvec::smallvec![BodyHead {
                                tx: transaction.id(),
                                descriptor_hash: descriptor_hash(descriptor),
                                tx_commitment: staged_commitment,
                                artifacts: (!fast_forward).then(|| {
                                    descriptor
                                        .artifact_refs()
                                        .copied()
                                        .collect::<Vec<_>>()
                                        .into_boxed_slice()
                                }),
                                transaction: None,
                                artifact_bytes: descriptor
                                    .artifact_refs()
                                    .fold(0u64, |sum, reference| sum.saturating_add(reference.len)),
                                tx_len: u64::try_from(transaction_bytes.len()).unwrap_or(u64::MAX),
                            }],
                            causal: fast_forward.then(|| Arc::new(descriptor.material.clone())),
                            interpreted: true,
                        },
                        merge_append: !fast_forward,
                    });
                }
                _ => {
                    // The opaque branch: authorized, commitment-bound material
                    // for an unavailable World/schema or a missing key epoch.
                    // Retain byte-identically; never call a World, never
                    // decrypt, never import into the engine.
                    let already = self.raw_material.get(&key).is_some_and(|entries| {
                        entries
                            .iter()
                            .any(|(held_tx, _, _)| held_tx == &transaction.id())
                    });
                    if already {
                        outcome.unchanged = outcome.unchanged.saturating_add(1);
                        continue;
                    }
                    let chain = match (descriptor.mutation_model, current_chain) {
                        (MUTATION_COLLABORATIVE, Some(current))
                            if current != descriptor.base_frontier =>
                        {
                            combine_chains(&current, &descriptor.resulting_frontier)
                        }
                        (MUTATION_ATOMIC | MUTATION_IMMUTABLE_ATOMIC, Some(current))
                            if chain_order(&descriptor.resulting_frontier, &current).is_lt() =>
                        {
                            current
                        }
                        _ => descriptor.resulting_frontier,
                    };
                    overlay.insert(key.clone(), (chain, false));
                    planned.push(Planned {
                        unit: *unit,
                        key: key.clone(),
                        pack: pack.to_vec(),
                        artifacts: None,
                        material: descriptor.material.clone(),
                        record: BodyRecord {
                            binding: BodyBinding {
                                schema: descriptor.schema.clone(),
                                schema_version: descriptor.schema_version,
                                encoding: descriptor.encoding.clone(),
                                mutation_model: descriptor.mutation_model,
                            },
                            chain,
                            heads: smallvec::smallvec![BodyHead {
                                tx: transaction.id(),
                                descriptor_hash: descriptor_hash(descriptor),
                                tx_commitment: tx_commitment(&transaction_bytes),
                                artifacts: current_chain.is_some().then(|| {
                                    descriptor
                                        .artifact_refs()
                                        .copied()
                                        .collect::<Vec<_>>()
                                        .into_boxed_slice()
                                }),
                                transaction: None,
                                artifact_bytes: descriptor
                                    .artifact_refs()
                                    .fold(0u64, |sum, reference| sum.saturating_add(reference.len)),
                                tx_len: u64::try_from(transaction_bytes.len()).unwrap_or(u64::MAX),
                            }],
                            causal: Some(Arc::new(descriptor.material.clone())),
                            interpreted: false,
                        },
                        // Opaque material is retained byte-identically per
                        // author: a distinct envelope for a Body we already
                        // hold joins the set rather than replacing it.
                        //
                        // "Already hold" must be asked of the STAGED view, not
                        // the committed one. `overlay` exists so that successive
                        // writes to one Body inside a single bundle classify
                        // against what this bundle has already planned; every
                        // other classification in this loop honours it, and the
                        // placeholder chain ten lines above already does. Asking
                        // `self.bodies` here instead made two opaque heads of a
                        // Body that is NEW to this replica both plan a replace,
                        // so the second silently overwrote the first — and the
                        // receiver then served that truncation onward as a
                        // complete, root-validated Body.
                        merge_append: current_chain.is_some(),
                    });
                }
            }
        }

        if planned.is_empty() {
            self.adopt_declarations_only(
                ctx,
                bundle_declared,
                bundle_declaration_heads,
                bundle_descriptors,
            )?;
            return Ok(outcome);
        }

        // ---- Phase 4: quota projection over the COMPLETE resulting state. --
        // Every planned change adds its envelope; a REPLACING change (fast
        // forward) additionally reclaims the old head set, once per key. An
        // appending change (concurrent merge head) reclaims nothing — its
        // material joins the set. The projection is conservative: it never
        // under-counts the resulting ledger.
        {
            let (mut projected_bytes, mut projected_bodies) = self.usage();
            let mut opaque_delta: BTreeMap<WorldId, (u64, u64)> = BTreeMap::new();
            let mut counted_tx: BTreeSet<usize> = BTreeSet::new();
            let mut seen_key: BTreeSet<BodyKey> = BTreeSet::new();
            for change in &planned {
                let artifact_len = change.record.protected_total();
                if artifact_len > self.quota.max_body_bytes
                    || u64::try_from(change.pack.len()).unwrap_or(u64::MAX)
                        > self.quota.max_body_bytes
                {
                    return Err(Failure::QuotaExceeded);
                }
                let old = self.bodies.get(&change.key);
                projected_bytes = projected_bytes.saturating_add(artifact_len);
                let first_for_key = seen_key.insert(change.key.clone());
                if first_for_key {
                    match old {
                        Some(old_record) if !change.merge_append => {
                            projected_bytes =
                                projected_bytes.saturating_sub(old_record.protected_total());
                        }
                        Some(_) => {}
                        None => projected_bodies = projected_bodies.saturating_add(1),
                    }
                }
                if counted_tx.insert(change.unit) {
                    let transaction_len = units
                        .get(change.unit)
                        .map(|(transaction, _)| transaction.encode().len())
                        .and_then(|len| u64::try_from(len).ok())
                        .ok_or(Failure::Illegitimate(Invalid::IncompleteMaterial))?;
                    projected_bytes = projected_bytes.saturating_add(transaction_len);
                }
                if !change.record.interpreted {
                    let entry = opaque_delta
                        .entry(change.key.world.clone())
                        .or_insert((0, 0));
                    entry.0 = entry.0.saturating_add(artifact_len);
                    if old.is_none() && first_for_key {
                        entry.1 = entry.1.saturating_add(1);
                    }
                }
            }
            if projected_bytes > self.quota.max_space_bytes
                || projected_bodies > self.quota.max_space_bodies
            {
                return Err(Failure::QuotaExceeded);
            }
            for (world, (dbytes, dbodies)) in opaque_delta {
                let (cur_bytes, cur_bodies) = self.opaque_usage(&world);
                if cur_bytes.saturating_add(dbytes) > self.quota.max_unknown_world_bytes
                    || cur_bodies.saturating_add(dbodies) > self.quota.max_unknown_world_bodies
                {
                    return Err(Failure::OpaqueQuotaExceeded);
                }
            }
        }

        // Space pinning happens only once nothing can refuse the bundle.
        if self.space.is_none() {
            self.space = tx_space;
        }

        // ---- Phase 5: engine application, in unit order. ---------------------
        // Per-unit causal evidence drives the frontier advance, matching the
        // sequential single-transaction semantics exactly.
        struct AcceptedChange {
            key: BodyKey,
            pack: Vec<u8>,
            record: BodyRecord,
            unit: usize,
            merge_append: bool,
        }
        let mut changed: Vec<AcceptedChange> = Vec::new();
        let mut unit_causal: BTreeMap<usize, Vec<u8>> = BTreeMap::new();
        for change in planned {
            match &change.artifacts {
                Some(artifacts) => {
                    let mut applied = false;
                    let mut failed = false;
                    for artifact in artifacts {
                        match lock_fabric(&self.fabric)
                            .import_artifact(&fabric_key(&change.key), artifact)
                        {
                            Ok(status) => applied |= status.applied,
                            Err(_) => failed = true,
                        }
                    }
                    if failed {
                        self.poisoned = true;
                        return Err(Failure::Engine(EngineFailure::Invalid(
                            fabric::commit::Invalid::Import,
                        )));
                    }
                    if change.record.binding.mutation_model == MUTATION_COLLABORATIVE {
                        let current = lock_fabric(&self.fabric)
                            .version(&fabric_key(&change.key))
                            .map_err(|_| {
                                Failure::Engine(EngineFailure::Invalid(
                                    fabric::commit::Invalid::Import,
                                ))
                            })?;
                        if !matches!(
                            lock_fabric(&self.fabric).relation(
                                &fabric_key(&change.key),
                                &current,
                                &change.material.version,
                            ),
                            CausalRelation::Equal | CausalRelation::Dominates
                        ) {
                            self.poisoned = true;
                            return Err(Failure::Engine(EngineFailure::Invalid(
                                fabric::commit::Invalid::Import,
                            )));
                        }
                    }
                    if !applied {
                        outcome.unchanged = outcome.unchanged.saturating_add(1);
                        continue;
                    }
                    outcome.accepted = outcome.accepted.saturating_add(1);
                    let causal = unit_causal.entry(change.unit).or_default();
                    let head = change.record.head()?;
                    for reference in change.record.artifacts(head) {
                        causal.extend_from_slice(&reference.hash);
                    }
                    changed.push(AcceptedChange {
                        key: change.key,
                        pack: change.pack,
                        record: change.record,
                        unit: change.unit,
                        merge_append: change.merge_append,
                    });
                }
                None => {
                    outcome.unsupported_retained = outcome.unsupported_retained.saturating_add(1);
                    unit_causal.entry(change.unit).or_default();
                    changed.push(AcceptedChange {
                        key: change.key,
                        pack: change.pack,
                        record: change.record,
                        unit: change.unit,
                        merge_append: change.merge_append,
                    });
                }
            }
        }

        if changed.is_empty() {
            outcome.current = previous;
            self.adopt_declarations_only(
                ctx,
                bundle_declared,
                bundle_declaration_heads,
                bundle_descriptors,
            )?;
            return Ok(outcome);
        }
        outcome
            .bodies
            .extend(changed.iter().map(|change| change.key.clone()));
        outcome.bodies.sort();
        outcome.bodies.dedup();

        // Preserve the bundle's canonical transaction-id order and the exact
        // signed attribution for every transaction that contributed material.
        // `units` is canonicalized by `validate_contact`'s BTreeMap; the direct
        // incorporation surface supplies one unit. No union attribution is
        // invented for a multi-actor bundle.
        for unit in unit_causal.keys() {
            let transaction = units
                .get(*unit)
                .map(|(transaction, _)| transaction)
                .ok_or(Failure::Illegitimate(Invalid::IncompleteMaterial))?;
            let mut bodies: Vec<BodyKey> = changed
                .iter()
                .filter(|change| change.unit == *unit)
                .map(|change| change.key.clone())
                .collect();
            bodies.sort();
            bodies.dedup();
            if bodies.is_empty() {
                continue;
            }
            let actor = mechanics::ids::ActorId::parse(&transaction.core.actor)
                .ok_or(Failure::Illegitimate(Invalid::Encoding))?;
            outcome.changes.push(crate::convergence::TransactionChange {
                operation: transaction.core.operation,
                actor,
                device: mechanics::ids::DeviceId::from_key_bytes(&transaction.core.signer),
                bodies,
            });
        }

        // Frontier: advance once per unit that contributed changes, in unit
        // order, from that unit's transaction id + engine causal evidence.
        let mut next_frontier = previous;
        for (idx, causal_tail) in &unit_causal {
            let touched = changed.iter().any(|c| &c.unit == idx);
            if !touched {
                continue;
            }
            let mut causal = Vec::with_capacity(16usize.saturating_add(causal_tail.len()));
            let transaction = units
                .get(*idx)
                .map(|(transaction, _)| transaction)
                .ok_or(Failure::Illegitimate(Invalid::IncompleteMaterial))?;
            causal.extend_from_slice(&transaction.id());
            causal.extend_from_slice(causal_tail);
            next_frontier = advance(next_frontier, &causal);
        }

        // ---- Phase 6: fold accepted changes into final head sets, then ONE
        // durable commit for the complete bundle. A fast-forward change
        // REPLACES a Body's head set (its envelope contains the prior state);
        // a concurrent merge head JOINS it — the union of head envelopes is
        // the merged state every restart and every downstream Contact must be
        // able to reproduce.
        let mut final_records: BTreeMap<BodyKey, BodyRecord> = BTreeMap::new();
        // Every accepted (envelope, transaction) pair, per head — all of it
        // must land durable (or in raw material) for re-serving.
        let mut staged_material: Vec<(BodyKey, [u8; 32], Vec<u8>, usize)> = Vec::new();
        for change in changed {
            let staged_head = change.record.head()?.clone();
            staged_material.push((
                change.key.clone(),
                staged_head.tx,
                change.pack.clone(),
                change.unit,
            ));
            let base = final_records
                .remove(&change.key)
                .or_else(|| self.bodies.get(&change.key).cloned());
            let folded = match (base, change.merge_append) {
                (Some(mut existing), true) => {
                    // The singleton sentinel borrows the record-wide Material.
                    // A concurrent join is about to replace that coordinate
                    // with the merged-state Material, so freeze the old
                    // author's exact signed closure into the overflow form
                    // before adding the new head.
                    existing.promote_singleton_closure()?;
                    if !existing.has_commitment(&staged_head.tx_commitment) {
                        existing.heads.push(staged_head);
                    }
                    existing.chain = change.record.chain;
                    existing.interpreted = change.record.interpreted;
                    existing
                }
                _ => change.record,
            };
            final_records.insert(change.key, folded);
        }
        let persisted: Option<BTreeMap<BodyKey, BodyRecord>> = if self.durable.is_some() {
            Some(self.persist_bundle(
                ctx,
                units,
                &staged_material,
                &final_records,
                bundle_declared,
                bundle_descriptors,
                next_frontier,
            )?)
        } else {
            None
        };
        let checkpoint_candidates: Vec<BodyKey> = final_records.keys().cloned().collect();
        for (key, record) in final_records {
            // A prepared checkpoint is anchored to the exact single-head
            // material that was current when its seed was captured. Incoming
            // incorporation can replace or join that head while retaining the
            // same local delta prefix; accepting the old job would then trim
            // material which the checkpoint never observed. Discard it before
            // publishing the incorporated record and schedule a fresh seed
            // below once Fabric and the record agree again.
            if let Ok(mut jobs) = self.checkpoint_jobs.lock() {
                jobs.remove(&key);
            }
            self.remember_replaced_heads(&key);
            let record = persisted
                .as_ref()
                .and_then(|f| f.get(&key).cloned())
                .unwrap_or(record);
            if !record.interpreted || persisted.is_none() {
                // Opaque material is always re-served from memory; a
                // NON-durable replica keeps interpreted material in memory
                // too — it has no object store to re-read from, and a Body it
                // now advertises must stay exportable. Retention is per head.
                let entries = self.raw_material.entry(key.clone()).or_default();
                for (skey, tx_id, envelope, unit) in &staged_material {
                    if skey == &key && !entries.iter().any(|(t, _, _)| t == tx_id) {
                        let transaction = units
                            .get(*unit)
                            .map(|(transaction, _)| transaction)
                            .ok_or(Failure::Illegitimate(Invalid::IncompleteMaterial))?;
                        entries.push((*tx_id, envelope.clone(), transaction.encode()));
                    }
                }
                // Drop retained entries for heads the fold replaced.
                entries.retain(|(t, _, _)| record.heads.iter().any(|h| &h.tx == t));
            } else {
                self.raw_material.remove(&key);
            }
            self.bodies.insert(key, record);
        }
        self.frontier = next_frontier;
        outcome.current = next_frontier;
        for key in checkpoint_candidates {
            self.schedule_checkpoint_if_hot(&key);
            self.release_durable_atomic_writer_image(&key);
        }
        Ok(outcome)
    }

    /// The bundle's one durable write: every staged head's envelope, every
    /// referenced signed transaction record, and the replacement Manifest over
    /// the complete post-bundle Body set — a single journal commit.
    #[allow(clippy::too_many_arguments)]
    fn persist_bundle(
        &mut self,
        ctx: &CommitContext<'_>,
        units: &[IncorporationUnit],
        staged_material: &[(BodyKey, [u8; 32], Vec<u8>, usize)],
        final_records: &BTreeMap<BodyKey, BodyRecord>,
        declared: &BTreeMap<BodyKey, Vec<[u8; 32]>>,
        descriptors: &[crate::content::ContentDescriptor],
        next_frontier: ReplicaFrontier,
    ) -> Result<BTreeMap<BodyKey, BodyRecord>, Failure> {
        // Fill object refs into a working copy of the final records: each
        // staged head gets refs to the objects written below; heads carried
        // over from the prior record keep the refs they already have.
        let mut new_records: BTreeMap<BodyKey, BodyRecord> = final_records.clone();
        let mut tx_bytes_by_unit: BTreeMap<usize, Vec<u8>> = BTreeMap::new();
        for (key, tx_id, pack, unit) in staged_material {
            let encoded = units
                .get(*unit)
                .map(|(transaction, _)| transaction.encode())
                .ok_or(Failure::Illegitimate(Invalid::IncompleteMaterial))?;
            let tx_bytes = tx_bytes_by_unit.entry(*unit).or_insert(encoded).clone();
            let transaction = units
                .get(*unit)
                .map(|(transaction, _)| transaction)
                .ok_or(Failure::Illegitimate(Invalid::IncompleteMaterial))?;
            let descriptor = transaction
                .core
                .descriptors
                .iter()
                .find(|descriptor| &descriptor.key() == key)
                .ok_or(Failure::Illegitimate(Invalid::IncompleteMaterial))?;
            let _ = decode_artifact_pack(descriptor, pack)?;
            if let Some(record) = new_records.get_mut(key) {
                if let Some(head) = record.heads.iter_mut().find(|h| &h.tx == tx_id) {
                    // Keep the signed closure explicit until `persist` has
                    // finalized the record-wide causal Material. It may be a
                    // locally re-sealed equivalent with different object
                    // hashes; `persist` compacts only an exact match.
                    head.artifacts = Some(
                        descriptor
                            .artifact_refs()
                            .copied()
                            .collect::<Vec<_>>()
                            .into_boxed_slice(),
                    );
                    head.transaction = Some(object_ref(&tx_bytes));
                    head.tx_commitment = tx_commitment(&tx_bytes);
                    head.artifact_bytes = descriptor
                        .artifact_refs()
                        .fold(0u64, |sum, object| sum.saturating_add(object.len));
                    head.tx_len = u64::try_from(tx_bytes.len()).unwrap_or(u64::MAX);
                }
            }
        }

        // Only the touched Bodies reach the durable write. The shape this
        // replaced overlaid every record into one map and re-encoded it.
        let mut new_objects: Vec<Vec<u8>> = Vec::new();
        for tx_bytes in tx_bytes_by_unit.values() {
            new_objects.push(tx_bytes.clone());
        }
        for (key, _, pack, unit) in staged_material {
            let descriptor = units
                .get(*unit)
                .and_then(|(transaction, _)| {
                    transaction
                        .core
                        .descriptors
                        .iter()
                        .find(|descriptor| &descriptor.key() == key)
                })
                .ok_or(Failure::Illegitimate(Invalid::IncompleteMaterial))?;
            new_objects.extend(decode_artifact_pack(descriptor, pack)?);
        }
        let mut staged: BTreeMap<BodyKey, Option<BodyRecord>> = new_records
            .iter()
            .map(|(k, v)| (k.clone(), Some(v.clone())))
            .collect();
        // The descriptors land in the same journal commit as the Bodies that
        // declare them. Two commits would leave a window in which this Replica
        // advertises a declaration it cannot back — which its own peers would
        // then correctly refuse.
        self.persist(
            Some(ctx),
            &mut staged,
            declared,
            descriptors,
            None,
            new_objects,
            next_frontier,
        )?;
        for (key, record) in staged {
            if let Some(record) = record {
                new_records.insert(key, record);
            }
        }

        // The caller's in-memory index must adopt THESE records — the ones
        // carrying the object refs just persisted — or the replica cannot
        // re-serve incorporated material until a reopen reloads the meta
        // (a served manifest would name Bodies its own export skips, which a
        // receiving peer correctly rejects whole as illegitimate).
        Ok(new_records)
    }

    /// Validate a completed Contact's staged material into a **sealed**
    /// [`crate::convergence::ValidatedContactBundle`] — the only input
    /// [`Replica::incorporate_bundle`] accepts. Order matters and is durable:
    ///
    /// 1. the staged records split into mechanics authority material and
    ///    canonical signed transactions;
    /// 2. the authority batch is incorporated **first** as its own durable
    ///    phase via the mechanics [`crate::convergence::AuthorityIncorporator`]
    ///    (legitimate authority advancement is independently valid Space
    ///    history and may survive a later Body failure);
    /// 3. the manifest root must be canonical, correctly signed, and its
    ///    signer authorized at its authority frontier;
    /// 4. the index must be complete, canonical, and exactly the root's;
    /// 5. every transaction must verify with signer standing at its referenced
    ///    historical frontier;
    /// 6. every received Body artifact delivery must resolve to exactly one
    ///    descriptor of a provided transaction. A delivery may omit signed
    ///    refs already retained locally, but the completed pack must exactly
    ///    back the descriptor's closure and be named by a manifest entry
    ///    binding both descriptor and transaction — **no received object
    ///    outside the verified graph is admitted**.
    ///
    /// Any failure rejects the whole staging with nothing retained (the
    /// already-durable authority receipt excepted, by design).
    pub fn validate_contact(
        &self,
        staged: &crate::convergence::StagedContactMaterial,
        authority: &dyn AuthoritySource,
        incorporator: &mut dyn crate::convergence::AuthorityIncorporator,
    ) -> Result<crate::convergence::ValidatedContactBundle, Failure> {
        // Carry the description instead of dropping it on the floor. Every
        // caller below already writes one; until now `From<String> for Invalid`
        // discarded it at the moment it became the only useful thing.
        let illegit = |reason: String| Failure::IllegitimateContact {
            kind: Invalid::Binding,
            reason,
        };
        // 1. Split the authority section.
        let mut transactions: Vec<(Transaction, Vec<u8>)> = Vec::new();
        let mut authority_material: Vec<Vec<u8>> = Vec::new();
        for record in &staged.authority_records {
            match Transaction::decode_canonical(record) {
                Ok(tx) => {
                    // A duplicated transaction id under different bytes is an
                    // equivocation attempt — reject the whole staging rather
                    // than letting one id alias two transactions.
                    if let Some((_, prior_bytes)) =
                        transactions.iter().find(|(t, _)| t.id() == tx.id())
                    {
                        if prior_bytes != record {
                            return Err(illegit(
                                "duplicate transaction id with different bytes".into(),
                            ));
                        }
                        continue;
                    }
                    transactions.push((tx, record.clone()));
                }
                Err(_) => authority_material.push(record.clone()),
            }
        }
        // 2. Authority first — an explicit durable phase with its receipt.
        let authority_receipt = incorporator
            .incorporate_authority(&authority_material)
            .map_err(|e| illegit(format!("authority batch: {e}")))?;
        // An **authority-only** staging (an unadmitted peer serving its
        // mechanics records — its admission request — with no standing to
        // advertise a Manifest): empty root bytes, and therefore no nodes, no
        // transactions, and no Body payloads. The authority phase above is the
        // whole exchange.
        if staged.manifest_root_bytes.is_empty() {
            if !staged.manifest_nodes.is_empty()
                || !staged.bodies.is_empty()
                || !transactions.is_empty()
            {
                return Err(illegit(
                    "material offered without a Manifest advertisement".into(),
                ));
            }
            return Ok(crate::convergence::ValidatedContactBundle {
                authority_receipt,
                units: Vec::new(),
                declared_content: BTreeMap::new(),
                declaration_heads: BTreeMap::new(),
                descriptors: Vec::new(),
            });
        }
        // 3. + 4. Authority-verified manifest root and its complete index.
        let root = ManifestRoot::decode_canonical(&staged.manifest_root_bytes)
            .map_err(|e| illegit(format!("manifest root: {e}")))?;
        let root_space = root.space;
        let authorized = root
            .verify_authorized(authority)
            .map_err(|e| illegit(format!("manifest root: {e}")))?;

        // The index nodes arrive as a bag of bytes; address them and let the
        // root say which ones belong. A node the sender threw in that no root
        // reaches is simply never read.
        let offered: BTreeMap<[u8; 32], Vec<u8>> = staged
            .manifest_nodes
            .iter()
            .map(|bytes| (journal::object_content_hash(bytes), bytes.clone()))
            .collect();
        struct OfferedNodes<'a>(&'a BTreeMap<[u8; 32], Vec<u8>>);
        impl crate::index::NodeSource for OfferedNodes<'_> {
            fn node(&self, hash: &[u8; 32]) -> Option<Vec<u8>> {
                self.0.get(hash).cloned()
            }
        }
        let nodes = OfferedNodes(&offered);
        authorized
            .root()
            .verify_index(&nodes)
            .map_err(|_| Failure::Illegitimate(Invalid::Index))?;

        let mut entries: BTreeMap<BodyKey, ManifestEntry> = BTreeMap::new();
        let mut entry_failure: Option<String> = None;
        crate::index::stream(&nodes, authorized.root().body_index_root, &mut |entry| {
            if entry_failure.is_some() {
                return;
            }
            match ManifestEntry::decode_canonical(&entry.value) {
                Ok(decoded) => {
                    entries.insert(decoded.key.clone(), decoded);
                }
                Err(e) => entry_failure = Some(format!("manifest entry: {e}")),
            }
        })
        .map_err(|_| Failure::Illegitimate(Invalid::Index))?;
        if let Some(reason) = entry_failure {
            tracing::warn!(%reason, "manifest entry is not canonical");
            return Err(Failure::Illegitimate(Invalid::Encoding));
        }
        // 5. Every provided transaction verifies with historical standing,
        //    bound to the root's Space.
        for (tx, _) in &transactions {
            tx.verify_authorized(authority)
                .map_err(|e| illegit(format!("transaction: {e}")))?;
            if tx.core.space != root_space {
                return Err(illegit("transaction outside the root's Space".into()));
            }
        }
        // 6. Every received Body payload resolves through the verified graph.
        type Units = BTreeMap<[u8; 32], (Transaction, Vec<(BodyKey, Vec<u8>)>)>;
        let mut units: Units = BTreeMap::new();
        for (tx_id, key, envelope) in &staged.bodies {
            if envelope.len() > MAX_BODY_BYTES {
                return Err(illegit("payload exceeds the Body maximum".into()));
            }
            let Some((tx, tx_bytes)) = transactions.iter().find(|(t, _)| &t.id() == tx_id) else {
                return Err(illegit("payload without a provided transaction".into()));
            };
            let Some(descriptor) = tx.core.descriptors.iter().find(|d| &d.key() == key) else {
                return Err(illegit("payload without a matching descriptor".into()));
            };
            let complete = self
                .complete_artifact_delivery(key, descriptor, envelope)
                .map_err(|error| illegit(format!("artifact closure: {error}")))?;
            let Some(key_entry) = entries.get(key) else {
                return Err(illegit("payload outside the advertised manifest".into()));
            };
            let bound = key_entry.heads.iter().any(|head| {
                head.descriptor_hash == descriptor_hash(descriptor)
                    && head.transaction_commitment == tx_commitment(tx_bytes)
            });
            if !bound {
                return Err(illegit(
                    "manifest entry does not bind this descriptor/transaction".into(),
                ));
            }
            units
                .entry(*tx_id)
                .or_insert_with(|| (tx.clone(), Vec::new()))
                .1
                .push((key.clone(), complete));
        }
        // 7. Root completeness: adopting the advertised root is atomic, so
        //    every entry it names must be reconstructable — either from a
        //    byte-identical local record or from the transferred material.
        //    A root naming material that is neither held nor transferred is
        //    rejected whole; no subset is adopted under it.
        // Received heads are identified by (key, transaction commitment): the
        // transferred payload's transaction bytes are in `transactions`.
        let mut received: BTreeSet<(&BodyKey, [u8; 32])> = BTreeSet::new();
        for (tx_id, key, _) in &staged.bodies {
            if let Some((_, tx_bytes)) = transactions.iter().find(|(t, _)| &t.id() == tx_id) {
                received.insert((key, tx_commitment(tx_bytes)));
            }
        }
        for (key, key_entry) in &entries {
            for entry in &key_entry.heads {
                if received.contains(&(key, entry.transaction_commitment)) {
                    continue;
                }
                let local_matches = self.bodies.get(key).is_some_and(|record| {
                    record.heads.iter().any(|h| {
                        (h.descriptor_hash == entry.descriptor_hash)
                            && (h.tx_commitment == entry.transaction_commitment)
                    })
                });
                if !local_matches {
                    return Err(Failure::Illegitimate(Invalid::IncompleteMaterial));
                }
            }
        }
        let declared_content: BTreeMap<BodyKey, Vec<[u8; 32]>> = entries
            .iter()
            .filter(|(_, entry)| !entry.content_refs.is_empty())
            .map(|(key, entry)| (key.clone(), entry.content_refs.clone()))
            .collect();

        // 8. Content completeness, the same rule as rule 7 and for the same
        //    reason: adopting the advertised root is atomic, so every content
        //    id it declares must resolve — from the advertised catalog, or from
        //    a descriptor this Replica already holds. Anything else adopts a
        //    manifest naming content nobody on this machine can ever ask for,
        //    and the gap would only surface when someone tried to open it.
        //
        //    Held-locally counts because convergence is incremental: a peer
        //    that sent us the descriptor last week is not obliged to send it
        //    again for us to accept a comment on the same issue.
        let advertised = self.advertised_content(&nodes, authorized.root())?;
        let mut descriptors = Vec::new();
        for (_key, refs) in &declared_content {
            for content in refs {
                if let Some(descriptor) = advertised.get(content) {
                    if self
                        .content_descriptor(&crate::content::ContentRef {
                            content_id: *content,
                        })
                        .is_none()
                    {
                        descriptors.push(descriptor.clone());
                    }
                    continue;
                }
                if self
                    .content_descriptor(&crate::content::ContentRef {
                        content_id: *content,
                    })
                    .is_some()
                {
                    continue;
                }
                return Err(Failure::Illegitimate(Invalid::UnbackedContent));
            }
        }
        descriptors.sort_by_key(|d| *d.content_ref().as_bytes());
        descriptors.dedup_by_key(|d| *d.content_ref().as_bytes());

        let declaration_heads = entries
            .iter()
            .filter(|(_, entry)| !entry.content_refs.is_empty())
            .map(|(key, entry)| (key.clone(), entry.heads.clone()))
            .collect();
        Ok(crate::convergence::ValidatedContactBundle {
            declared_content,
            declaration_heads,
            authority_receipt,
            units: units.into_values().collect(),
            descriptors,
        })
    }

    /// The content catalog an advertisement carries, keyed by content id.
    ///
    /// The index was already structurally verified by
    /// [`crate::manifest::ManifestRoot::verify_index`] — every entry decodes,
    /// sits under the key it hashes to, and belongs to the root's Space. This
    /// only reads it out.
    fn advertised_content(
        &self,
        nodes: &dyn crate::index::NodeSource,
        root: &ManifestRoot,
    ) -> Result<BTreeMap<[u8; 32], crate::content::ContentDescriptor>, Failure> {
        let mut catalog = BTreeMap::new();
        let mut failure: Option<String> = None;
        crate::index::stream(nodes, root.content_index_root, &mut |entry| {
            if failure.is_some() {
                return;
            }
            match crate::content::ContentDescriptor::decode_canonical(&entry.value) {
                Ok(descriptor) => {
                    catalog.insert(*descriptor.content_ref().as_bytes(), descriptor);
                }
                Err(e) => failure = Some(format!("content entry: {e}")),
            }
        })
        .map_err(|_| Failure::Illegitimate(Invalid::Index))?;
        match failure {
            Some(_) => Err(Failure::Illegitimate(Invalid::Index)),
            None => Ok(catalog),
        }
    }

    /// Incorporate a sealed validated bundle — the only Convergence entry for
    /// Contact-received material. Everything the bundle names was verified by
    /// [`Replica::validate_contact`]; per-transaction incorporation still
    /// re-verifies legitimacy (defense in depth) and enforces quotas before
    /// any byte reaches the engine.
    pub fn incorporate_bundle(
        &mut self,
        ctx: &CommitContext<'_>,
        bundle: crate::convergence::ValidatedContactBundle,
        authority: &dyn AuthoritySource,
    ) -> Result<ConvergenceOutcome, Failure> {
        self.mutation_available()?;
        self.incorporate_units(
            ctx,
            &bundle.units,
            &bundle.declared_content,
            &bundle.declaration_heads,
            &bundle.descriptors,
            authority,
        )
    }

    /// Build and sign the current Manifest over the full Body set **and the
    /// content catalog those Bodies reach**, returning the root plus every
    /// index node a peer needs to verify it — the advertisement a Contact
    /// serves. Deterministic for a given state and signer.
    ///
    /// Both halves travel or neither is any use. A Body's manifest entry names
    /// content ids; a content id alone is a name for bytes nobody but the
    /// author can ask for, because asking requires the geometry, the epoch, and
    /// the Merkle root that only the descriptor carries. Advertising the first
    /// without the second converges an attachment as an opaque id.
    ///
    /// Only *reachable* descriptors are advertised: those some live Body
    /// declares. A descriptor no live Body declares is this Station's own
    /// business, and pushing it at a peer would grow their catalog from ours.
    ///
    /// Deliberately **narrower** than what the sweep keeps. The sweep also
    /// spares content under a live pending-declaration hold — an upload whose
    /// Body has not been committed yet — and that content is exactly what a
    /// peer has no use for: no Body names it, so nothing on their side would
    /// ever reference it, and their own sweep would have to undo the adoption.
    /// "May I delete this" and "may I show this to a peer" are two questions,
    /// and [`Self::retained_content`] answers the first.
    ///
    /// Shipping the whole index is what F4 replaces: two peers will compare
    /// roots, descend only where subtree hashes differ, and exchange divergent
    /// leaves. Until then this costs what a full walk costs, and is correct.
    pub fn export_manifest(
        &self,
        ctx: &CommitContext<'_>,
    ) -> Result<(Vec<u8>, Vec<Vec<u8>>), Failure> {
        use crate::index::NodeSink;

        let entries: Vec<ManifestEntry> = self
            .bodies
            .iter()
            .map(|(key, record)| {
                Self::manifest_entry(
                    key,
                    record,
                    self.declared_content.get(key).cloned().unwrap_or_default(),
                )
            })
            .collect::<Result<_, _>>()?;
        let mut sink = NodeSink::default();
        let body_root = crate::manifest::build_body_index(entries, &mut sink)
            .map_err(|_| Failure::Illegitimate(Invalid::Index))?;
        let content_root =
            crate::manifest::build_content_index(self.advertised_descriptors()?, &mut sink)
                .map_err(|_| Failure::Illegitimate(Invalid::Index))?;
        let root = ManifestRoot::sign_with(
            ctx.space,
            self.frontier,
            body_root,
            content_root,
            ctx.authority_frontier.clone(),
            ctx.signer,
        )
        .ok_or_else(|| Failure::Illegitimate("sign manifest root".into()))?;
        Ok((root.encode(), sink.written))
    }

    /// The descriptors an advertisement carries: every committed descriptor
    /// some live Body declares.
    ///
    /// Derived from the Body set on every export rather than maintained,
    /// because that is the same rule the sweep applies — a maintained set and a
    /// derived one would eventually disagree, and the disagreement would be
    /// invisible until a peer could not open an attachment.
    fn advertised_descriptors(&self) -> Result<Vec<crate::content::ContentDescriptor>, Failure> {
        let Some(store) = self.durable.as_ref() else {
            return Ok(Vec::new());
        };
        let reachable = self.reachable_content();
        if reachable.is_empty() {
            return Ok(Vec::new());
        }
        let mut descriptors = Vec::with_capacity(reachable.len());
        let mut failure: Option<String> = None;
        crate::index::stream(&StoreNodes(store), self.content_index_root, &mut |entry| {
            if failure.is_some() {
                return;
            }
            match crate::content::ContentDescriptor::decode_canonical(&entry.value) {
                Ok(descriptor) => {
                    if reachable.contains(descriptor.content_ref().as_bytes()) {
                        descriptors.push(descriptor);
                    }
                }
                Err(e) => failure = Some(format!("content entry: {e}")),
            }
        })
        .map_err(|_| Failure::Integrity(Defect::Index))?;
        match failure {
            Some(_) => Err(Failure::Integrity(Defect::Index)),
            None => Ok(descriptors),
        }
    }

    /// The root of this Replica's published catalog — what a peer compares
    /// against to discover divergence without either side enumerating.
    ///
    /// This replaces the holdings declaration as the thing a Contact opens
    /// with. A declaration had to name every head, and the frame carrying it
    /// was sized for exactly that; a root is 40 bytes whatever the catalog
    /// holds, and equal roots prove equal catalogs outright because the
    /// encoding is canonical.
    pub fn published_root(&self) -> Option<([u8; 32], u64)> {
        self.manifest_body_root.map(|root| (root.hash, root.count))
    }

    /// The signed manifest root this Replica is currently advertising.
    pub fn published_manifest_root(&self) -> Option<ManifestRoot> {
        let reference = self.manifest_root_object?;
        let bytes = self.durable.as_ref()?.read_object(&reference).ok()?;
        ManifestRoot::decode_canonical(&bytes).ok()
    }

    /// How many objects the store has promised to keep.
    ///
    /// A maintenance and test observation, not a product surface. It exists
    /// because the required set is the one thing whose growth has no natural
    /// ceiling: an entry is a promise that never expires, so anything admitted
    /// there by mistake is permanent.
    pub fn required_object_count(&self) -> Option<usize> {
        Some(self.durable.as_ref()?.required_objects().ok()?.len())
    }

    /// How many objects the store physically holds — the growth observation
    /// beside the promise above: after a sweep it is the live population,
    /// between sweeps it includes what commits superseded.
    pub fn stored_object_count(&self) -> Option<usize> {
        self.durable.as_ref()?.stored_objects().ok()
    }

    /// Collect objects no root reaches. A maintenance beat, safe at any quiet
    /// moment — the store sweeps periodically on its own, and this lets a
    /// caller that knows it is idle pre-empt that.
    pub fn collect_unreachable_objects(&self) -> Result<(), Failure> {
        let Some(store) = self.durable.as_ref() else {
            return Ok(());
        };
        store.collect_unreachable().map_err(Failure::Durability)
    }

    /// Serve the requested published-catalog nodes to a descending peer.
    ///
    /// Bounded by the caller and by what exists: an unknown hash is simply
    /// absent from the answer, so a peer cannot use requests to probe for
    /// anything it could not already address.
    pub(crate) fn published_nodes(
        &self,
        hashes: &[[u8; 32]],
        max: usize,
    ) -> BTreeMap<[u8; 32], Vec<u8>> {
        use crate::index::NodeSource;
        let Some(store) = self.durable.as_ref() else {
            return BTreeMap::new();
        };
        let nodes = StoreNodes(store);
        hashes
            .iter()
            .take(max)
            .filter_map(|hash| nodes.node(hash).map(|bytes| (*hash, bytes)))
            .collect()
    }

    /// Begin discovering what a peer holds that this Replica does not.
    ///
    /// The session is a pure state machine: it asks for node hashes, is fed
    /// the answers, and eventually names the Bodies whose advertised heads
    /// differ. Cost is proportional to the disagreement, not to either
    /// catalog — two converged peers exchange nothing at all.
    pub(crate) fn begin_reconciliation(
        &self,
        peer_root: Option<crate::index::ChildRef>,
        max_nodes: u64,
    ) -> crate::index::Reconciliation {
        crate::index::Reconciliation::begin(self.manifest_body_root, peer_root, max_nodes)
    }

    /// Feed a reconciliation the nodes it asked for, against this Replica's own
    /// catalog.
    pub(crate) fn absorb_reconciliation(
        &self,
        session: &mut crate::index::Reconciliation,
        nodes: &BTreeMap<[u8; 32], Vec<u8>>,
    ) -> Result<(), Failure> {
        let Some(store) = self.durable.as_ref() else {
            return Err(Failure::Illegitimate(
                "a non-durable Replica has no catalog to reconcile against".into(),
            ));
        };
        session
            .absorb(&StoreNodes(store), nodes)
            .map_err(|_| Failure::Integrity(Defect::Index))
    }

    /// Decode the divergent entries a completed reconciliation produced into
    /// the Bodies and heads this Replica should ask for.
    pub(crate) fn divergent_heads(
        entries: &[crate::index::IndexEntry],
    ) -> Result<Vec<DivergentBody>, Failure> {
        entries
            .iter()
            .map(|entry| {
                let published = ManifestEntry::decode_canonical(&entry.value)
                    .map_err(|_| Failure::Illegitimate(Invalid::Encoding))?;
                let commitments = published
                    .heads
                    .iter()
                    .map(|h| h.transaction_commitment)
                    .collect();
                Ok((published.key, commitments))
            })
            .collect()
    }

    /// The complete per-head holdings summary: every `(key, transaction
    /// commitment)` pair this Replica's index carries — exactly the vocabulary
    /// the Manifest advertises (one entry per constituent head), so a peer can
    /// declare "I already hold these" and be served only the difference. The
    /// summary commits to nothing on its own: a receiver's adoption is still
    /// judged by the full completeness rule, so a wrong or lying summary can
    /// only cost bandwidth or starve the claimant, never corrupt state.
    pub fn head_commitments(&self) -> Vec<(BodyKey, [u8; 32])> {
        let mut out = Vec::new();
        for (key, record) in self.bodies.iter() {
            // OPAQUE heads are deliberately NOT declared: upgrading retained
            // material to interpreted happens through the incorporation path
            // (re-receipt once the schema and key epoch are available), so a
            // declaration that suppressed re-transfer would freeze a joiner's
            // pre-admission material opaque forever.
            if !record.interpreted {
                continue;
            }
            for head in &record.heads {
                out.push((key.clone(), head.tx_commitment));
            }
        }
        out
    }

    fn remember_replaced_heads(&mut self, key: &BodyKey) {
        let Some(record) = self.bodies.get(key) else {
            return;
        };
        let heads: Vec<([u8; 32], Vec<Object>)> = record
            .heads
            .iter()
            .map(|head| {
                (
                    head.tx_commitment,
                    record.artifacts(head).map(artifact_object).collect(),
                )
            })
            .collect();
        for (commitment, artifacts) in heads {
            let cache_key = (key.clone(), commitment);
            if self.recent_head_artifacts.contains_key(&cache_key) {
                continue;
            }
            self.recent_head_artifacts
                .insert(cache_key.clone(), artifacts);
            self.recent_head_order.push_back(cache_key);
        }
        while self.recent_head_order.len() > RECENT_HEAD_ARTIFACTS {
            if let Some(expired) = self.recent_head_order.pop_front() {
                self.recent_head_artifacts.remove(&expired);
            }
        }
    }

    /// Complete a bandwidth-minimized delivery pack from content-addressed
    /// artifacts this Replica already retains. The signed descriptor remains
    /// the complete authority; omission is accepted only when every missing
    /// ref resolves locally and the reconstructed strict pack validates.
    fn complete_artifact_delivery(
        &self,
        key: &BodyKey,
        descriptor: &Descriptor,
        delivery: &[u8],
    ) -> Result<Vec<u8>, Failure> {
        let delivered = decode_artifact_delivery_pack(descriptor, delivery)?;
        let mut available: BTreeMap<([u8; 32], u64), Vec<u8>> = delivered
            .into_iter()
            .map(|artifact| {
                let reference = object_ref(&artifact);
                ((reference.hash, reference.len), artifact)
            })
            .collect();

        // A non-durable Replica retains strict packs for each current head.
        // Index those objects once before resolving the closure.
        if let Some(entries) = self.raw_material.get(key) {
            for (_, pack, tx_bytes) in entries {
                let Ok(tx) = Transaction::decode_canonical(tx_bytes) else {
                    continue;
                };
                let Some(local_descriptor) = tx
                    .core
                    .descriptors
                    .iter()
                    .find(|candidate| candidate.key() == *key)
                else {
                    continue;
                };
                let Ok(artifacts) = decode_artifact_pack(local_descriptor, pack) else {
                    continue;
                };
                for artifact in artifacts {
                    let reference = object_ref(&artifact);
                    available
                        .entry((reference.hash, reference.len))
                        .or_insert(artifact);
                }
            }
        }

        let mut complete = Vec::with_capacity(descriptor.artifact_refs().count());
        for reference in descriptor.artifact_refs() {
            let coordinate = (reference.hash, reference.len);
            if let Some(artifact) = available.remove(&coordinate) {
                complete.push(artifact);
                continue;
            }
            let Some(store) = &self.durable else {
                return Err(Failure::Illegitimate(
                    "artifact closure is incomplete".into(),
                ));
            };
            let artifact = store
                .read_object(&Object {
                    hash: reference.hash,
                    len: reference.len,
                })
                .map_err(|_| Failure::Illegitimate("artifact closure is incomplete".into()))?;
            complete.push(artifact);
        }
        let pack = encode_artifact_pack(&complete)?;
        decode_artifact_pack(descriptor, &pack)?;
        Ok(pack)
    }

    /// Export this Replica's current material for a peer: for each Body, its
    /// **retained** signed transaction record and protected artifact pack —
    /// byte-identical to what was committed or incorporated, grouped by
    /// transaction. Opaque Bodies forward their retained bytes unchanged.
    pub fn export_material(&self) -> Result<ExportedMaterial, Failure> {
        self.export_material_excluding(&std::collections::BTreeSet::new())
    }

    /// [`Replica::export_material`], omitting every head whose `(key,
    /// transaction commitment)` the peer DECLARED it already holds — the
    /// O(changed) Contact transfer. The advertised Manifest still names every
    /// head; the receiver's root-completeness validation reconstructs omitted
    /// entries from its own byte-equivalent local material (the same rule that
    /// admits any partial transfer), so omission is pure bandwidth: a stale or
    /// false declaration fails the CLAIMANT's adoption, nothing else.
    pub fn export_material_excluding(
        &self,
        held: &std::collections::BTreeSet<(BodyKey, [u8; 32])>,
    ) -> Result<ExportedMaterial, Failure> {
        type Grouped = BTreeMap<[u8; 32], (Transaction, Vec<(BodyKey, Vec<u8>)>)>;
        let mut by_tx: Grouped = BTreeMap::new();
        for (key, record) in self.bodies.iter() {
            // A Body the manifest advertises MUST be fully exportable — every
            // constituent head, from the retained in-memory material or the
            // durable object store. A gap here would let this replica serve a
            // root that names material it cannot supply, which every peer
            // correctly rejects whole.
            for head in &record.heads {
                if held.contains(&(key.clone(), head.tx_commitment)) {
                    continue;
                }
                let raw = self
                    .raw_material
                    .get(key)
                    .and_then(|entries| entries.iter().find(|(t, _, _)| t == &head.tx));
                let (pack, tx_bytes) = match (raw, &self.durable) {
                    (Some((_, pack, tx_bytes)), _) => (pack.clone(), tx_bytes.clone()),
                    (None, Some(store)) => {
                        let Some(tx_ref) = head.transaction else {
                            return Err(Failure::Integrity(Defect::MissingMaterial));
                        };
                        let refs = record.artifacts(head);
                        let mut artifacts = Vec::with_capacity(refs.len());
                        for reference in refs {
                            artifacts.push(
                                store
                                    .read_object(&artifact_object(reference))
                                    .map_err(|_| Failure::Integrity(Defect::Encoding))?,
                            );
                        }
                        let pack = encode_artifact_pack(&artifacts)?;
                        let tx_bytes = store
                            .read_object(&tx_ref)
                            .map_err(|_| Failure::Integrity(Defect::Encoding))?;
                        (pack, tx_bytes)
                    }
                    (None, None) => {
                        return Err(Failure::Integrity(Defect::MissingMaterial));
                    }
                };
                let tx = Transaction::decode_canonical(&tx_bytes)
                    .map_err(|_| Failure::Integrity(Defect::Encoding))?;
                let descriptor = tx
                    .core
                    .descriptors
                    .iter()
                    .find(|descriptor| descriptor.key() == *key)
                    .ok_or(Failure::Integrity(Defect::MissingMaterial))?;
                let artifacts = decode_artifact_pack(descriptor, &pack)
                    .map_err(|_| Failure::Integrity(Defect::CorruptMaterial))?;
                let mut known = BTreeSet::new();
                for (_, commitment) in
                    held.range((key.clone(), [u8::MIN; 32])..=(key.clone(), [u8::MAX; 32]))
                {
                    if let Some(references) =
                        self.recent_head_artifacts.get(&(key.clone(), *commitment))
                    {
                        known.extend(
                            references
                                .iter()
                                .map(|reference| (reference.hash, reference.len)),
                        );
                    }
                }
                let delivery = if known.is_empty() {
                    pack
                } else {
                    let missing: Vec<Vec<u8>> = artifacts
                        .into_iter()
                        .filter(|artifact| {
                            let reference = object_ref(artifact);
                            !known.contains(&(reference.hash, reference.len))
                        })
                        .collect();
                    encode_artifact_pack(&missing)?
                };
                let entry = by_tx.entry(head.tx).or_insert_with(|| (tx, Vec::new()));
                entry.1.push((key.clone(), delivery));
            }
        }
        Ok(by_tx.into_values().collect())
    }

    /// Apply staged ops to the engine, translating and validating each.
    fn prepare_ops(
        &mut self,
        request_label: &str,
        ops: &[(BodyKey, Op)],
    ) -> Result<fabric::Prepared, Failure> {
        // Durable collaborative Bodies are cold causal closures until touched.
        // Inflate only the distinct existing collaborative writers this batch
        // names; Fabric's 64-entry LRU keeps them hot and drops evicted exports
        // because the Journal remains their authoritative cold image.
        let mut needed = BTreeMap::<BodyKey, Arc<CausalMaterial>>::new();
        for (key, _) in ops {
            let Some(record) = self.bodies.get(key) else {
                continue;
            };
            if !record.interpreted || record.binding.mutation_model != MUTATION_COLLABORATIVE {
                continue;
            }
            // Scratch/non-durable Replicas have no cold object source. Their
            // writer is authoritative and retained for the Replica lifetime;
            // after a concurrent peer merge its version can legitimately be
            // newer than any one signed head Material. Never try to "inflate"
            // that exact hot merged state through a nonexistent Journal.
            if self.durable.is_none()
                && lock_fabric(&self.fabric)
                    .body_snapshot(&fabric_key(key))
                    .ok()
                    .flatten()
                    .is_some()
            {
                continue;
            }
            if matches!(
                lock_fabric(&self.fabric).version(&fabric_key(key)),
                Ok(version) if record.causal.as_ref().is_some_and(|material| material.version == version)
            ) {
                continue;
            }
            let material = record
                .causal
                .clone()
                .ok_or(Failure::Integrity(Defect::MissingMaterial))?;
            needed.entry(key.clone()).or_insert(material);
        }
        for (key, material) in needed {
            let snapshot = self.body_from_causal_material(&key, &material)?;
            if snapshot.read_shared().is_some()
                || snapshot.version().ok() != Some(material.version.clone())
            {
                return Err(Failure::Integrity(Defect::CorruptMaterial));
            }
            let status = lock_fabric(&self.fabric)
                .import_verified_snapshot(&fabric_key(&key), &snapshot)
                .map_err(|_| Failure::Integrity(Defect::CorruptMaterial))?;
            if status.pending {
                return Err(Failure::Integrity(Defect::CorruptMaterial));
            }
        }
        let mut fabric_ops = Vec::with_capacity(ops.len());
        for (key, op) in ops {
            fabric_ops.push(translate(fabric_key(key), op)?);
        }
        match lock_fabric(&self.fabric).prepare(fabric::Transaction::new(request_label, fabric_ops))
        {
            Ok(prepared) => Ok(prepared),
            Err(EngineFailure::Unsupported) => Err(Failure::UnsupportedOp),
            Err(EngineFailure::TypeConflict) => Err(Failure::TypeConflict),
            Err(EngineFailure::Invalid(invalid)) => Err(Failure::InvalidOp(invalid)),
            Err(EngineFailure::OutcomeUnknown) => {
                self.poisoned = true;
                Err(Failure::OutcomeUnknown)
            }
            Err(EngineFailure::Journal(journal::Failure::Integrity(defect))) => {
                self.poisoned = true;
                Err(Failure::Integrity(Defect::Store(defect)))
            }
            Err(EngineFailure::Journal(failure)) => {
                self.poisoned = true;
                Err(Failure::Durability(failure))
            }
        }
    }

    /// Apply staged ops to the engine, translating and validating each.
    fn apply_ops(
        &mut self,
        request_label: &str,
        ops: &[(BodyKey, Op)],
    ) -> Result<fabric::Receipt, Failure> {
        let prepared = self.prepare_ops(request_label, ops)?;
        Ok(lock_fabric(&self.fabric).finalize(prepared))
    }

    /// Track records for an unattributed (non-durable) commit so bindings and
    /// reads stay consistent in tests.
    fn update_records_unattributed(&mut self, ops: &[(BodyKey, Op)]) -> Result<(), Failure> {
        let seed = mint_chain_seed()?;
        let mut tx = [0u8; 32];
        tx.get_mut(..16)
            .ok_or(Failure::Integrity(Defect::Encoding))?
            .copy_from_slice(&seed);
        let mut touched: Vec<BodyKey> = ops.iter().map(|(k, _)| k.clone()).collect();
        touched.sort();
        touched.dedup();
        for key in touched {
            match lock_fabric(&self.fabric).export_body(&fabric_key(&key)) {
                None => {
                    self.bodies.remove(&key);
                }
                Some(export) => {
                    let base = self
                        .bodies
                        .get(&key)
                        .map(|r| r.chain)
                        .unwrap_or(ReplicaFrontier::EMPTY);
                    let model = match export {
                        BodyExport::Atomic(_) => MUTATION_ATOMIC,
                        BodyExport::Collaborative(_) => MUTATION_COLLABORATIVE,
                    };
                    let record = BodyRecord {
                        binding: self.bodies.get(&key).map(|r| r.binding.clone()).unwrap_or(
                            BodyBinding {
                                schema: SchemaId::parse("unattributed")
                                    .ok_or(Failure::Integrity(Defect::Encoding))?,
                                schema_version: 1,
                                encoding: EncodingId::parse("bytes")
                                    .ok_or(Failure::Integrity(Defect::Encoding))?,
                                mutation_model: model,
                            },
                        ),
                        chain: advance_chain(base, &seed),
                        heads: smallvec::smallvec![BodyHead {
                            tx,
                            descriptor_hash: [0u8; 32],
                            tx_commitment: [0u8; 32],
                            artifacts: Some(Vec::new().into_boxed_slice()),
                            transaction: None,
                            artifact_bytes: 0,
                            tx_len: 0,
                        }],
                        causal: self
                            .bodies
                            .get(&key)
                            .and_then(|record| record.causal.clone()),
                        interpreted: true,
                    };
                    self.bodies.insert(key, record);
                }
            }
        }
        Ok(())
    }

    fn populate_local_record_refs<T>(
        tx: &Transaction,
        sealed: &[(BodyKey, Vec<u8>, T)],
        new_records: &mut BTreeMap<BodyKey, Option<BodyRecord>>,
        durable: bool,
    ) -> Result<(), Failure> {
        let tx_bytes = tx.encode();
        let tx_ref = object_ref(&tx_bytes);
        let commitment = tx_commitment(&tx_bytes);
        for (key, pack, _) in sealed {
            let descriptor = tx
                .core
                .descriptors
                .iter()
                .find(|descriptor| &descriptor.key() == key)
                .ok_or(Failure::Integrity(Defect::MissingMaterial))?;
            let _ = decode_artifact_delivery_pack(descriptor, pack)?;
            let record = new_records
                .get_mut(key)
                .and_then(Option::as_mut)
                .ok_or(Failure::Integrity(Defect::MissingMaterial))?;
            let head = record.head_mut()?;
            head.artifacts = None;
            head.transaction = durable.then_some(tx_ref);
            head.tx_commitment = commitment;
            head.artifact_bytes = descriptor
                .artifact_refs()
                .fold(0u64, |sum, object| sum.saturating_add(object.len));
            head.tx_len = u64::try_from(tx_bytes.len()).unwrap_or(u64::MAX);
            head.descriptor_hash = descriptor_hash(descriptor);
        }
        Ok(())
    }

    /// Derive the exact signed read coordinate without writing any object or
    /// mutating any catalog. The persistent index touches only changed paths;
    /// its emitted nodes are deliberately discarded until `persist` rebuilds
    /// the identical paths inside the journal transaction.
    fn preview_manifest_root(
        &self,
        ctx: &CommitContext<'_>,
        changed: &BTreeMap<BodyKey, Option<BodyRecord>>,
        declared: &BTreeMap<BodyKey, Vec<[u8; 32]>>,
        next_frontier: ReplicaFrontier,
    ) -> Result<[u8; 32], Failure> {
        use crate::index::{self, IndexChange, NodeSink};

        let Some(store) = self.durable.as_ref() else {
            return Ok(next_frontier.root);
        };
        let mut changes = Vec::with_capacity(changed.len());
        for (key, record) in changed {
            let refs = declared
                .get(key)
                .cloned()
                .unwrap_or_else(|| self.declared_content.get(key).cloned().unwrap_or_default());
            let value = record
                .as_ref()
                .map(|record| Self::manifest_entry(key, record, refs).map(|entry| entry.encode()))
                .transpose()?;
            changes.push(IndexChange {
                key: body_index_key(key),
                value,
            });
        }
        let mut sink = NodeSink::default();
        let body_root = index::apply(
            &StoreNodes(store),
            self.manifest_body_root,
            changes,
            &mut sink,
        )
        .map_err(|_| Failure::Integrity(Defect::Index))?;
        let root = ManifestRoot::sign_with(
            ctx.space,
            next_frontier,
            body_root,
            self.content_index_root,
            ctx.authority_frontier.clone(),
            ctx.signer,
        )
        .ok_or_else(|| Failure::Illegitimate("sign candidate manifest root".into()))?;
        Ok(object_ref(&root.encode()).hash)
    }

    fn finalize_prepared_action(
        &mut self,
        ctx: &CommitContext<'_>,
        state: PreparedActionState,
    ) -> Result<RequestReceipt, Failure> {
        let (fabric, mut data) = match state {
            PreparedActionState::Mutation { fabric, data } => (fabric, data),
            PreparedActionState::Noop { receipt } => {
                if self.durable.is_some() {
                    self.persist_receipt_only(&receipt)?;
                } else {
                    self.receipts
                        .insert(receipt.scope_key(), (receipt.clone(), None));
                }
                return Ok(receipt);
            }
        };

        if ctx.space != &data.manifest_space
            || ctx.authority_frontier != data.manifest_authority_frontier
            || ctx.signer.signer_key() != data.manifest_signer
        {
            let _ = lock_fabric(&self.fabric).rollback(fabric);
            return Err(Failure::Illegitimate(
                "prepared action finalized with different publication authority".into(),
            ));
        }
        match &self.space {
            Some(space) if space != ctx.space => {
                let _ = lock_fabric(&self.fabric).rollback(fabric);
                return Err(Failure::Illegitimate(
                    "commit addressed to a different Space".into(),
                ));
            }
            None => self.space = Some(ctx.space.clone()),
            Some(_) => {}
        }

        let persisted = if let Some(tx) = &data.transaction {
            if self.durable.is_some() {
                self.persist_transaction(
                    ctx,
                    tx,
                    &data.sealed,
                    &mut data.new_records,
                    Some(data.receipt.clone()),
                    data.next_frontier,
                    &data.declared,
                )
                .map(|_| ())
            } else {
                let tx_bytes = tx.encode();
                for (key, pack, _) in &data.sealed {
                    let descriptor = tx
                        .core
                        .descriptors
                        .iter()
                        .find(|descriptor| descriptor.key() == *key)
                        .ok_or(Failure::Integrity(Defect::MissingMaterial))?;
                    let pack = self.complete_artifact_delivery(key, descriptor, pack)?;
                    self.raw_material
                        .insert(key.clone(), vec![(tx.id(), pack, tx_bytes.clone())]);
                }
                Ok(())
            }
        } else {
            Ok(())
        };
        if let Err(error) = persisted {
            if lock_fabric(&self.fabric).rollback(fabric).is_err() {
                self.poisoned = true;
                return Err(Failure::OutcomeUnknown);
            }
            return Err(error);
        }

        let checkpoint_candidates: Vec<BodyKey> = data
            .new_records
            .iter()
            .filter_map(|(key, record)| record.as_ref().map(|_| key.clone()))
            .collect();
        let durable = self.durable.is_some();
        for (key, record) in data.new_records {
            self.remember_replaced_heads(&key);
            match record {
                None => {
                    self.bodies.remove(&key);
                    self.raw_material.remove(&key);
                }
                Some(record) => {
                    if durable {
                        self.raw_material.remove(&key);
                    }
                    self.bodies.insert(key, record);
                }
            }
        }
        self.frontier = data.next_frontier;
        if !durable {
            self.receipts
                .insert(data.receipt.scope_key(), (data.receipt.clone(), None));
        }
        let _ = lock_fabric(&self.fabric).finalize(fabric);
        for key in checkpoint_candidates {
            self.schedule_checkpoint_if_hot(&key);
            self.release_durable_atomic_writer_image(&key);
        }

        let actual_root = {
            let root = self.manifest_root();
            if root == crate::transaction::NO_PARENT_ROOT {
                self.frontier.root
            } else {
                root
            }
        };
        if actual_root != data.candidate_root {
            self.poisoned = true;
            tracing::error!(
                candidate = ?data.candidate_root,
                actual = ?actual_root,
                "prepared manifest preview disagreed with durable publication"
            );
            return Err(Failure::OutcomeUnknown);
        }
        Ok(data.receipt)
    }

    /// Persist a local signed transaction: the transaction record, sealed
    /// payloads, receipt, and manifest, at one journal linearization point.
    /// Returns the durable receipt.
    #[allow(clippy::too_many_arguments)]
    fn persist_transaction(
        &mut self,
        ctx: &CommitContext<'_>,
        tx: &Transaction,
        sealed: &[(BodyKey, Vec<u8>, CausalMaterial)],
        new_records: &mut BTreeMap<BodyKey, Option<BodyRecord>>,
        receipt: Option<RequestReceipt>,
        next_frontier: ReplicaFrontier,
        declared: &BTreeMap<BodyKey, Vec<[u8; 32]>>,
    ) -> Result<RequestReceipt, Failure> {
        let sealed: Vec<(BodyKey, Vec<u8>, ())> = sealed
            .iter()
            .map(|(k, e, _)| (k.clone(), e.clone(), ()))
            .collect();
        let receipt = receipt.ok_or(Failure::Integrity(Defect::MissingMaterial))?;
        self.persist_graph(
            ctx,
            tx,
            &sealed,
            new_records,
            Some(&receipt),
            next_frontier,
            declared,
        )?;
        Ok(receipt)
    }

    /// Persist ONLY a new idempotency receipt. No Body changed and no manifest
    /// is republished, so this writes the receipt, one index path, and the
    /// commit point — and nothing else.
    fn persist_receipt_only(&mut self, receipt: &RequestReceipt) -> Result<(), Failure> {
        let frontier = self.frontier;
        let mut changed = BTreeMap::new();
        self.persist(
            None,
            &mut changed,
            &BTreeMap::new(),
            &[],
            Some(receipt),
            Vec::new(),
            frontier,
        )?;
        let bytes = receipt.encode();
        self.receipt_cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(receipt.scope_key(), receipt.clone(), object_ref(&bytes));
        Ok(())
    }

    /// The one durable-write path: assemble the canonical object graph and run
    /// the journal protocol. Every failure before the manifest linearization
    /// point poisons this handle (the engine has already applied in memory);
    /// `OutcomeUnknown` demands reopen-not-retry.
    #[allow(clippy::too_many_arguments)]
    fn persist_graph(
        &mut self,
        ctx: &CommitContext<'_>,
        tx: &Transaction,
        sealed: &[(BodyKey, Vec<u8>, ())],
        new_records: &mut BTreeMap<BodyKey, Option<BodyRecord>>,
        receipt: Option<&RequestReceipt>,
        next_frontier: ReplicaFrontier,
        declared: &BTreeMap<BodyKey, Vec<[u8; 32]>>,
    ) -> Result<(), Failure> {
        let tx_bytes = tx.encode();
        let tx_ref = object_ref(&tx_bytes);
        Self::populate_local_record_refs(tx, sealed, new_records, true)?;

        // The transaction object is written only if a head names it. A batch
        // whose every touched Body is a tombstone leaves no head at all, and an
        // object nothing references enters the required set with a refcount of
        // zero — permanent, because the only thing that could release it is a
        // reference that was never taken.
        let referenced = new_records.values().flatten().any(|record| {
            record
                .heads
                .iter()
                .any(|head| head.transaction == Some(tx_ref))
        });
        let mut new_objects: Vec<Vec<u8>> = Vec::new();
        if referenced {
            new_objects.push(tx_bytes);
        }
        for (key, pack, _) in sealed {
            let descriptor = tx
                .core
                .descriptors
                .iter()
                .find(|descriptor| &descriptor.key() == key)
                .ok_or(Failure::Integrity(Defect::MissingMaterial))?;
            new_objects.extend(decode_artifact_delivery_pack(descriptor, pack)?);
        }
        self.persist(
            Some(ctx),
            new_records,
            declared,
            &[],
            receipt,
            new_objects,
            next_frontier,
        )?;

        // Durable receipt refs become authoritative in memory.
        if let Some(receipt) = receipt {
            let bytes = &receipt.encode();
            self.receipt_cache
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(receipt.scope_key(), receipt.clone(), object_ref(bytes));
        }
        Ok(())
    }

    /// Lossy compatibility read of one Atomic Body.
    ///
    /// This may perform blocking protected-object I/O for a durable cold
    /// image and intentionally collapses every typed resolver failure to
    /// `None`. Exact Runtime paths use `ReadSnapshot::body_presence`, reserve
    /// `body_image_bounds`, and call `resolve_body_image` outside station/core
    /// locks instead.
    pub fn read(&self, key: &BodyKey) -> Option<Vec<u8>> {
        if !self.prepared_in_flight.load(Ordering::Acquire) {
            if let Some(bytes) = lock_fabric(&self.fabric).read(&fabric_key(key)) {
                return Some(bytes);
            }
        }
        let record = self.bodies.get(key)?;
        if !record.interpreted {
            return None;
        }
        if !matches!(
            record.binding.mutation_model,
            MUTATION_ATOMIC | MUTATION_IMMUTABLE_ATOMIC
        ) {
            return None;
        }
        let material = record.causal.as_deref()?;
        let store = self.durable.as_ref()?;
        let keys = self.keys.as_ref()?;
        let resolver = pin_body_image_resolver(store.reader(), keys, None, [material]).ok()?;
        resolver
            .resolve(key, &record.binding, material)
            .ok()?
            .read()
    }

    #[cfg(test)]
    pub(crate) fn atomic_writer_image_loaded_for_test(&self, key: &BodyKey) -> bool {
        lock_fabric(&self.fabric).read(&fabric_key(key)).is_some()
    }

    #[cfg(test)]
    pub(crate) fn collaborative_writer_image_loaded_for_test(&self, key: &BodyKey) -> bool {
        lock_fabric(&self.fabric)
            .body_snapshot(&fabric_key(key))
            .ok()
            .flatten()
            .is_some()
    }

    #[cfg(test)]
    pub(crate) fn writer_body_count_for_test(&self) -> u64 {
        lock_fabric(&self.fabric).body_count()
    }

    #[cfg(test)]
    pub(crate) fn advance_parent_for_test(&mut self) {
        self.frontier = advance(self.frontier, b"intervening-authoritative-truth");
    }

    #[cfg(test)]
    pub(crate) fn collect_unreachable_for_test(&self) -> Result<(), Failure> {
        self.durable
            .as_ref()
            .ok_or(Failure::Poisoned)?
            .collect_unreachable()
            .map_err(Failure::Durability)
    }

    /// Project the committed collaborative view of a Body. List elements carry
    /// the stable ids `ListRemove`/`ListMove` take.
    ///
    /// A Body whose collaborative types this build does not implement returns
    /// `SchemaAhead` rather than a partial view — the material is still stored,
    /// forwarded, and converged, because byte-completeness does not require
    /// comprehension.
    pub fn read_collaborative(
        &self,
        key: &BodyKey,
    ) -> Result<fabric::CollaborativeView, fabric::projection::Failure> {
        if !self.prepared_in_flight.load(Ordering::Acquire) {
            if let Ok(view) = lock_fabric(&self.fabric).read_collaborative(&fabric_key(key)) {
                return Ok(view);
            }
        }
        let record = self
            .bodies
            .get(key)
            .filter(|record| {
                record.interpreted
                    && record.binding.mutation_model == MUTATION_COLLABORATIVE
                    && record.causal.is_some()
            })
            .ok_or(fabric::projection::Failure::NotCollaborative)?;
        let material = record
            .causal
            .as_deref()
            .ok_or(fabric::projection::Failure::NotCollaborative)?;
        let store = self
            .durable
            .as_ref()
            .ok_or(fabric::projection::Failure::NotCollaborative)?;
        let keys = self
            .keys
            .as_ref()
            .ok_or(fabric::projection::Failure::NotCollaborative)?;
        pin_body_image_resolver(store.reader(), keys, None, [material])
            .map_err(|_| fabric::projection::Failure::NotCollaborative)?
            .resolve(key, &record.binding, material)
            .map_err(|_| fabric::projection::Failure::NotCollaborative)?
            .read_collaborative()
    }
}

/// Combine two collaborative chain frontiers deterministically (order-free).
fn combine_chains(a: &ReplicaFrontier, b: &ReplicaFrontier) -> ReplicaFrontier {
    let (lo, hi) = if a.root <= b.root { (a, b) } else { (b, a) };
    let mut h = blake3::Hasher::new();
    h.update(BODY_CHAIN_DOMAIN);
    h.update(&lo.root);
    h.update(&hi.root);
    ReplicaFrontier::new(
        *h.finalize().as_bytes(),
        a.transaction_count.max(b.transaction_count),
    )
}

fn object_ref(bytes: &[u8]) -> Object {
    Object {
        hash: journal::object_content_hash(bytes),
        len: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
    }
}

fn artifact_object(reference: &ArtifactRef) -> Object {
    Object {
        hash: reference.hash,
        len: reference.len,
    }
}

/// Encode the transport representation for one signed descriptor's artifact
/// closure. The descriptor is the authority; this pack is only a bounded
/// delivery container for the protected objects it names. Artifact order is
/// deliberately not semantic: Fabric accepts causally incomplete artifacts
/// pending and resolves them when their dependencies arrive.
const ARTIFACT_PACK_VERSION: u8 = 1;

pub(crate) fn encode_artifact_pack(artifacts: &[Vec<u8>]) -> Result<Vec<u8>, Failure> {
    let count = u16::try_from(artifacts.len()).map_err(|_| Failure::QuotaExceeded)?;
    let mut bytes = Vec::new();
    bytes.push(ARTIFACT_PACK_VERSION);
    bytes.extend_from_slice(&count.to_be_bytes());
    for artifact in artifacts {
        if bytes
            .len()
            .checked_add(8)
            .and_then(|len| len.checked_add(artifact.len()))
            .is_none_or(|len| len > MAX_BODY_BYTES)
        {
            return Err(Failure::QuotaExceeded);
        }
        let len = u64::try_from(artifact.len()).map_err(|_| Failure::QuotaExceeded)?;
        bytes.extend_from_slice(&len.to_be_bytes());
        bytes.extend_from_slice(artifact);
    }
    Ok(bytes)
}

pub(crate) fn decode_artifact_pack(
    descriptor: &Descriptor,
    bytes: &[u8],
) -> Result<Vec<Vec<u8>>, Failure> {
    decode_artifact_pack_inner(descriptor, bytes, true)
}

fn decode_artifact_delivery_pack(
    descriptor: &Descriptor,
    bytes: &[u8],
) -> Result<Vec<Vec<u8>>, Failure> {
    decode_artifact_pack_inner(descriptor, bytes, false)
}

fn decode_artifact_pack_inner(
    descriptor: &Descriptor,
    bytes: &[u8],
    require_complete: bool,
) -> Result<Vec<Vec<u8>>, Failure> {
    if bytes.len() > MAX_BODY_BYTES {
        return Err(Failure::Illegitimate(
            "artifact closure exceeds the Body maximum".into(),
        ));
    }
    let Some((&version, bytes)) = bytes.split_first() else {
        return Err(Failure::Illegitimate("artifact pack is truncated".into()));
    };
    if version != ARTIFACT_PACK_VERSION {
        return Err(Failure::Illegitimate(
            "artifact pack version is unsupported".into(),
        ));
    }
    let Some((count_bytes, mut remaining)) = bytes.split_at_checked(2) else {
        return Err(Failure::Illegitimate("artifact pack is truncated".into()));
    };
    let count =
        usize::from(u16::from_be_bytes(count_bytes.try_into().map_err(
            |_| Failure::Illegitimate("artifact pack is truncated".into()),
        )?));
    let expected = descriptor.artifact_refs().count();
    if count > expected || (require_complete && count != expected) {
        return Err(Failure::Illegitimate(
            "artifact closure is incomplete".into(),
        ));
    }
    let mut artifacts = Vec::with_capacity(count);
    for _ in 0..count {
        let Some((len_bytes, tail)) = remaining.split_at_checked(8) else {
            return Err(Failure::Illegitimate("artifact pack is truncated".into()));
        };
        let len = u64::from_be_bytes(
            len_bytes
                .try_into()
                .map_err(|_| Failure::Illegitimate("artifact pack is truncated".into()))?,
        );
        let len = usize::try_from(len)
            .map_err(|_| Failure::Illegitimate("artifact exceeds the Body maximum".into()))?;
        if len > MAX_BODY_BYTES {
            return Err(Failure::Illegitimate(
                "artifact exceeds the Body maximum".into(),
            ));
        }
        let Some((artifact, tail)) = tail.split_at_checked(len) else {
            return Err(Failure::Illegitimate("artifact pack is truncated".into()));
        };
        artifacts.push(artifact.to_vec());
        remaining = tail;
    }
    if !remaining.is_empty() {
        return Err(Failure::Illegitimate(
            "artifact pack has trailing bytes".into(),
        ));
    }
    let references: BTreeSet<([u8; 32], u64, [u8; 16])> = descriptor
        .artifact_refs()
        .map(|reference| (reference.hash, reference.len, reference.epoch))
        .collect();
    let mut delivered = BTreeMap::new();
    for artifact in artifacts {
        let Some(epoch) = mechanics::authorization::body_epoch_id(&artifact) else {
            return Err(Failure::Illegitimate(
                "artifact envelope has an invalid shape".into(),
            ));
        };
        if artifact.len() > MAX_BODY_BYTES || artifact.len() < BODY_ENVELOPE_OVERHEAD {
            return Err(Failure::Illegitimate(
                "artifact envelope has an invalid shape".into(),
            ));
        }
        let actual = object_ref(&artifact);
        if !references.contains(&(actual.hash, actual.len, epoch))
            || delivered
                .insert((actual.hash, actual.len, epoch), artifact)
                .is_some()
        {
            return Err(Failure::Illegitimate(
                "artifact does not match the signed closure".into(),
            ));
        }
    }
    let mut ordered = Vec::with_capacity(delivered.len());
    for reference in descriptor.artifact_refs() {
        match delivered.remove(&(reference.hash, reference.len, reference.epoch)) {
            Some(artifact) => ordered.push(artifact),
            None if require_complete => {
                return Err(Failure::Illegitimate(
                    "artifact closure is incomplete".into(),
                ));
            }
            None => {}
        }
    }
    Ok(ordered)
}

/// Validate one staged Body operation against the frozen algebra (path grammar
/// and limits) and translate it into its Engine operation. Replica owns this
/// translation; a World never authors Engine operations, and Engine never sees
/// an op Replica has not validated.
fn translate(key: Key, op: &Op) -> Result<fabric::Op, Failure> {
    let path_ok = |p: &str| {
        algebra::valid_path(p)
            .then_some(())
            .ok_or(Failure::PathInvalid)
    };
    let value_ok = |v: &[u8]| {
        (v.len() <= algebra::MAX_VALUE_BYTES)
            .then_some(())
            .ok_or(Failure::OpLimit)
    };
    Ok(match op {
        Op::ReplaceAtomic { value } => fabric::Op::PutCanonical {
            key,
            value: value.clone(),
        },
        Op::Create => fabric::Op::CreateBody { key },
        Op::Tombstone => fabric::Op::Remove { key },
        Op::RegisterSet { path, value } => {
            path_ok(path)?;
            value_ok(value)?;
            fabric::Op::RegisterSet {
                key,
                path: path.clone(),
                value: value.clone(),
            }
        }
        Op::RegisterClear { path } => {
            path_ok(path)?;
            fabric::Op::RegisterClear {
                key,
                path: path.clone(),
            }
        }
        Op::MapSet {
            path,
            key: entry,
            value,
        } => {
            path_ok(path)?;
            value_ok(value)?;
            if entry.len() > algebra::MAX_MAP_KEY_BYTES {
                return Err(Failure::OpLimit);
            }
            fabric::Op::MapSet {
                key,
                path: path.clone(),
                entry: entry.clone(),
                value: value.clone(),
            }
        }
        Op::MapRemove { path, key: entry } => {
            path_ok(path)?;
            fabric::Op::MapRemove {
                key,
                path: path.clone(),
                entry: entry.clone(),
            }
        }
        Op::ListInsert { path, index, value } => {
            path_ok(path)?;
            value_ok(value)?;
            fabric::Op::ListInsert {
                key,
                path: path.clone(),
                index: *index,
                value: value.clone(),
            }
        }
        Op::ListRemove { path, element } => {
            path_ok(path)?;
            fabric::Op::ListRemove {
                key,
                path: path.clone(),
                element: element.clone(),
            }
        }
        Op::ListMove {
            path,
            element,
            index,
        } => {
            path_ok(path)?;
            fabric::Op::ListMove {
                key,
                path: path.clone(),
                element: element.clone(),
                index: *index,
            }
        }
        Op::TextSplice {
            path,
            index,
            delete,
            insert,
        } => {
            path_ok(path)?;
            if insert.len() > algebra::MAX_TEXT_INSERT_BYTES {
                return Err(Failure::OpLimit);
            }
            fabric::Op::TextSplice {
                key,
                path: path.clone(),
                index: *index,
                delete: *delete,
                insert: insert.clone(),
            }
        }
        Op::SetAdd { path, value } => {
            path_ok(path)?;
            value_ok(value)?;
            fabric::Op::SetAdd {
                key,
                path: path.clone(),
                value: value.clone(),
            }
        }
        Op::SetRemove { path, value } => {
            path_ok(path)?;
            value_ok(value)?;
            fabric::Op::SetRemove {
                key,
                path: path.clone(),
                value: value.clone(),
            }
        }
        Op::CounterAdd { path, delta } => {
            path_ok(path)?;
            fabric::Op::CounterAdd {
                key,
                path: path.clone(),
                delta: *delta,
            }
        }
        Op::TreeInsert {
            path,
            parent,
            after,
            value,
        } => {
            path_ok(path)?;
            value_ok(value)?;
            fabric::Op::TreeInsert {
                key,
                path: path.clone(),
                parent: parent.clone(),
                after: after.clone(),
                value: value.clone(),
            }
        }
        Op::TreeMove {
            path,
            node,
            parent,
            after,
        } => {
            path_ok(path)?;
            fabric::Op::TreeMove {
                key,
                path: path.clone(),
                node: node.clone(),
                parent: parent.clone(),
                after: after.clone(),
            }
        }
        Op::TreeRemove { path, node } => {
            path_ok(path)?;
            fabric::Op::TreeRemove {
                key,
                path: path.clone(),
                node: node.clone(),
            }
        }
        Op::TreeSet {
            path,
            node,
            key: entry,
            value,
        } => {
            path_ok(path)?;
            value_ok(value)?;
            if entry.len() > algebra::MAX_MAP_KEY_BYTES {
                return Err(Failure::OpLimit);
            }
            fabric::Op::TreeSet {
                key,
                path: path.clone(),
                node: node.clone(),
                entry: entry.clone(),
                value: value.clone(),
            }
        }
        Op::TreeUnset {
            path,
            node,
            key: entry,
        } => {
            path_ok(path)?;
            fabric::Op::TreeUnset {
                key,
                path: path.clone(),
                node: node.clone(),
                entry: entry.clone(),
            }
        }
        Op::TreeAnchor {
            path,
            anchor,
            parent,
        } => {
            path_ok(path)?;
            // An anchor is stored as a node data entry, so it is bounded by
            // what a map key is bounded by — the same limit, because it is
            // literally the same storage.
            if anchor.len() > algebra::MAX_MAP_KEY_BYTES
                || parent
                    .as_ref()
                    .is_some_and(|p| p.len() > algebra::MAX_MAP_KEY_BYTES)
            {
                return Err(Failure::OpLimit);
            }
            fabric::Op::TreeAnchor {
                key,
                path: path.clone(),
                anchor: anchor.clone(),
                parent: parent.clone(),
            }
        }
        Op::LogAppend {
            path,
            value,
            retain,
        } => {
            path_ok(path)?;
            value_ok(value)?;
            fabric::Op::LogAppend {
                key,
                path: path.clone(),
                value: value.clone(),
                retain: *retain,
            }
        }
    })
}

// A note on `BODY_EPOCH_ID_LEN`: referenced for the doc contract; the concrete
// parsing lives in mechanics.
const _: () = assert!(BODY_EPOCH_ID_LEN == 16);

#[cfg(test)]
mod generation_format_tests {
    use super::*;

    #[test]
    fn version_two_store_meta_is_a_lossless_input_to_generation_journaling() {
        let prior = PriorIndexedStoreMeta {
            format_version: READABLE_STORE_META_FORMAT_VERSION,
            space: None,
            frontier: ReplicaFrontier::EMPTY,
            quota: QuotaConfig::default(),
            body_index_root: None,
            manifest_body_root: None,
            content_index_root: None,
            receipt_index_root: None,
            manifest_root: None,
        };
        let bytes = postcard::to_stdvec(&prior).expect("v2 meta");
        let (current, receipt_ledger_complete) =
            decode_store_meta(&bytes).expect("v2 remains readable");
        assert!(!receipt_ledger_complete);
        assert_eq!(current.format_version, STORE_META_FORMAT_VERSION);
        assert_eq!(current.frontier, prior.frontier);
        assert_eq!(current.quota, prior.quota);
        assert!(current.generation_index_root.is_none());
    }

    /// The generation immediately behind the current one must open, and must
    /// keep the generation root that distinguishes it from version 2.
    #[test]
    fn the_previous_generation_opens_and_keeps_its_generation_root() {
        let root = IndexRef {
            hash: [7u8; 32],
            count: 4,
        };
        let prior = PriorGenerationStoreMeta {
            format_version: PRIOR_GENERATION_STORE_META_FORMAT_VERSION,
            space: None,
            frontier: ReplicaFrontier::EMPTY,
            quota: QuotaConfig::default(),
            body_index_root: None,
            manifest_body_root: None,
            content_index_root: None,
            receipt_index_root: Some(root),
            generation_index_root: Some(root),
            manifest_root: None,
        };
        let bytes = postcard::to_stdvec(&prior).expect("v3 meta");
        let (current, receipt_ledger_complete) =
            decode_store_meta(&bytes).expect("the previous generation remains readable");

        assert!(!receipt_ledger_complete);
        assert_eq!(current.format_version, STORE_META_FORMAT_VERSION);
        assert_eq!(current.generation_index_root, Some(root));
        assert_eq!(current.receipt_count, root.count);
        assert!(current.ownership_index_root.is_none());
    }

    /// A version this build cannot read is refused by naming both figures.
    /// Reporting it as damage sends somebody looking for a defect that is not
    /// there.
    #[test]
    fn an_unreadable_version_names_itself_and_what_this_build_reads() {
        let mut prior = PriorGenerationStoreMeta {
            format_version: STORE_META_FORMAT_VERSION + 1,
            space: None,
            frontier: ReplicaFrontier::EMPTY,
            quota: QuotaConfig::default(),
            body_index_root: None,
            manifest_body_root: None,
            content_index_root: None,
            receipt_index_root: None,
            generation_index_root: None,
            manifest_root: None,
        };
        let bytes = postcard::to_stdvec(&prior).expect("meta");
        let Err(failure) = decode_store_meta(&bytes) else {
            panic!("a version from the future was accepted");
        };
        let said = format!("{failure:?}");
        assert!(
            said.contains(&(STORE_META_FORMAT_VERSION + 1).to_string())
                && said.contains(&STORE_META_FORMAT_VERSION.to_string()),
            "the refusal named neither figure: {said}"
        );

        prior.format_version = 1;
        let bytes = postcard::to_stdvec(&prior).expect("meta");
        let Err(failure) = decode_store_meta(&bytes) else {
            panic!("a version below the readable floor was accepted");
        };
        assert!(
            format!("{failure:?}").contains(&READABLE_STORE_META_FORMAT_VERSION.to_string()),
            "the refusal did not say what this build reads"
        );
    }
}

#[cfg(test)]
mod body_directory_tests {
    use super::*;

    fn key(number: u32) -> BodyKey {
        let mut raw = [0u8; 16];
        raw[12..].copy_from_slice(&number.to_be_bytes());
        BodyKey::new(
            WorldId::parse("dev.lait.scale").expect("world"),
            crate::body::BodyId::from_bytes(raw),
        )
    }

    fn value(number: u32) -> SnapshotBody {
        let body_key = key(number);
        let fabric_key = fabric_key(&body_key);
        SnapshotBody::resident(
            &body_key,
            BodyBinding {
                schema: SchemaId::parse("record").expect("schema"),
                schema_version: 1,
                encoding: EncodingId::parse("postcard").expect("encoding"),
                mutation_model: MUTATION_ATOMIC,
            },
            snapshot_stamp(&number.to_be_bytes()),
            fabric::BodySnapshot::from_export(
                &fabric_key,
                fabric::BodyExport::Atomic(number.to_be_bytes().to_vec()),
            )
            .expect("snapshot"),
        )
    }

    #[test]
    fn lexical_front_insert_preserves_slots_and_unchanged_directory_leaves() {
        let mut builder = BodyDirectoryBuilder::default();
        for number in 1..=1_024 {
            builder.push(Arc::new(key(number)), value(number));
        }
        let prior = builder.finish();
        let held_key = key(777);
        let held_ix = prior.body_ix(&held_key).expect("held slot");
        let prior_key = prior.slot(held_ix).expect("held row").key.clone();
        let untouched = prior
            .lookup
            .leaves
            .iter()
            .skip(1)
            .cloned()
            .collect::<Vec<_>>();

        let mut next = prior.clone();
        next.insert(Arc::new(key(0)), value(0));

        assert_eq!(next.body_ix(&held_key), Some(held_ix));
        assert!(Arc::ptr_eq(
            &prior_key,
            &next.slot(held_ix).expect("stable row").key
        ));
        assert_eq!(
            next.body_ix(&key(0)).map(BodyIx::as_u32),
            Some(1_024),
            "a lexical-front insert appends a stable slot instead of renumbering"
        );
        for leaf in untouched {
            assert!(
                next.lookup
                    .leaves
                    .iter()
                    .any(|candidate| Arc::ptr_eq(candidate, &leaf)),
                "an unaffected lookup leaf must be shared Arc-identically"
            );
        }
    }

    #[test]
    fn source_fingerprint_is_slot_independent_and_stamp_sensitive() {
        let binding = value(1).binding;
        let snapshot =
            ReadSnapshot::from_body_rows_for_test([1u32, 2, 3].into_iter().map(|number| {
                let body_key = key(number);
                let SnapshotImage::Resident(body) = value(number).image else {
                    unreachable!("Body directory fixture is resident")
                };
                (
                    body_key.clone(),
                    binding.clone(),
                    number.to_be_bytes().to_vec(),
                    body,
                )
            }));
        let mut different_slots = snapshot.clone();
        different_slots.bodies.remove(&key(1));
        different_slots.bodies.remove(&key(2));
        different_slots.bodies.insert(Arc::new(key(2)), value(2));
        different_slots.bodies.insert(Arc::new(key(1)), value(1));
        different_slots.schema_bodies = schema_body_index(&different_slots.bodies);
        let world = WorldId::parse("dev.lait.scale").expect("world");
        let sources = [(SchemaId::parse("record").expect("schema"), 1)];
        assert_eq!(
            snapshot.body_payload_bytes_with_schema_version(&world, &sources[0].0, 1),
            12
        );
        assert_eq!(
            snapshot.source_fingerprint(&world, &sources),
            different_slots.source_fingerprint(&world, &sources),
            "construction/slot allocation order is not cache identity"
        );

        let mut changed = snapshot.clone();
        let changed_ix = changed.bodies.body_ix(&key(2)).expect("changed slot");
        changed
            .bodies
            .slots
            .get_mut(changed_ix.as_u32() as usize)
            .and_then(Option::as_mut)
            .expect("changed row")
            .value
            .stamp = snapshot_stamp(&99u32.to_be_bytes());
        assert_ne!(
            snapshot.source_fingerprint(&world, &sources),
            changed.source_fingerprint(&world, &sources),
            "an exact source stamp change must miss the persisted corpus image"
        );
    }
}

#[cfg(test)]
mod receipt_cache_tests {
    use super::*;

    fn receipt(ordinal: u16, effect_bytes: usize) -> RequestReceipt {
        let mut request = [0u8; 16];
        request[..2].copy_from_slice(&ordinal.to_be_bytes());
        RequestReceipt {
            version: 2,
            space: SpaceId::from_digest([0x51; 16]),
            world: WorldId::parse("dev.lait.receipts").expect("world"),
            device: mechanics::actor::device_from_seed(&[0x52; 32]),
            request,
            payload_hash: [0x53; 32],
            effect: vec![0x54; effect_bytes],
            bodies: Vec::new(),
            frontier: ReplicaFrontier::EMPTY,
            manifest_root: [0x55; 32],
            implementation_digest: [0x56; 32],
            extractor_schema_digest: [0x57; 32],
            transaction: [0x58; 32],
        }
    }

    #[test]
    fn hot_receipts_are_bounded_by_count_and_physical_bytes() {
        let mut cache = ReceiptCache::default();
        for ordinal in 0..300 {
            let receipt = receipt(ordinal, 0);
            let bytes = receipt.encode();
            cache.insert(receipt.scope_key(), receipt, object_ref(&bytes));
        }
        assert_eq!(cache.order.len(), HOT_RECEIPT_CACHE);
        assert!(cache.retained_bytes_estimate() <= HOT_RECEIPT_CACHE_BYTES);

        let mut large = ReceiptCache::default();
        for ordinal in 0..64 {
            let receipt = receipt(ordinal, crate::receipt::MAX_EFFECT_BYTES);
            let bytes = receipt.encode();
            large.insert(receipt.scope_key(), receipt, object_ref(&bytes));
        }
        assert!(large.order.len() < 64);
        assert!(large.retained_bytes_estimate() <= HOT_RECEIPT_CACHE_BYTES);
    }

    #[test]
    fn singleton_body_record_layout_is_explicitly_bounded() {
        eprintln!(
            "replica-record-layout body_record={} body_head={} heads={} binding={} frontier={} material={} artifact_ref={} head_artifacts={} directory_entry={} snapshot_body={} snapshot_slot={}",
            std::mem::size_of::<BodyRecord>(),
            std::mem::size_of::<BodyHead>(),
            std::mem::size_of::<smallvec::SmallVec<[BodyHead; 1]>>(),
            std::mem::size_of::<BodyBinding>(),
            std::mem::size_of::<ReplicaFrontier>(),
            std::mem::size_of::<CausalMaterial>(),
            std::mem::size_of::<ArtifactRef>(),
            std::mem::size_of::<smallvec::SmallVec<[ArtifactRef; 1]>>(),
            std::mem::size_of::<SnapshotDirectoryEntry<BodyKey, Arc<BodyRecord>>>(),
            std::mem::size_of::<SnapshotBody>(),
            std::mem::size_of::<BodySlot>(),
        );
        assert!(std::mem::size_of::<BodyRecord>() <= 512);
    }
}
