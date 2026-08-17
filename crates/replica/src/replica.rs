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
use std::sync::{mpsc, Arc, Mutex, OnceLock};

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
pub use crate::protected::{MUTATION_ATOMIC, MUTATION_COLLABORATIVE};

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
    /// The application effect exceeded [`crate::receipt::MAX_EFFECT_BYTES`].
    /// Nothing was committed.
    EffectTooLarge,
    /// The Space material quota (bytes or Body count) would be exceeded.
    /// Nothing was committed and no staging was retained.
    QuotaExceeded,
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
pub enum PreparedActionOutcome<'a> {
    Prepared(PreparedAction<'a>),
    Replayed(RequestReceipt),
}

impl PreparedActionOutcome<'_> {
    /// The original receipt for a replay, or the candidate receipt for a fresh
    /// request.
    pub fn receipt(&self) -> &RequestReceipt {
        match self {
            Self::Prepared(prepared) => prepared.receipt(),
            Self::Replayed(receipt) => receipt,
        }
    }
}

/// One locally prepared action. The guard exclusively borrows its Replica, so
/// no other commit can interleave between candidate extraction and durable
/// publication. Dropping it rolls the Fabric candidate back.
pub struct PreparedAction<'a> {
    replica: &'a mut Replica,
    state: Option<PreparedActionState>,
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
    /// [`MUTATION_ATOMIC`] or [`MUTATION_COLLABORATIVE`].
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
    /// Protected Fabric artifact objects in signed descriptor order (durable
    /// stores only). The descriptor itself lives in `transaction`.
    artifacts: Vec<Object>,
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
    heads: Vec<BodyHead>,
    /// Whether the Body is interpreted by the local engine. `false` is the
    /// opaque branch: retained byte-identically, absent from reads.
    interpreted: bool,
    /// The protected causal artifact closure for the current interpreted
    /// state. It is local durable material, not part of the signed peer head:
    /// the interchange protocol still carries the author's original envelope.
    /// Ordinary edits extend this bounded descriptor by one delta reference.
    /// The one-time indexed-v2 baseline migration decodes its prior record
    /// shape explicitly and initializes this to `None`.
    causal: Option<CausalMaterial>,
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
    /// Generation id → immutable changed-Body delta object. The delta objects
    /// themselves are retained requirements; this index makes ancestry and
    /// exact historical reconstruction logarithmically addressable.
    generation_index_root: Option<IndexRef>,
    manifest_root: Option<Object>,
}

/// The immediately prior indexed-catalog format. Version 3 adds only the
/// generation journal root, so a version-2 store is a lossless input: it opens
/// by recording its current committed state as the first complete generation
/// baseline while writing version 3 metadata.
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
const STORE_META_FORMAT_VERSION: u8 = 4;
const READABLE_STORE_META_FORMAT_VERSION: u8 = 2;

fn decode_store_meta(bytes: &[u8]) -> Result<StoreMeta, Failure> {
    if let Ok(meta) = postcard::from_bytes::<StoreMeta>(bytes) {
        if meta.format_version == STORE_META_FORMAT_VERSION {
            return Ok(meta);
        }
    }
    let prior: PriorIndexedStoreMeta = postcard::from_bytes(bytes).map_err(|error| {
        integrity_cause(
            Defect::Encoding,
            "decode prior indexed store metadata",
            error,
        )
    })?;
    if prior.format_version != READABLE_STORE_META_FORMAT_VERSION {
        return Err(Failure::Integrity(Defect::Encoding));
    }
    Ok(StoreMeta {
        format_version: STORE_META_FORMAT_VERSION,
        space: prior.space,
        frontier: prior.frontier,
        quota: prior.quota,
        body_index_root: prior.body_index_root,
        manifest_body_root: prior.manifest_body_root,
        content_index_root: prior.content_index_root,
        receipt_index_root: prior.receipt_index_root,
        generation_index_root: None,
        manifest_root: prior.manifest_root,
    })
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

/// One Body replacement in a durable generation delta. `material: None` means
/// absent or opaque at that generation; neither is readable by a World. The
/// descriptor names protected causal artifacts rather than embedding a full
/// plaintext Body export in every generation.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ArchivedBody {
    key: BodyKey,
    binding: Option<BodyBinding>,
    stamp: Vec<u8>,
    material: Option<CausalMaterial>,
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
}

const GENERATION_DELTA_FORMAT_VERSION: u8 = 2;

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

/// The index key a receipt scope sits under.
fn receipt_index_key(scope: &[u8]) -> [u8; 32] {
    let mut h = blake3::Hasher::new();
    h.update(b"lait/replica/receipt-scope/1");
    h.update(scope);
    *h.finalize().as_bytes()
}

/// The Orbit's durable local materialization, over a Engine engine.
struct PreparedCheckpoint {
    base: CausalMaterial,
    receiver: mpsc::Receiver<Option<(ArtifactRef, Vec<u8>)>>,
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
    fabric: Engine,
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
    keys: Option<Arc<dyn BodyKeySource>>,
    space: Option<SpaceId>,
    supported: SupportedSchemas,
    quota: QuotaConfig,
    bodies: BTreeMap<BodyKey, BodyRecord>,
    receipts: BTreeMap<Vec<u8>, (RequestReceipt, Option<Object>)>,
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
    manifest_root_object: Option<Object>,
    /// How many live Body-head references each signed transaction object has.
    ///
    /// A commit must tell the journal which objects stopped being required, and
    /// "the ones this Body used to name" is the wrong answer: one signed
    /// transaction record covers every Body in its batch, so dropping it when
    /// one of them moves on would strand the others. Counting is the cheap
    /// correct answer, and it stays O(changed) because only touched Bodies
    /// adjust it. Protected artifacts are generation material: once published
    /// they remain required for historical reads, so they are intentionally
    /// never put through this live-head release map.
    object_refs: BTreeMap<[u8; 32], u64>,
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

/// One interpreted Body in an immutable Replica generation.
#[derive(Debug, Clone)]
struct SnapshotBody {
    binding: BodyBinding,
    stamp: Vec<u8>,
    body: fabric::BodySnapshot,
}

/// Calibrated above the 1-record-per-Body release fixture after excluding the
/// exact export/stamp bytes: shared BodyKey, dense Body directory, schema
/// membership, binding, and leaf/spine slack.
const SNAPSHOT_BODY_FIXED_ESTIMATE: u64 = 400;

fn snapshot_body_retained_estimate(body: &SnapshotBody) -> u64 {
    SNAPSHOT_BODY_FIXED_ESTIMATE
        .saturating_add(u64::try_from(body.stamp.len()).unwrap_or(u64::MAX))
        .saturating_add(body.body.retained_bytes())
}

fn snapshot_directory_retained_estimate(
    bodies: &SnapshotDirectory<Arc<BodyKey>, SnapshotBody>,
) -> u64 {
    bodies.iter().fold(0u64, |total, (_, body)| {
        total.saturating_add(snapshot_body_retained_estimate(body))
    })
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

impl<K, V> Default for SnapshotDirectory<K, V> {
    fn default() -> Self {
        Self {
            leaves: imbl::Vector::new(),
            len: 0,
        }
    }
}

impl<K: Clone + Ord, V: Clone> SnapshotDirectory<K, V> {
    fn from_entries(mut entries: Vec<(K, V)>) -> Self {
        entries.sort_by(|left, right| left.0.cmp(&right.0));
        debug_assert!(entries.windows(2).all(|pair| pair[0].0 != pair[1].0));
        let len = entries.len();
        let leaves = entries
            .chunks(SNAPSHOT_DIRECTORY_LEAF)
            .map(|chunk| {
                Arc::from(
                    chunk
                        .iter()
                        .cloned()
                        .map(|(key, value)| SnapshotDirectoryEntry { key, value })
                        .collect::<Vec<_>>(),
                )
            })
            .collect();
        Self { leaves, len }
    }

    fn len(&self) -> usize {
        self.len
    }

    fn is_empty(&self) -> bool {
        self.len == 0
    }

    fn leaf_for<Q>(&self, key: &Q) -> usize
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        let mut low = 0usize;
        let mut high = self.leaves.len();
        while low < high {
            let mid = low + (high - low) / 2;
            let leaf = self.leaves.get(mid).expect("snapshot directory midpoint");
            let last = leaf.last().expect("snapshot directory leaf is non-empty");
            if last.key.borrow() < key {
                low = mid + 1;
            } else {
                high = mid;
            }
        }
        low.min(self.leaves.len().saturating_sub(1))
    }

    fn get_key_value<Q>(&self, key: &Q) -> Option<(&K, &V)>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        if self.leaves.is_empty() {
            return None;
        }
        let leaf = self.leaves.get(self.leaf_for(key))?;
        let position = leaf
            .binary_search_by(|entry| entry.key.borrow().cmp(key))
            .ok()?;
        let entry = &leaf[position];
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
        let leaf_index = self.leaf_for(&key);
        let mut leaf = self.leaves[leaf_index].to_vec();
        match leaf.binary_search_by(|entry| entry.key.cmp(&key)) {
            Ok(position) => {
                let old = std::mem::replace(&mut leaf[position].value, value);
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
                    self.leaves.insert(leaf_index + 1, Arc::from(right));
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
        let leaf_index = self.leaf_for(key);
        let mut leaf = self.leaves[leaf_index].to_vec();
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
        let mut leaf_index = after.map_or(0, |key| self.leaf_for(key));
        let mut first = true;
        let mut page = Vec::with_capacity(limit);
        while leaf_index < self.leaves.len() && page.len() < limit {
            let leaf = &self.leaves[leaf_index];
            let start = if first {
                first = false;
                after.map_or(0, |key| {
                    leaf.partition_point(|entry| entry.key.borrow() <= key)
                })
            } else {
                0
            };
            page.extend(
                leaf[start..]
                    .iter()
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
    bodies: SnapshotDirectory<Arc<BodyKey>, SnapshotBody>,
    /// Exact schema membership, persistently shared across generations.
    /// Exec and World projections enter through this index rather than
    /// scanning the Space-wide Body map.
    schema_bodies: imbl::OrdMap<(WorldId, SchemaId, u32), SnapshotDirectory<Arc<BodyKey>, ()>>,
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

type SchemaBodyIndex = imbl::OrdMap<(WorldId, SchemaId, u32), SnapshotDirectory<Arc<BodyKey>, ()>>;

fn insert_schema_body(index: &mut SchemaBodyIndex, key: Arc<BodyKey>, binding: &BodyBinding) {
    let coordinate = (
        key.world.clone(),
        binding.schema.clone(),
        binding.schema_version,
    );
    let mut keys = index.get(&coordinate).cloned().unwrap_or_default();
    keys.insert(key, ());
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
    keys.remove(key);
    if keys.is_empty() {
        index.remove(&coordinate);
    } else {
        index.insert(coordinate, keys);
    }
}

fn schema_body_index(bodies: &SnapshotDirectory<Arc<BodyKey>, SnapshotBody>) -> SchemaBodyIndex {
    let mut grouped = BTreeMap::<(WorldId, SchemaId, u32), Vec<(Arc<BodyKey>, ())>>::new();
    for (key, body) in bodies.iter() {
        grouped
            .entry((
                key.world.clone(),
                body.binding.schema.clone(),
                body.binding.schema_version,
            ))
            .or_default()
            .push((key.clone(), ()));
    }
    grouped
        .into_iter()
        .map(|(coordinate, entries)| (coordinate, SnapshotDirectory::from_entries(entries)))
        .collect()
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
        let mut entries = Vec::new();
        for (key, binding, stamp, body) in rows {
            entries.push((
                Arc::new(key),
                SnapshotBody {
                    binding,
                    stamp,
                    body,
                },
            ));
        }
        let bodies = SnapshotDirectory::from_entries(entries);
        let schema_bodies = schema_body_index(&bodies);
        let retained_bytes_estimate = snapshot_directory_retained_estimate(&bodies);
        Self {
            root: [0u8; 32],
            frontier: ReplicaFrontier::EMPTY,
            bodies,
            schema_bodies,
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

    pub fn read(&self, key: &BodyKey) -> Option<Vec<u8>> {
        self.bodies.get(key)?.body.read()
    }

    pub fn read_collaborative(
        &self,
        key: &BodyKey,
    ) -> Result<fabric::CollaborativeView, fabric::projection::Failure> {
        let Some(body) = self.bodies.get(key) else {
            return Err(fabric::projection::Failure::NotCollaborative);
        };
        body.body.read_collaborative()
    }

    pub fn body_version(&self, key: &BodyKey) -> Option<fabric::Version> {
        self.bodies.get(key)?.body.version().ok()
    }

    pub fn anchor(&self, key: &BodyKey, path: &str, position: u64) -> Option<fabric::Anchor> {
        self.bodies
            .get(key)?
            .body
            .anchor(&fabric_key(key), path, position)
    }

    pub fn resolve_anchor(
        &self,
        key: &BodyKey,
        anchor: &fabric::Anchor,
    ) -> fabric::AnchorResolution {
        self.bodies
            .get(key)
            .map(|body| body.body.resolve(&fabric_key(key), anchor))
            .unwrap_or(fabric::AnchorResolution::Drifted)
    }

    pub fn binding(&self, key: &BodyKey) -> Option<&BodyBinding> {
        self.bodies.get(key).map(|body| &body.binding)
    }

    pub fn body_stamp(&self, key: &BodyKey) -> Option<Vec<u8>> {
        self.bodies.get(key).map(|body| body.stamp.clone())
    }

    pub fn body_keys(&self) -> Vec<BodyKey> {
        self.bodies.keys().map(|key| key.as_ref().clone()).collect()
    }

    /// Number of readable Bodies in this immutable generation, without
    /// cloning their keys or walking schema membership.
    pub fn body_count(&self) -> u64 {
        u64::try_from(self.bodies.len()).unwrap_or(u64::MAX)
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
            .map(|keys| keys.keys().map(|key| key.as_ref().clone()).collect())
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
            .flat_map(|(_, keys)| keys.keys().map(|key| key.as_ref().clone()))
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
            .flat_map(|(_, keys)| keys.keys_page_after(after, limit))
            .map(|key| key.as_ref().clone())
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
    fn generation_delta(&self, root: &[u8; 32]) -> Result<Option<GenerationDelta>, Failure> {
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
        let mut cursor = *root;
        let mut seen = BTreeSet::new();
        let mut deltas = Vec::new();
        loop {
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
        let mut bodies = SnapshotDirectory::default();
        let mut resolved = BTreeSet::new();
        for delta in &deltas {
            for archived in &delta.changed {
                if !resolved.insert(archived.key.clone()) {
                    continue;
                }
                if let (Some(binding), Some(material)) = (&archived.binding, &archived.material) {
                    let body = self.body_from_causal_material(&archived.key, material)?;
                    bodies.insert(
                        Arc::new(archived.key.clone()),
                        SnapshotBody {
                            binding: binding.clone(),
                            stamp: archived.stamp.clone(),
                            body,
                        },
                    );
                }
            }
        }
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
            schema_bodies: schema_body_index(&bodies),
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

impl PreparedAction<'_> {
    /// The receipt that will become authoritative if this candidate is
    /// finalized.
    pub fn receipt(&self) -> &RequestReceipt {
        match self.state.as_ref().expect("prepared action retains state") {
            PreparedActionState::Noop { receipt } => receipt,
            PreparedActionState::Mutation { data, .. } => &data.receipt,
        }
    }

    /// The exact read coordinate the durable finalize will publish.
    pub fn candidate_root(&self) -> [u8; 32] {
        match self.state.as_ref().expect("prepared action retains state") {
            PreparedActionState::Noop { .. } => {
                let root = self.replica.manifest_root();
                if root == crate::transaction::NO_PARENT_ROOT {
                    self.replica.frontier.root
                } else {
                    root
                }
            }
            PreparedActionState::Mutation { data, .. } => data.candidate_root,
        }
    }

    /// Freeze the prepared candidate by replacing only touched persistent-map
    /// paths. `prior` must be the Replica's current committed generation; a
    /// historical image is never silently advanced as though it were current.
    pub fn candidate_snapshot(&self, prior: &ReadSnapshot) -> Result<ReadSnapshot, Failure> {
        let committed_root = {
            let root = self.replica.manifest_root();
            if root == crate::transaction::NO_PARENT_ROOT {
                self.replica.frontier.root
            } else {
                root
            }
        };
        if prior.root != committed_root || prior.frontier != self.replica.frontier {
            return Err(Failure::ParentManifestUnavailable);
        }
        let PreparedActionState::Mutation { data, .. } =
            self.state.as_ref().expect("prepared action retains state")
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

        let mut bodies = prior.bodies.clone();
        let mut schema_bodies = prior.schema_bodies.clone();
        let mut retained_bytes_estimate = prior.retained_bytes_estimate;
        for (key, record) in new_records {
            let shared_key = if let Some((held, prior_body)) = prior.bodies.get_key_value(key) {
                retained_bytes_estimate = retained_bytes_estimate
                    .saturating_sub(snapshot_body_retained_estimate(prior_body));
                remove_schema_body(&mut schema_bodies, key, &prior_body.binding);
                held.clone()
            } else {
                Arc::new(key.clone())
            };
            let frozen = record.as_ref().filter(|record| record.interpreted).map(
                |record| -> Result<SnapshotBody, Failure> {
                    let body = self
                        .replica
                        .fabric
                        .body_snapshot(&fabric_key(key))
                        .map_err(Failure::Engine)?
                        .ok_or(Failure::Integrity(Defect::MissingMaterial))?;
                    Ok(SnapshotBody {
                        binding: record.binding.clone(),
                        stamp: record_stamp(record),
                        body,
                    })
                },
            );
            match frozen.transpose()? {
                Some(body) => {
                    retained_bytes_estimate = retained_bytes_estimate
                        .saturating_add(snapshot_body_retained_estimate(&body));
                    insert_schema_body(&mut schema_bodies, shared_key.clone(), &body.binding);
                    bodies.insert(shared_key, body);
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
            let old = self
                .replica
                .declared_content
                .get(key)
                .map(Vec::as_slice)
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
                .replica
                .declared_content_counts
                .get(&id)
                .copied()
                .unwrap_or(0);
            let next = current.saturating_sub(removed).saturating_add(added);
            if next == 0 {
                content.remove(&id);
            } else if !content.contains_key(&id) {
                let descriptor = self
                    .replica
                    .content_descriptor(&crate::content::ContentRef { content_id: id })
                    .ok_or(Failure::Integrity(Defect::MissingMaterial))?;
                content.insert(id, descriptor);
            }
        }

        Ok(ReadSnapshot {
            root: *candidate_root,
            frontier: *next_frontier,
            bodies,
            schema_bodies,
            declared_content,
            content,
            retained_bytes_estimate,
        })
    }

    /// Durably publish the prepared transaction, then accept its live Fabric
    /// state. The caller may install the already-built read image only after
    /// this succeeds.
    pub fn finalize(mut self, ctx: &CommitContext<'_>) -> Result<RequestReceipt, Failure> {
        let state = self.state.take().expect("prepared action retains state");
        self.replica.finalize_prepared_action(ctx, state)
    }
}

impl Drop for PreparedAction<'_> {
    fn drop(&mut self) {
        let Some(PreparedActionState::Mutation { fabric, .. }) = self.state.take() else {
            return;
        };
        if self.replica.fabric.rollback(fabric).is_err() {
            self.replica.poisoned = true;
            tracing::error!("prepared Replica action could not be rolled back");
        }
    }
}

impl Replica {
    /// Build a Replica over a given Engine engine (no durability, no keys).
    fn from_engine(fabric: Engine) -> Self {
        Self {
            fabric,
            frontier: ReplicaFrontier::EMPTY,
            durable: None,
            // Nothing was read off a disk to build this, so nothing was
            // verified. `Replica::open` is the only thing that sets this.
            verified_at_ms: None,
            poisoned: false,
            keys: None,
            space: None,
            supported: SupportedSchemas::default(),
            quota: QuotaConfig::default(),
            bodies: BTreeMap::new(),
            body_index_root: None,
            manifest_body_root: None,
            content_index_root: None,
            declared_content: BTreeMap::new(),
            declared_content_counts: BTreeMap::new(),
            pending_content: BTreeMap::new(),
            receipt_index_root: None,
            generation_index_root: None,
            manifest_root_object: None,
            object_refs: BTreeMap::new(),
            receipts: BTreeMap::new(),
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
        let mut bytes: u64 = 0;
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
        for (receipt, _) in self.receipts.values() {
            bytes = bytes.saturating_add(u64::try_from(receipt.encode().len()).unwrap_or(u64::MAX));
        }
        (bytes, u64::try_from(self.bodies.len()).unwrap_or(u64::MAX))
    }

    /// The retained-unknown-World usage for one World: (bytes, bodies).
    pub fn opaque_usage(&self, world: &WorldId) -> (u64, u64) {
        let mut bytes: u64 = 0;
        let mut count: u64 = 0;
        for (key, record) in &self.bodies {
            if !record.interpreted && &key.world == world {
                bytes = bytes.saturating_add(record.protected_total());
                count = count.saturating_add(1);
            }
        }
        (bytes, count)
    }

    /// Open the durable Replica at a journaled store root: run crash recovery,
    /// verify and load the canonical object graph (signed transactions, sealed
    /// Body payloads, receipts, manifest), and import every Body whose key
    /// epoch is locally held into the engine. A Body without local key
    /// material is retained opaquely. Missing or corrupt objects fail
    /// integrity validation without heuristic repair.
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
        let mut replica = Self::from_engine(Engine::new()).with_keys(keys.clone());
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
        let meta = decode_store_meta(&meta_bytes)?;
        replica.frontier = meta.frontier;
        replica.space = meta.space.clone();
        replica.quota = meta.quota.clamped();
        replica.body_index_root = meta.body_index_root;
        replica.manifest_body_root = meta.manifest_body_root;
        replica.content_index_root = meta.content_index_root;
        replica.receipt_index_root = meta.receipt_index_root;
        replica.generation_index_root = meta.generation_index_root;
        replica.manifest_root_object = meta.manifest_root;

        // Stream the catalogs rather than decoding one giant vector. The engine
        // still materialises every Body — Engine holds a document per Body — so
        // opening remains proportional to the store. What changed is that the
        // commit point no longer is.
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

        let mut indexed_receipts: Vec<IndexedReceipt> = Vec::new();
        let mut decode_failure = false;
        crate::index::stream(&StoreNodes(&store), meta.receipt_index_root, &mut |entry| {
            if decode_failure {
                return;
            }
            match postcard::from_bytes::<IndexedReceipt>(&entry.value) {
                Ok(receipt) => indexed_receipts.push(receipt),
                Err(_) => decode_failure = true,
            }
        })
        .map_err(|_| Failure::Integrity(Defect::Index))?;
        if decode_failure {
            return Err(Failure::Integrity(Defect::Encoding));
        }

        for IndexedBody { key, mut record } in indexed_bodies {
            if record.heads.is_empty() {
                return Err(Failure::Integrity(Defect::MissingMaterial));
            }
            // Load and verify EVERY constituent head; a multi-writer Body's
            // state is the engine merge of all of them, restored in order.
            let mut loaded: Vec<(Descriptor, Vec<Vec<u8>>, Vec<u8>)> = Vec::new();
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
                        && record.binding.mutation_model == MUTATION_ATOMIC
                {
                    return Err(Failure::Integrity(Defect::CorruptMaterial));
                }
                let expected: Vec<Object> = descriptor
                    .artifact_refs()
                    .map(|reference| Object {
                        hash: reference.hash,
                        len: reference.len,
                    })
                    .collect();
                if expected != head.artifacts {
                    return Err(Failure::Integrity(Defect::CorruptMaterial));
                }
                let mut artifacts = Vec::with_capacity(head.artifacts.len());
                for reference in &head.artifacts {
                    artifacts.push(
                        store
                            .read_object(reference)
                            .map_err(|_| Failure::Integrity(Defect::Encoding))?,
                    );
                }
                loaded.push((descriptor, artifacts, tx_bytes));
            }
            // A Body retained opaquely stays opaque at reopen: interpreting it
            // later requires explicit revalidation through the incorporation
            // path, never a silent flip on restart. A Body that WAS
            // interpreted must open again — if its epoch key has since gone
            // away it degrades to opaque (retained, unread) rather than
            // failing the whole store.
            let mut degraded = !record.interpreted;
            if record.interpreted {
                for (descriptor, envelopes, _) in &loaded {
                    let mut opened = Vec::with_capacity(envelopes.len());
                    for envelope in envelopes {
                        let epoch = mechanics::authorization::body_epoch_id(envelope)
                            .ok_or(Failure::Integrity(Defect::CorruptMaterial))?;
                        let Some(key_cap) = keys.opening_key(&epoch) else {
                            degraded = true;
                            break;
                        };
                        opened.push(
                            open_artifact(&key_cap, envelope)
                                .map_err(|_| Failure::Integrity(Defect::CorruptMaterial))?,
                        );
                    }
                    if degraded {
                        break;
                    }
                    let mut proof = Engine::new();
                    for artifact in &opened {
                        proof
                            .import_artifact(&fabric_key(&key), artifact)
                            .map_err(|_| Failure::Integrity(Defect::CorruptMaterial))?;
                    }
                    if proof
                        .version(&fabric_key(&key))
                        .map_err(|_| Failure::Integrity(Defect::CorruptMaterial))?
                        != descriptor.material.version
                    {
                        return Err(Failure::Integrity(Defect::CorruptMaterial));
                    }
                    if descriptor.mutation_model == MUTATION_COLLABORATIVE
                        || descriptor.resulting_frontier == record.chain
                    {
                        // `proof` has already imported and checked this exact
                        // material. Hand its immutable image to the long-lived
                        // Engine so the common one-head Body shares the Arc
                        // export+Version instead of decoding every artifact a
                        // second time during cold recovery. Concurrent heads
                        // still enter Fabric's ordinary merge path.
                        let verified = proof
                            .body_snapshot(&fabric_key(&key))
                            .map_err(|_| Failure::Integrity(Defect::CorruptMaterial))?
                            .ok_or(Failure::Integrity(Defect::MissingMaterial))?;
                        let status = replica
                            .fabric
                            .import_verified_snapshot(&fabric_key(&key), &verified)
                            .map_err(|_| Failure::Integrity(Defect::CorruptMaterial))?;
                        if status.pending {
                            return Err(Failure::Integrity(Defect::CorruptMaterial));
                        }
                    }
                }
            }
            if degraded {
                record.interpreted = false;
                let entries: Vec<([u8; 32], Vec<u8>, Vec<u8>)> = record
                    .heads
                    .iter()
                    .zip(loaded)
                    .map(|(h, (_, artifacts, tx_bytes))| {
                        encode_artifact_pack(&artifacts).map(|pack| (h.tx, pack, tx_bytes))
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                replica.raw_material.insert(key.clone(), entries);
            }
            for hash in Self::record_object_refs(&record) {
                let count = replica.object_refs.entry(hash).or_insert(0);
                *count = count.saturating_add(1);
            }
            replica.bodies.insert(key, record);
        }
        // Declarations live in the published catalog, so reopening recovers
        // them from the same place a peer would read them.
        let mut declared_failure = false;
        crate::index::stream(&StoreNodes(&store), meta.manifest_body_root, &mut |entry| {
            if declared_failure {
                return;
            }
            match crate::manifest::ManifestEntry::decode_canonical(&entry.value) {
                Ok(published) if !published.content_refs.is_empty() => {
                    for content in &published.content_refs {
                        let count = replica.declared_content_counts.entry(*content).or_insert(0);
                        *count = count.saturating_add(1);
                    }
                    replica
                        .declared_content
                        .insert(published.key, published.content_refs);
                }
                Ok(_) => {}
                Err(_) => declared_failure = true,
            }
        })
        .map_err(|_| Failure::Integrity(Defect::Index))?;
        if declared_failure {
            return Err(Failure::Integrity(Defect::Encoding));
        }

        for IndexedReceipt { scope, object } in indexed_receipts {
            let bytes = store
                .read_object(&object)
                .map_err(|_| Failure::Integrity(Defect::Encoding))?;
            let receipt = RequestReceipt::decode_canonical(&bytes)
                .map_err(|_| Failure::Integrity(Defect::Encoding))?;
            let count = replica.object_refs.entry(object.hash).or_insert(0);
            *count = count.saturating_add(1);
            replica.receipts.insert(scope, (receipt, Some(object)));
        }
        if let Some(root) = meta.manifest_root {
            let count = replica.object_refs.entry(root.hash).or_insert(0);
            *count = count.saturating_add(1);
        }
        replica.durable = Some(store);
        // A current-format interpreted record commits a complete causal
        // closure. Verify every descriptor and protected artifact while the
        // store is being placed, not on the first historical query. Records
        // without one are accepted only on the pre-generation migration path;
        // `persist_generation_baseline` fills them before open returns.
        if replica.generation_index_root.is_some() {
            for (key, record) in &replica.bodies {
                if !record.interpreted {
                    continue;
                }
                let material = record
                    .causal
                    .as_ref()
                    .ok_or(Failure::Integrity(Defect::MissingMaterial))?;
                let _ = replica.body_from_causal_material(key, material)?;
            }
        }
        // Everything above IS the verification pass, and this is the only
        // place that can honestly stamp one. `Store::open` re-read every
        // required object and re-derived its content address; this function
        // then decoded and verified every signed transaction and opened every
        // sealed envelope it holds a key for — with no heuristic repair, so
        // arriving here means the store was whole at this instant. Nothing
        // else in the system walks the whole store, so nothing else can move
        // this forward: for a live Station it reads "when it was placed",
        // which is the truth.
        replica.verified_at_ms = Some(mechanics::wallclock::now_millis());
        // A version-2 indexed store has no ancestry index. Establish its
        // current committed state as generation zero before returning it to a
        // writer. That makes the pre-commit coordinate a Spec revision records
        // immediately queryable, while changing no World fact or Manifest.
        replica.persist_generation_baseline()?;
        Ok(replica)
    }

    fn persist_generation_baseline(&mut self) -> Result<(), Failure> {
        use crate::index::{self, IndexChange, NodeSink};

        if self.generation_index_root.is_some() || self.manifest_root_object.is_none() {
            return Ok(());
        }
        let root = self.manifest_root();
        let mut records = self.bodies.clone();
        let mut added = Vec::new();
        for (key, record) in &mut records {
            if !record.interpreted {
                record.causal = None;
                continue;
            }
            let prior = self.bodies.get(key).and_then(|body| body.causal.as_ref());
            let (material, artifacts) = self.next_causal_material(key, record, prior, &added)?;
            record.causal = Some(material);
            added.extend(artifacts);
        }
        let changed = records
            .iter()
            .map(|(key, record)| ArchivedBody {
                key: key.clone(),
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
        let indexed = IndexedGeneration {
            root,
            object: delta_ref,
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
        let meta = StoreMeta {
            format_version: STORE_META_FORMAT_VERSION,
            space: self.space.clone(),
            frontier: self.frontier,
            quota: self.quota,
            body_index_root,
            manifest_body_root: self.manifest_body_root,
            content_index_root: self.content_index_root,
            receipt_index_root: self.receipt_index_root,
            generation_index_root,
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
        added.push(delta_bytes);
        let store = self.durable.as_mut().ok_or(Failure::Poisoned)?;
        store
            .commit(
                &added,
                &[],
                journal::Index {
                    roots: &roots,
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
        Ok(())
    }

    /// Every releasable object one Body record names. Artifact objects are
    /// omitted because immutable read generations retain them beyond the live
    /// head that first introduced them.
    fn record_object_refs(record: &BodyRecord) -> Vec<[u8; 32]> {
        let mut out = Vec::with_capacity(record.heads.len());
        for head in &record.heads {
            if let Some(r) = head.transaction {
                out.push(r.hash);
            }
        }
        out
    }

    fn retain_object(&mut self, hash: [u8; 32]) {
        let count = self.object_refs.entry(hash).or_insert(0);
        *count = count.saturating_add(1);
    }

    /// Drop one reference, reporting whether that was the last.
    fn release_object(&mut self, hash: [u8; 32]) -> bool {
        match self.object_refs.get_mut(&hash) {
            Some(count) if *count > 1 => {
                *count = count.saturating_sub(1);
                false
            }
            Some(_) => {
                self.object_refs.remove(&hash);
                true
            }
            None => false,
        }
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
        let reference = record
            .head()?
            .artifacts
            .first()
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
                    .read_object(&reference)
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
                self.fabric.version(&fabric_key(key)),
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
        let Ok(seed) = self.fabric.checkpoint_seed(&fabric_key(key)) else {
            return;
        };
        let Ok(sealing_key) = self.artifact_sealing_key(record, &[]) else {
            return;
        };
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
                    },
                    envelope,
                ))
            });
            let _ = sender.send(prepared);
        });
        if permit.submit(work).is_ok() {
            if let Ok(mut jobs) = self.checkpoint_jobs.lock() {
                jobs.entry(key.clone())
                    .or_insert(PreparedCheckpoint { base, receiver });
            }
        }
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
            let artifact = this
                .fabric
                .export_checkpoint(&engine_key, &CausalVersion::empty())
                .map_err(|_| Failure::Integrity(Defect::CorruptMaterial))?;
            let version = artifact
                .result()
                .cloned()
                .unwrap_or_else(CausalVersion::empty);
            let (checkpoint, envelope) =
                this.protected_artifact(&artifact, record, pending_objects)?;
            let material = CausalMaterial {
                format_version: CAUSAL_FORMAT_VERSION,
                checkpoint,
                delta_tail: Vec::new(),
                history_root: prior.and_then(|material| material.history_root),
                history_count: prior.map_or(0, |material| material.history_count),
                version,
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
        let artifact = self
            .fabric
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
            let mut delta_tail = prior.delta_tail[covered_tail..].to_vec();
            delta_tail.push(reference);
            let material = CausalMaterial {
                format_version: CAUSAL_FORMAT_VERSION,
                checkpoint,
                delta_tail,
                history_root: prior.history_root,
                history_count: prior.history_count,
                version: result,
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
                        && self
                            .fabric
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
                    prior.and_then(|old| old.causal.as_ref()),
                    &new_objects,
                )?;
                if let Some(Some(record)) = changed.get_mut(&key) {
                    record.causal = Some(material);
                }
                new_objects.extend(artifacts);
            }
        }

        // 1. Body catalog: one index change per touched Body, plus a refcount
        //    pass that decides which objects genuinely stopped being needed.
        //    One signed transaction record covers every Body in its batch, so
        //    "the objects this Body used to name" is the wrong removal set.
        let mut body_changes: Vec<IndexChange> = Vec::with_capacity(changed.len());
        for (key, record) in changed.iter() {
            let prior = self
                .bodies
                .get(key)
                .map(Self::record_object_refs)
                .unwrap_or_default();
            let now = record
                .as_ref()
                .map(Self::record_object_refs)
                .unwrap_or_default();
            for hash in &now {
                self.retain_object(*hash);
            }
            for hash in prior {
                if self.release_object(hash) && !now.contains(&hash) {
                    removed.push(hash);
                }
            }
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
        if let Some(receipt) = new_receipt {
            let bytes = receipt.encode();
            let reference = object_ref(&bytes);
            new_objects.push(bytes);
            self.retain_object(reference.hash);
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
                self.retain_object(reference.hash);
                if let Some(prior) = self.manifest_root_object {
                    if prior.hash != reference.hash && self.release_object(prior.hash) {
                        removed.push(prior.hash);
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
                    let material = next
                        .filter(|record| record.interpreted)
                        .and_then(|record| record.causal.clone());
                    archived.push(ArchivedBody {
                        key,
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
                let reference = object_ref(&bytes);
                new_objects.push(bytes);
                let indexed = IndexedGeneration {
                    root: root_object.hash,
                    object: reference,
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

        let meta = StoreMeta {
            format_version: STORE_META_FORMAT_VERSION,
            space: self.space.clone(),
            frontier: next_frontier,
            quota: self.quota,
            body_index_root,
            manifest_body_root,
            content_index_root,
            receipt_index_root,
            generation_index_root,
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

        let store = self.durable.as_mut().ok_or(Failure::Poisoned)?;
        let caller_index = journal::Index {
            roots: &roots,
            nodes: &index_nodes,
        };
        match store.commit(&added, &removed, caller_index, meta_bytes) {
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
        self.generation_index_root = generation_index_root;
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
        if self.poisoned {
            return Err(Failure::Poisoned);
        }
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
        if self.poisoned {
            return Err(Failure::Poisoned);
        }
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
        self.fabric.version(&fabric_key(key)).ok()
    }

    /// Take an anchor at a position inside a collaborative value.
    pub fn anchor(&self, key: &BodyKey, path: &str, position: u64) -> Option<fabric::Anchor> {
        self.fabric.anchor(&fabric_key(key), path, position).ok()
    }

    /// Resolve an anchor. Total, and never mutates the Body.
    pub fn resolve_anchor(
        &self,
        key: &BodyKey,
        anchor: &fabric::Anchor,
    ) -> fabric::AnchorResolution {
        self.fabric.resolve(&fabric_key(key), anchor)
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
        self.retain_object(root_ref.hash);
        let mut removed = Vec::new();
        if let Some(prior) = self.manifest_root_object {
            if prior.hash != root_ref.hash && self.release_object(prior.hash) {
                removed.push(prior.hash);
            }
        }

        let archived = if self.generation_index_root.is_none() {
            self.bodies
                .iter()
                .map(|(key, record)| ArchivedBody {
                    key: key.clone(),
                    binding: Some(record.binding.clone()),
                    stamp: record_stamp(record),
                    material: record.interpreted.then(|| record.causal.clone()).flatten(),
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
        let indexed = IndexedGeneration {
            root: root_ref.hash,
            object: delta_ref,
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

        let meta = StoreMeta {
            format_version: STORE_META_FORMAT_VERSION,
            space: self.space.clone(),
            frontier,
            quota: self.quota,
            body_index_root: self.body_index_root,
            manifest_body_root: self.manifest_body_root,
            content_index_root,
            receipt_index_root: self.receipt_index_root,
            generation_index_root,
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

        let store = self.durable.as_mut().ok_or(Failure::Poisoned)?;
        let caller_index = journal::Index {
            roots: &roots,
            nodes: &index_nodes,
        };
        match store.commit(&added, &removed, caller_index, meta_bytes) {
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
        self.manifest_root_object = Some(root_ref);
        self.frontier = frontier;

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
            return None;
        }
        let body = self.fabric.body_snapshot(&fabric_key(key)).ok().flatten()?;
        Some(SnapshotBody {
            binding: record.binding.clone(),
            stamp: self.body_stamp(key)?,
            body,
        })
    }

    fn replace_declared_content(&mut self, key: &BodyKey, refs: Vec<[u8; 32]>) {
        if let Some(prior) = self.declared_content.remove(key) {
            for content in prior {
                match self.declared_content_counts.get_mut(&content) {
                    Some(count) if *count > 1 => *count = count.saturating_sub(1),
                    Some(_) => {
                        self.declared_content_counts.remove(&content);
                    }
                    None => {}
                }
            }
        }
        if refs.is_empty() {
            return;
        }
        for content in &refs {
            let count = self.declared_content_counts.entry(*content).or_insert(0);
            *count = count.saturating_add(1);
        }
        self.declared_content.insert(key.clone(), refs);
    }

    fn snapshot_content(&self) -> imbl::OrdMap<[u8; 32], crate::content::ContentDescriptor> {
        self.declared_content
            .values()
            .flatten()
            .filter_map(|content_id| {
                let reference = crate::content::ContentRef {
                    content_id: *content_id,
                };
                self.content_descriptor(&reference)
                    .map(|descriptor| (*content_id, descriptor))
            })
            .collect()
    }

    /// Freeze the current committed state into a thread-safe read generation.
    /// This full form is used at activation and after an incorporation whose
    /// changed set is not locally known.
    pub fn read_snapshot(&self) -> ReadSnapshot {
        let mut entries = Vec::new();
        for key in self.bodies.keys() {
            if let Some(body) = self.freeze_body(key) {
                entries.push((Arc::new(key.clone()), body));
            }
        }
        let bodies = SnapshotDirectory::from_entries(entries);
        let manifest = self.manifest_root();
        let root = if manifest == crate::transaction::NO_PARENT_ROOT {
            self.frontier.root
        } else {
            manifest
        };
        let retained_bytes_estimate = snapshot_directory_retained_estimate(&bodies);
        ReadSnapshot {
            root,
            frontier: self.frontier,
            schema_bodies: schema_body_index(&bodies),
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

    /// Publish the next read generation by replacing only touched Body paths.
    /// Persistent-map structural sharing makes this O(changed log N), rather
    /// than cloning the World or scanning every Issue after every edit.
    pub fn advance_read_snapshot(&self, prior: &ReadSnapshot, changed: &[BodyKey]) -> ReadSnapshot {
        let mut bodies = prior.bodies.clone();
        let mut schema_bodies = prior.schema_bodies.clone();
        let mut retained_bytes_estimate = prior.retained_bytes_estimate;
        let mut declared_content = prior.declared_content.clone();
        let mut content = prior.content.clone();
        let mut touched_content = BTreeSet::new();
        let mut unique: BTreeSet<&BodyKey> = BTreeSet::new();
        for key in changed {
            if !unique.insert(key) {
                continue;
            }
            let shared_key = if let Some((held, prior_body)) = prior.bodies.get_key_value(key) {
                retained_bytes_estimate = retained_bytes_estimate
                    .saturating_sub(snapshot_body_retained_estimate(prior_body));
                remove_schema_body(&mut schema_bodies, key, &prior_body.binding);
                held.clone()
            } else {
                Arc::new(key.clone())
            };
            match self.freeze_body(key) {
                Some(body) => {
                    retained_bytes_estimate = retained_bytes_estimate
                        .saturating_add(snapshot_body_retained_estimate(&body));
                    insert_schema_body(&mut schema_bodies, shared_key.clone(), &body.binding);
                    bodies.insert(shared_key, body);
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
            bodies,
            schema_bodies,
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
            bodies: prior.bodies.clone(),
            schema_bodies: prior.schema_bodies.clone(),
            declared_content: prior.declared_content.clone(),
            content: prior.content.clone(),
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
        let mut bodies = SnapshotDirectory::default();
        // Each archived Material is a complete causal closure. Walking from
        // the target toward its baseline, the first entry for a Body is the
        // only one the requested generation can observe; reconstructing every
        // superseded intermediate value would make a hot Body quadratic in
        // its ancestry length.
        let mut resolved = BTreeSet::new();
        for delta in &deltas {
            for archived in &delta.changed {
                if !resolved.insert(archived.key.clone()) {
                    continue;
                }
                if let (Some(binding), Some(material)) = (&archived.binding, &archived.material) {
                    let body = self.body_from_causal_material(&archived.key, material)?;
                    bodies.insert(
                        Arc::new(archived.key.clone()),
                        SnapshotBody {
                            binding: binding.clone(),
                            stamp: archived.stamp.clone(),
                            body,
                        },
                    );
                }
            }
        }
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
            schema_bodies: schema_body_index(&bodies),
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
        match self.receipts.get(&key) {
            None => Ok(None),
            Some((r, _)) if &r.payload_hash == payload_hash => Ok(Some(r.clone())),
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
        if self.poisoned {
            return Err(Failure::Poisoned);
        }
        let receipt = self.apply_ops(request_label, ops)?;
        // Track minimal body records so bindings/tombstones behave uniformly.
        self.update_records_unattributed(ops)?;
        self.frontier = advance(self.frontier, receipt.causal().as_bytes());
        Ok(self.frontier)
    }

    /// Prepare a request under its persistent-idempotency scope without
    /// publishing it. Identical replay returns the original receipt without
    /// opening a candidate. A fresh request exclusively borrows this Replica
    /// through [`PreparedAction`] until the caller validates and finalizes it,
    /// or drops it to roll back.
    #[allow(clippy::too_many_arguments)]
    pub fn prepare_action<'a>(
        &'a mut self,
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
    ) -> Result<PreparedActionOutcome<'a>, Failure> {
        if self.poisoned {
            return Err(Failure::Poisoned);
        }
        if let Some(receipt) =
            self.lookup_action(ctx.space, world, device, request, payload_hash)?
        {
            return Ok(PreparedActionOutcome::Replayed(receipt));
        }
        if effect.len() > crate::receipt::MAX_EFFECT_BYTES {
            return Err(Failure::EffectTooLarge);
        }
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
            return Ok(PreparedActionOutcome::Prepared(PreparedAction {
                replica: self,
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
                match self.fabric.export_body(&fabric_key(key)) {
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
                            heads: vec![BodyHead {
                                tx: [0u8; 32],
                                descriptor_hash: [0u8; 32],
                                tx_commitment: [0u8; 32],
                                artifacts: Vec::new(),
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
                            let prior = self.bodies.get(key).and_then(|body| body.causal.as_ref());
                            let (material, artifacts) =
                                self.next_causal_material(key, &record, prior, &new_artifacts)?;
                            let pack = encode_artifact_pack(&artifacts)?;
                            new_artifacts.extend(artifacts);
                            record.causal = Some(material.clone());
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
                    .saturating_add(u64::try_from(receipt.encode().len()).unwrap_or(u64::MAX));
                if projected > self.quota.max_space_bytes {
                    return Err(Failure::QuotaExceeded);
                }
            }

            let candidate_root =
                self.preview_manifest_root(ctx, &new_records, &declared, next_frontier)?;
            receipt.manifest_root = candidate_root;
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
            Ok(data) => Ok(PreparedActionOutcome::Prepared(PreparedAction {
                replica: self,
                state: Some(PreparedActionState::Mutation { fabric, data }),
            })),
            Err(error) => {
                if self.fabric.rollback(fabric).is_err() {
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
                prepared.finalize(ctx).map(ActionOutcome::Committed)
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
        let keys: Vec<BodyKey> = self.raw_material.keys().cloned().collect();
        let mut upgraded_keys = Vec::new();
        let mut accepted = 0u32;
        for key in &keys {
            let Some(record) = self.bodies.get(key).cloned() else {
                continue;
            };
            if record.interpreted {
                continue;
            }
            let Some(retained) = self.raw_material.get(key).cloned() else {
                continue;
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
                    (MUTATION_ATOMIC, Some(current)) => {
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
                if descriptor.mutation_model == MUTATION_ATOMIC
                    && Some(descriptor.resulting_frontier) != chain
                {
                    continue;
                }
                let mut applied = false;
                for artifact in artifacts {
                    applied |= self
                        .fabric
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
            upgraded.causal = if opened.len() == 1 {
                opened
                    .first()
                    .map(|(_, descriptor, _)| descriptor.material.clone())
            } else {
                None
            };
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
                            (MUTATION_ATOMIC, Some(chain)) => {
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
                            (MUTATION_ATOMIC, Some(_)) => true,
                            (MUTATION_COLLABORATIVE, Some(_)) => false,
                            _ => false,
                        };
                    let chain = match descriptor.mutation_model {
                        MUTATION_ATOMIC => descriptor.resulting_frontier,
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
                            heads: vec![BodyHead {
                                tx: transaction.id(),
                                descriptor_hash: descriptor_hash(descriptor),
                                tx_commitment: staged_commitment,
                                artifacts: descriptor
                                    .artifact_refs()
                                    .map(|reference| Object {
                                        hash: reference.hash,
                                        len: reference.len,
                                    })
                                    .collect(),
                                transaction: None,
                                artifact_bytes: descriptor
                                    .artifact_refs()
                                    .fold(0u64, |sum, reference| sum.saturating_add(reference.len)),
                                tx_len: u64::try_from(transaction_bytes.len()).unwrap_or(u64::MAX),
                            }],
                            causal: fast_forward.then(|| descriptor.material.clone()),
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
                        (MUTATION_ATOMIC, Some(current))
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
                            heads: vec![BodyHead {
                                tx: transaction.id(),
                                descriptor_hash: descriptor_hash(descriptor),
                                tx_commitment: tx_commitment(&transaction_bytes),
                                artifacts: descriptor
                                    .artifact_refs()
                                    .map(|reference| Object {
                                        hash: reference.hash,
                                        len: reference.len,
                                    })
                                    .collect(),
                                transaction: None,
                                artifact_bytes: descriptor
                                    .artifact_refs()
                                    .fold(0u64, |sum, reference| sum.saturating_add(reference.len)),
                                tx_len: u64::try_from(transaction_bytes.len()).unwrap_or(u64::MAX),
                            }],
                            causal: Some(descriptor.material.clone()),
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
                        match self
                            .fabric
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
                        let current =
                            self.fabric.version(&fabric_key(&change.key)).map_err(|_| {
                                Failure::Engine(EngineFailure::Invalid(
                                    fabric::commit::Invalid::Import,
                                ))
                            })?;
                        if !matches!(
                            self.fabric.relation(
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
                    for reference in &change.record.head()?.artifacts {
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
                    head.artifacts = descriptor
                        .artifact_refs()
                        .map(|reference| Object {
                            hash: reference.hash,
                            len: reference.len,
                        })
                        .collect();
                    head.transaction = Some(object_ref(&tx_bytes));
                    head.tx_commitment = tx_commitment(&tx_bytes);
                    head.artifact_bytes = head
                        .artifacts
                        .iter()
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
        for (key, record) in &self.bodies {
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
            .map(|head| (head.tx_commitment, head.artifacts.clone()))
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
        for (key, record) in &self.bodies {
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
                        let mut artifacts = Vec::with_capacity(head.artifacts.len());
                        for reference in &head.artifacts {
                            artifacts.push(
                                store
                                    .read_object(reference)
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
        let mut fabric_ops = Vec::with_capacity(ops.len());
        for (key, op) in ops {
            fabric_ops.push(translate(fabric_key(key), op)?);
        }
        match self
            .fabric
            .prepare(fabric::Transaction::new(request_label, fabric_ops))
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
        Ok(self.fabric.finalize(prepared))
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
            match self.fabric.export_body(&fabric_key(&key)) {
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
                        heads: vec![BodyHead {
                            tx,
                            descriptor_hash: [0u8; 32],
                            tx_commitment: [0u8; 32],
                            artifacts: Vec::new(),
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
            head.artifacts = descriptor
                .artifact_refs()
                .map(|reference| Object {
                    hash: reference.hash,
                    len: reference.len,
                })
                .collect();
            head.transaction = durable.then_some(tx_ref);
            head.tx_commitment = commitment;
            head.artifact_bytes = head
                .artifacts
                .iter()
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
        let PreparedActionState::Mutation { fabric, mut data } = state else {
            let PreparedActionState::Noop { receipt } = state else {
                unreachable!();
            };
            if self.durable.is_some() {
                self.persist_receipt_only(&receipt)?;
            } else {
                self.receipts
                    .insert(receipt.scope_key(), (receipt.clone(), None));
            }
            return Ok(receipt);
        };

        if ctx.space != &data.manifest_space
            || ctx.authority_frontier != data.manifest_authority_frontier
            || ctx.signer.signer_key() != data.manifest_signer
        {
            let _ = self.fabric.rollback(fabric);
            return Err(Failure::Illegitimate(
                "prepared action finalized with different publication authority".into(),
            ));
        }
        match &self.space {
            Some(space) if space != ctx.space => {
                let _ = self.fabric.rollback(fabric);
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
            if self.fabric.rollback(fabric).is_err() {
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
        let _ = self.fabric.finalize(fabric);
        for key in checkpoint_candidates {
            self.schedule_checkpoint_if_hot(&key);
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
        self.receipts.insert(
            receipt.scope_key(),
            (receipt.clone(), Some(object_ref(&receipt.encode()))),
        );
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
            self.receipts.insert(
                receipt.scope_key(),
                (receipt.clone(), Some(object_ref(bytes))),
            );
        }
        Ok(())
    }

    /// Read the committed canonical bytes of an atomic Body, if present and
    /// interpreted (an opaque Body reads as absent).
    pub fn read(&self, key: &BodyKey) -> Option<Vec<u8>> {
        self.fabric.read(&fabric_key(key))
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
        self.fabric.read_collaborative(&fabric_key(key))
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
    let references: BTreeSet<([u8; 32], u64)> = descriptor
        .artifact_refs()
        .map(|reference| (reference.hash, reference.len))
        .collect();
    let mut delivered = BTreeMap::new();
    for artifact in artifacts {
        if artifact.len() > MAX_BODY_BYTES
            || artifact.len() < BODY_ENVELOPE_OVERHEAD
            || mechanics::authorization::body_epoch_id(&artifact).is_none()
        {
            return Err(Failure::Illegitimate(
                "artifact envelope has an invalid shape".into(),
            ));
        }
        let actual = object_ref(&artifact);
        if !references.contains(&(actual.hash, actual.len))
            || delivered
                .insert((actual.hash, actual.len), artifact)
                .is_some()
        {
            return Err(Failure::Illegitimate(
                "artifact does not match the signed closure".into(),
            ));
        }
    }
    let mut ordered = Vec::with_capacity(delivered.len());
    for reference in descriptor.artifact_refs() {
        match delivered.remove(&(reference.hash, reference.len)) {
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
        let current = decode_store_meta(&bytes).expect("v2 remains readable");
        assert_eq!(current.format_version, STORE_META_FORMAT_VERSION);
        assert_eq!(current.frontier, prior.frontier);
        assert_eq!(current.quota, prior.quota);
        assert!(current.generation_index_root.is_none());
    }
}
