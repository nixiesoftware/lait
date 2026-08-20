#![allow(
    clippy::arithmetic_side_effects,
    clippy::as_conversions,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "ledger replay validates bounded records; derived postcard serialization of owned semantic records is infallible"
)]
//! The authority ledger — mechanics' canonical journaled effect store and
//! materialization spine.
//!
//! One ledger per Space. It durably holds the Space's **signed authority
//! effects** — actor-plane events, ACL ops, and ceremony/space events, each a
//! [`SignedNode`] under its plane's domain — plus the sealed per-device
//! key-epoch envelopes, an [`crate::acl::ReplayCheckpoint`] materialization
//! for every committed frontier, and the [`BatchReceipt`] of every committed
//! batch. Everything commits through the semantics-free [`journal`] crate at
//! **one** linearization point per batch: verified effect objects, sealed-key
//! objects, the resulting checkpoint, its receipt, and the meta index land
//! atomically or not at all — no prefix of a batch can survive an invalid
//! later record, and a crash exposes the complete old or complete new ledger.
//!
//! **Frontiers are head sets, not opaque local state.** An authority frontier
//! canonically encodes the per-plane DAG heads (sorted, deduped), so any
//! holder of the same signed history can resolve the exact effect closure a
//! remote transaction was authorized against — the foundation of historical
//! authorization: standing is always evaluated **at the referenced frontier**,
//! never against current state. A frontier whose heads are not locally held is
//! missing history (retryable), not a validation pass.
//!
//! Effects remain the semantic source of truth; checkpoints are canonical
//! durable materializations of their deterministic replay and can never
//! introduce facts absent from them. A checkpoint whose semantics version
//! predates the current replay semantics is rebuilt from the signed effects —
//! an explicit, verified recovery, never a silent cache miss.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub mod generation;

use crate::acl::{self, AclState, ReplayCheckpoint, SignedOp};
use crate::actor::{self, Directory, SignedEvent};
use crate::genesis::Genesis;
use crate::ids::{ActorId, DeviceId, SpaceId};
use crate::space::SignedSpaceEvent;
use journal::{Failure as JournalFailure, Object, Store};

/// The replay-semantics version persisted in every checkpoint. Bumping it
/// forces an explicit rebuild of all checkpoints from the signed effects.
pub const LEDGER_SEMANTICS_VERSION: u16 = 1;

/// BLAKE3 derive-key context for the frontier digest.
const FRONTIER_CONTEXT: &str = "lait.authority-frontier.v1";
/// BLAKE3 derive-key context for the batch digest.
const BATCH_CONTEXT: &str = "lait.authority-batch.v1";

/// Why a ledger operation failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// A batch record failed validation (undecodable, wrong Space binding,
    /// bad signature, unknown kind). The **whole batch** was refused; the
    /// durable ledger is unchanged.
    InvalidRecord,
    /// A referenced frontier names effects this ledger does not hold. The
    /// caller may retry once the missing history arrives.
    MissingHistory,
    /// A referenced frontier is malformed (non-canonical bytes, unknown
    /// version, unsorted or duplicate heads).
    MalformedFrontier,
    /// The durable store failed (see [`JournalFailure`]).
    Journal(JournalFailure),
    /// The durable ledger failed integrity validation on open.
    Corrupt,
}

impl Failure {
    fn invalid_record(diagnostic: impl std::fmt::Display) -> Self {
        tracing::warn!(%diagnostic, "Authority record was invalid");
        Self::InvalidRecord
    }

    fn missing_history(diagnostic: impl std::fmt::Display) -> Self {
        tracing::debug!(%diagnostic, "Authority history was unavailable");
        Self::MissingHistory
    }

    fn malformed_frontier(diagnostic: impl std::fmt::Display) -> Self {
        tracing::warn!(%diagnostic, "Authority frontier was malformed");
        Self::MalformedFrontier
    }

    fn corrupt(diagnostic: impl std::fmt::Display) -> Self {
        tracing::error!(%diagnostic, "Authority ledger integrity check failed");
        Self::Corrupt
    }
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Failure::InvalidRecord => f.write_str("invalid authority record"),
            Failure::MissingHistory => f.write_str("missing authority history"),
            Failure::MalformedFrontier => f.write_str("malformed authority frontier"),
            Failure::Journal(e) => write!(f, "authority journal: {e}"),
            Failure::Corrupt => f.write_str("authority ledger corrupt"),
        }
    }
}
impl std::error::Error for Failure {}

impl From<JournalFailure> for Failure {
    fn from(e: JournalFailure) -> Self {
        Failure::Journal(e)
    }
}

/// One replicated **authoritative** effect: a signed node on one of the three
/// mechanics authority planes. The canonical wire encoding is postcard of this
/// enum (variant tags 0/1/2 — [`CeremonyMaterial`] owns the distinct tag 3 and
/// is *not* a `Effect`: ceremony transcript traffic never enters an
/// authority frontier).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Effect {
    /// Actor-plane event (inception, device add/revoke, recovery rotate).
    Actor(SignedEvent),
    /// Membership/ACL op (add/remove/grants/epoch mint/invite revoke).
    Acl(SignedOp),
    /// A **terminal** space-authority event (`Recover` / `Rotate` / `Reshare`
    /// installation) under the Space-event signing domain — the ONLY ceremony
    /// outcome that is an authority effect. A successful transcript produces
    /// exactly one of these; proposals, rounds, custody attestations and
    /// completion progress are [`CeremonyMaterial`] and never appear here.
    SpaceAuthority(SignedSpaceEvent),
}

impl Effect {
    pub fn encode(&self) -> Vec<u8> {
        postcard::to_stdvec(self).expect("encode ledger effect")
    }

    /// Canonical decode: exact re-encode equality, so a non-canonical byte
    /// stream can never alias a canonical effect.
    pub fn decode(bytes: &[u8]) -> Result<Self, Failure> {
        let effect: Effect = postcard::from_bytes(bytes)
            .map_err(|e| Failure::invalid_record(format!("undecodable effect: {e}")))?;
        if effect.encode() != bytes {
            return Err(Failure::invalid_record("non-canonical effect encoding"));
        }
        Ok(effect)
    }

    /// The effect's content hash — its inner signed node's DAG hash.
    pub fn hash(&self) -> String {
        match self {
            Effect::Actor(n) | Effect::Acl(n) | Effect::SpaceAuthority(n) => n.hash(),
        }
    }

    /// Verify the effect's signature under its plane's domain for `space`.
    /// Each variant admits exactly ONE signing domain — a ceremony-domain node
    /// wrapped as `SpaceAuthority` (or any other cross-domain substitution)
    /// fails here, refusing the whole batch before journal mutation.
    pub fn verify(&self, space: &SpaceId) -> bool {
        match self {
            Effect::Actor(n) => n.verify_sig(actor::ACTOR_DOMAIN, space.as_str()),
            Effect::Acl(n) => n.verify_sig(acl::ACL_DOMAIN, space.as_str()),
            Effect::SpaceAuthority(n) => {
                n.verify_sig(crate::space::SPACE_EVENT_DOMAIN, space.as_str())
            }
        }
    }

    fn kind(&self) -> u8 {
        match self {
            Effect::Actor(_) => 0,
            Effect::Acl(_) => 1,
            Effect::SpaceAuthority(_) => 2,
        }
    }

    fn parents(&self) -> &[String] {
        match self {
            Effect::Actor(n) | Effect::Acl(n) | Effect::SpaceAuthority(n) => &n.parents,
        }
    }
}

/// The encoded material-class tag [`CeremonyMaterial`] leads with — distinct
/// from every [`Effect`] variant tag (0/1/2), so neither class of bytes
/// can decode as the other.
pub const CEREMONY_MATERIAL_TAG: u8 = 3;

/// One replicated **ceremony-material** record: a FROST ceremony-board node
/// (proposal, authorization, DKG/signing round, custody attestation,
/// completion/abort progress) under the ceremony signing domain.
///
/// Ceremony material shares the one crash-safe Mechanics journal and the
/// mechanics-material Contact channel with authority effects, but it is a
/// distinct tagged material class with its own bounded synchronization cursor:
/// it never enters an [`AuthorityFrontier`], an authority checkpoint, a World
/// transaction, or an authorization receipt, and lifetime transcript traffic
/// never grows an ordinary frontier.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CeremonyMaterial {
    /// Always [`CEREMONY_MATERIAL_TAG`]; validated on decode.
    tag: u8,
    /// The signed ceremony-board node (verified ONLY under the ceremony
    /// domain — a Space-event-domain node substituted here rejects).
    pub node: SignedSpaceEvent,
}

impl CeremonyMaterial {
    pub fn new(node: SignedSpaceEvent) -> Self {
        Self {
            tag: CEREMONY_MATERIAL_TAG,
            node,
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        postcard::to_stdvec(self).expect("encode ceremony material")
    }

    /// Canonical decode: tag check plus exact re-encode equality.
    pub fn decode(bytes: &[u8]) -> Result<Self, Failure> {
        let material: CeremonyMaterial = postcard::from_bytes(bytes)
            .map_err(|e| Failure::invalid_record(format!("undecodable ceremony record: {e}")))?;
        if material.tag != CEREMONY_MATERIAL_TAG {
            return Err(Failure::invalid_record(format!(
                "ceremony record carries material-class tag {} (expected {CEREMONY_MATERIAL_TAG})",
                material.tag
            )));
        }
        if material.encode() != bytes {
            return Err(Failure::invalid_record(
                "non-canonical ceremony record encoding",
            ));
        }
        Ok(material)
    }

    /// The node's content hash.
    pub fn hash(&self) -> String {
        self.node.hash()
    }

    /// Verify under the **ceremony** signing domain only. A terminal
    /// Space-authority event smuggled into the ceremony class fails here.
    pub fn verify(&self, space: &SpaceId) -> bool {
        self.node
            .verify_sig(crate::dkg::CEREMONY_DOMAIN, space.as_str())
    }
}

/// BLAKE3 derive-key context for a ceremony compaction audit commitment.
const CEREMONY_AUDIT_CONTEXT: &str = "lait.ceremony-audit.v1";

/// The durable audit record terminal ceremony compaction leaves behind: a
/// commitment over the exact dropped packet hashes, so the terminal outcome
/// remains auditable after its transcript traffic is reclaimed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CeremonyAuditRecord {
    pub version: u16,
    pub space: SpaceId,
    /// Sorted hashes of the dropped ceremony packets.
    pub dropped: Vec<String>,
    /// The commitment over `space` + `dropped` (derive-key, domain-separated).
    pub commitment: [u8; 32],
}

impl CeremonyAuditRecord {
    fn build(space: &SpaceId, mut dropped: Vec<String>) -> Self {
        dropped.sort();
        dropped.dedup();
        let mut input = Vec::new();
        input.extend_from_slice(space.as_str().as_bytes());
        input.push(0x00);
        for h in &dropped {
            input.extend_from_slice(&(h.len() as u64).to_be_bytes());
            input.extend_from_slice(h.as_bytes());
        }
        let commitment = blake3::derive_key(CEREMONY_AUDIT_CONTEXT, &input);
        Self {
            version: 1,
            space: space.clone(),
            dropped,
            commitment,
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        postcard::to_stdvec(self).expect("encode ceremony audit")
    }

    fn decode(bytes: &[u8]) -> Result<Self, Failure> {
        postcard::from_bytes(bytes).map_err(|e| Failure::corrupt(format!("ceremony audit: {e}")))
    }
}

/// A staged sealed-key key with its decoded record and canonical bytes.
type StagedSealed = (([u8; 16], DeviceId), SealedKeyRecord, Vec<u8>);
/// A staged sealed-key index entry: key, plaintext-sealed bytes, object ref.
type StagedSealedRef = (([u8; 16], DeviceId), Vec<u8>, Object);

/// A sealed key-epoch envelope addressed to one device — distribution
/// material that rides beside the effects (its *authorization* is the signed
/// `MintEpoch` op; a forged envelope is inert because adoption checks the
/// mint's key commitment).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SealedKeyRecord {
    pub epoch: [u8; 16],
    pub device: DeviceId,
    pub sealed: Vec<u8>,
}

impl SealedKeyRecord {
    pub fn encode(&self) -> Vec<u8> {
        postcard::to_stdvec(self).expect("encode sealed key record")
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, Failure> {
        let rec: SealedKeyRecord = postcard::from_bytes(bytes)
            .map_err(|e| Failure::invalid_record(format!("undecodable sealed key: {e}")))?;
        if rec.encode() != bytes {
            return Err(Failure::invalid_record("non-canonical sealed key encoding"));
        }
        Ok(rec)
    }
}

/// The canonical head-set body an authority frontier encodes: per-plane DAG
/// heads, sorted and deduped. The encoded `version` is the wire version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct FrontierBody {
    version: u16,
    acl_heads: Vec<String>,
    actor_heads: Vec<String>,
    /// Heads of the terminal Space-authority plane (kind 2). Terminal effects
    /// are rare — one per completed recovery/elevation/reshare — so this list
    /// stays bounded; ceremony transcript traffic never appears here.
    space_authority_heads: Vec<String>,
}

impl FrontierBody {
    fn encode(&self) -> Vec<u8> {
        postcard::to_stdvec(self).expect("encode frontier")
    }

    fn decode(bytes: &[u8]) -> Result<Self, Failure> {
        let body: FrontierBody =
            postcard::from_bytes(bytes).map_err(|e| Failure::malformed_frontier(format!("{e}")))?;
        if body.version != 1 {
            return Err(Failure::malformed_frontier(format!(
                "unsupported frontier version {}",
                body.version
            )));
        }
        for list in [
            &body.acl_heads,
            &body.actor_heads,
            &body.space_authority_heads,
        ] {
            if list.windows(2).any(|w| w[0] >= w[1]) {
                return Err(Failure::malformed_frontier(
                    "frontier heads unsorted or duplicated",
                ));
            }
        }
        if body.encode() != bytes {
            return Err(Failure::malformed_frontier(
                "non-canonical frontier encoding",
            ));
        }
        Ok(body)
    }
}

/// BLAKE3 derive-key context for the checkpoint commitment.
///
/// v2, and the difference between v1 and v2 is a lesson paid for in full. v1
/// committed to the whole [`CheckpointObject`] **including the replayed
/// state**, on the reasoning that the replay is deterministic across nodes
/// holding the same closure. It is — across nodes running the same *build*.
/// It is not deterministic across builds: the state is a Rust struct whose
/// postcard bytes move whenever any field anywhere inside it changes shape,
/// and one such change (an authority op gaining a field) silently made every
/// receipt ever minted before it unverifiable by every build after it. A fresh
/// replica could join a space, incorporate its authority, and then refuse
/// every transaction in it with `Binding(Checkpoint)` — forever, on a fully
/// upgraded fleet, because the old receipts pin bytes no new build can ever
/// recompute.
const CHECKPOINT_CONTEXT: &str = "lait.authority-checkpoint.v2";

/// The canonical commitment of one materialized checkpoint: the **signed
/// closure only**, never the replayed interpretation of it.
///
/// Everything hashed here is a sorted set of hashes of signature-covered
/// canonical bytes — facts every build reconstructs identically from the same
/// ledger, forever. The replayed state is deliberately excluded: two correct
/// builds may legitimately interpret one closure differently (that is what a
/// schema migration *is*), so a commitment over the interpretation pits the
/// receipt against the reader's version rather than against the history it
/// actually claims. What the commitment must pin is "this receipt was minted
/// against exactly this closure" — and the closure is what it now pins.
/// Whether the *signer was authorized* is not this hash's job either way:
/// verification independently re-resolves the actor and re-evaluates the
/// demand against its own replay of the same closure.
fn checkpoint_commitment(cp: &CheckpointObject) -> [u8; 32] {
    #[derive(Serialize)]
    struct ClosureCommitment<'a> {
        semantics: u16,
        frontier: &'a [u8],
        effect_set: &'a [String],
        actor_events: &'a [String],
        space_events: &'a [String],
    }
    let input = ClosureCommitment {
        semantics: cp.semantics,
        frontier: &cp.frontier,
        effect_set: &cp.effect_set,
        actor_events: &cp.actor_events,
        space_events: &cp.space_events,
    };
    let bytes = postcard::to_stdvec(&input).expect("encode checkpoint commitment");
    blake3::derive_key(CHECKPOINT_CONTEXT, &bytes)
}

/// The companion facts an authorization evaluation binds. Runtime supplies
/// them; the receipt commits to every one.
pub struct AuthorizationRequest<'a> {
    pub world: &'a str,
    pub actor: &'a str,
    pub device: [u8; 32],
    pub authority_frontier: &'a [u8],
    pub parent_manifest_root: [u8; 32],
    pub implementation_id: [u8; 32],
    pub intent_digest: [u8; 32],
    pub demand: &'a [u8],
    pub effect_operations_digest: [u8; 32],
    pub body_transaction_core_digest: [u8; 32],
}

/// Why an authorization evaluation refused. A denial is a typed result and
/// never a receipt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// Standing was evaluated and found wanting. The reason names *which*
    /// question failed, because each sends the caller to a different remedy —
    /// a collapsed "denied" once told an admin holding every grant that they
    /// lacked write standing.
    Denied(DenialReason),
    /// The claimed implementation id is not active at the pinned frontier.
    ImplementationNotActive,
    /// The demand bytes are malformed/non-canonical.
    Demand(crate::demand::Invalid),
    /// Frontier resolution failed (missing history, malformed frontier, or a
    /// durable failure). Never a standing problem.
    Ledger(Failure),
}

/// Which standing question a [`Refusal::Denied`] failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DenialReason {
    /// The device resolves to no actor at the pinned frontier — admission not
    /// yet incorporated here, or membership revoked.
    DeviceUnbound,
    /// The device's actor differs from the claimed one.
    ActorMismatch,
    /// The actor resolved, but no capability grant satisfies the demand at the
    /// pinned frontier — the true "no standing for this change".
    DemandUnsatisfied,
    /// An internal precondition failed (malformed key or space bytes). Carried
    /// under `Denied` because every caller treats it as a refusal, but it must
    /// never be phrased as a grants problem.
    Internal(&'static str),
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Refusal::Denied(DenialReason::DeviceUnbound) => {
                write!(f, "the device resolves to no actor at the pinned frontier")
            }
            Refusal::Denied(DenialReason::ActorMismatch) => {
                write!(f, "the device belongs to a different actor than claimed")
            }
            Refusal::Denied(DenialReason::DemandUnsatisfied) => {
                write!(f, "demand unsatisfied — no capability grant covers it")
            }
            Refusal::Denied(DenialReason::Internal(what)) => {
                write!(f, "internal precondition failed: {what}")
            }
            Refusal::ImplementationNotActive => {
                write!(f, "implementation not active at the pinned frontier")
            }
            Refusal::Demand(e) => write!(f, "{e}"),
            Refusal::Ledger(e) => write!(f, "{e}"),
        }
    }
}
impl std::error::Error for Refusal {}

/// The exact companion coordinates a remote receipt must bind.
pub struct ReceiptExpectations<'a> {
    pub device: &'a [u8; 32],
    pub authority_frontier: &'a [u8],
    pub parent_manifest_root: &'a [u8; 32],
    pub intent_digest: &'a [u8; 32],
    pub demand: &'a [u8],
    pub effect_operations_digest: &'a [u8; 32],
    pub body_transaction_core_digest: &'a [u8; 32],
}

/// Why remote receipt verification refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Invalid {
    /// A bound field disagrees with the transaction (substitution).
    Binding(ReceiptField),
    /// The demand is not satisfied at the referenced frontier by the claimed
    /// actor (or the actor does not resolve there).
    Unsatisfied,
    /// Frontier resolution failed (missing history is retryable).
    Ledger(Failure),
}

/// Which authorization-receipt coordinate failed validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReceiptField {
    Space,
    Decision,
    Device,
    AuthorityFrontier,
    ParentManifest,
    Intent,
    Operations,
    Core,
    Demand,
    DemandDigest,
    Checkpoint,
    Implementation,
    Actor,
    Evidence,
}

impl std::fmt::Display for Invalid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Invalid::Binding(field) => write!(f, "receipt binding mismatch: {field:?}"),
            Invalid::Unsatisfied => write!(f, "demand unsatisfied at the referenced frontier"),
            Invalid::Ledger(e) => write!(f, "{e}"),
        }
    }
}
impl std::error::Error for Invalid {}

/// The digest a checkpoint/receipt keys a frontier by.
fn frontier_digest(space: &SpaceId, frontier_bytes: &[u8]) -> [u8; 32] {
    let mut input = Vec::with_capacity(space.as_str().len() + 1 + frontier_bytes.len());
    input.extend_from_slice(space.as_str().as_bytes());
    input.push(0x00);
    input.extend_from_slice(frontier_bytes);
    blake3::derive_key(FRONTIER_CONTEXT, &input)
}

/// The durable receipt of one authority-batch incorporation: the explicit
/// binding an incorporated batch proves — Space, the frontier before, the
/// frontier after, and a digest over the exact ordered canonical batch bytes.
/// A replay of the identical batch returns the identical receipt. This proves
/// **history incorporation**; it is not World authorization evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatchReceipt {
    pub space: SpaceId,
    pub prior_frontier: Vec<u8>,
    pub resulting_frontier: Vec<u8>,
    pub batch_digest: [u8; 32],
}

impl BatchReceipt {
    pub fn encode(&self) -> Vec<u8> {
        postcard::to_stdvec(self).expect("encode batch receipt")
    }
    fn decode(bytes: &[u8]) -> Result<Self, Failure> {
        postcard::from_bytes(bytes).map_err(|e| Failure::corrupt(format!("receipt: {e}")))
    }
}

/// Digest over the exact ordered canonical batch bytes: reordering,
/// substituting, or truncating the batch changes it.
pub fn batch_digest(records: &[Vec<u8>]) -> [u8; 32] {
    let mut input = Vec::new();
    input.extend_from_slice(&(records.len() as u64).to_be_bytes());
    for r in records {
        input.extend_from_slice(&(r.len() as u64).to_be_bytes());
        input.extend_from_slice(r);
    }
    blake3::derive_key(BATCH_CONTEXT, &input)
}

/// One durable checkpoint object: the frontier it materializes, the exact
/// effect closure it covers, the replay materialization with provenance, and
/// the semantics version that produced it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct CheckpointObject {
    semantics: u16,
    frontier: Vec<u8>,
    /// Sorted hashes of every effect in the frontier's closure (all planes) —
    /// the effect-set commitment.
    effect_set: Vec<String>,
    /// Sorted hashes of the actor events in the closure (the continuation
    /// precondition input).
    actor_events: Vec<String>,
    /// Sorted hashes of the terminal Space-authority events in the closure —
    /// they seed the effective bootstrap root, so a continuation is only valid
    /// while this set is unchanged.
    space_events: Vec<String>,
    replay: ReplayCheckpoint,
}

/// The ledger's opaque journal metadata: the complete index, persisted at
/// every commit's linearization point.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct LedgerMeta {
    version: u8,
    genesis: Genesis,
    /// (effect hash, kind, object)
    effects: Vec<(String, u8, Object)>,
    /// ((epoch, device), object)
    sealed: Vec<(([u8; 16], DeviceId), Object)>,
    /// (frontier digest, checkpoint object)
    checkpoints: Vec<([u8; 32], Object)>,
    /// (batch digest, receipt object)
    receipts: Vec<([u8; 32], Object)>,
    /// The current frontier's canonical bytes.
    frontier: Vec<u8>,
    /// The ceremony-material log: (sequence, node hash, object), append order.
    ceremony: Vec<(u64, String, Object)>,
    /// The next ceremony sequence — the bounded synchronization cursor.
    ceremony_next_seq: u64,
    /// Durable compaction audit records: (commitment, object).
    ceremony_audits: Vec<([u8; 32], Object)>,
}

/// A resolved view of the authority state at one frontier.
#[derive(Clone)]
pub struct StateView {
    pub acl: AclState,
    pub plane: Directory,
    pub frontier: Vec<u8>,
}

impl std::fmt::Debug for StateView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StateView")
            .field("frontier", &self.frontier)
            .finish_non_exhaustive()
    }
}

impl StateView {
    /// Whether `signer` (a raw device key) had authoring standing here.
    pub fn signer_can_write(&self, signer: &[u8; 32]) -> bool {
        let device = DeviceId::from_key_bytes(signer);
        match self.plane.actor_of_device(&device) {
            Some(actor) => self.acl.can_write(actor),
            None => false,
        }
    }
    /// Whether `signer` belonged to an admitted member (any standing).
    pub fn signer_is_member(&self, signer: &[u8; 32]) -> bool {
        let device = DeviceId::from_key_bytes(signer);
        match self.plane.actor_of_device(&device) {
            Some(actor) => self.acl.is_member(actor),
            None => false,
        }
    }
}

/// The journaled authority ledger for one Space.
pub struct Authority {
    store: Store,
    genesis: Genesis,
    /// Every held effect, by hash.
    effects: BTreeMap<String, Effect>,
    effect_refs: BTreeMap<String, (u8, Object)>,
    sealed: BTreeMap<([u8; 16], DeviceId), (Vec<u8>, Object)>,
    /// Durable checkpoints by frontier digest.
    checkpoint_refs: BTreeMap<[u8; 32], Object>,
    /// Decoded checkpoint cache (bounded).
    checkpoint_cache: BTreeMap<[u8; 32], CheckpointObject>,
    receipts: BTreeMap<[u8; 32], BatchReceipt>,
    receipt_refs: BTreeMap<[u8; 32], Object>,
    frontier: Vec<u8>,
    /// The ceremony-material log, in append (sequence) order.
    ceremony: Vec<(u64, String, SignedSpaceEvent)>,
    /// Held ceremony records by node hash → (sequence, object).
    ceremony_refs: BTreeMap<String, (u64, Object)>,
    /// The next ceremony sequence to assign (the bounded cursor).
    ceremony_next_seq: u64,
    /// Durable compaction audit records: (commitment, object), oldest first.
    ceremony_audits: Vec<([u8; 32], Object)>,
    /// The replay-semantics version this handle materializes at (the crate
    /// const in production; parameterized so the explicit rebuild path is
    /// testable).
    semantics: u16,
}

/// Bounded decoded-checkpoint cache size (durable checkpoints remain loadable
/// from their objects; this only bounds memory).
const CHECKPOINT_CACHE_MAX: usize = 64;

impl Authority {
    /// Create a fresh ledger for a Space at `root` (fails if one exists).
    pub fn create(root: impl Into<PathBuf>, genesis: Genesis) -> Result<Self, Failure> {
        let root = root.into();
        let store = Store::open(&root)?;
        if store.manifest().is_some() {
            return Err(Failure::corrupt("a ledger already exists at this root"));
        }
        let mut ledger = Self {
            store,
            genesis,
            effects: BTreeMap::new(),
            effect_refs: BTreeMap::new(),
            sealed: BTreeMap::new(),
            checkpoint_refs: BTreeMap::new(),
            checkpoint_cache: BTreeMap::new(),
            receipts: BTreeMap::new(),
            receipt_refs: BTreeMap::new(),
            frontier: Vec::new(),
            ceremony: Vec::new(),
            ceremony_refs: BTreeMap::new(),
            ceremony_next_seq: 0,
            ceremony_audits: Vec::new(),
            semantics: LEDGER_SEMANTICS_VERSION,
        };
        // Commit the empty-frontier baseline: genesis-only state, materialized.
        ledger.commit_batch(&[], &[])?;
        Ok(ledger)
    }

    /// Open an existing ledger, verifying the complete index. Every checkpoint
    /// whose semantics version is stale is discarded (rebuilt lazily from the
    /// signed effects — an explicit verified recovery, not a silent miss).
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, Failure> {
        Self::open_expecting_semantics(root, LEDGER_SEMANTICS_VERSION)
    }

    /// [`Authority::open`] at an explicit semantics version — the test
    /// seam proving the semantics-version rebuild is a verified recovery from
    /// the signed effects, never a silent cache miss.
    #[doc(hidden)]
    pub fn open_expecting_semantics(
        root: impl Into<PathBuf>,
        semantics: u16,
    ) -> Result<Self, Failure> {
        let store = Store::open(root)?;
        let meta_bytes = store
            .caller_meta()?
            .ok_or_else(|| Failure::corrupt("no committed ledger at this root"))?;
        let meta: LedgerMeta = postcard::from_bytes(&meta_bytes)
            .map_err(|e| Failure::corrupt(format!("ledger meta: {e}")))?;
        if meta.version != 1 {
            return Err(Failure::corrupt(format!(
                "unsupported ledger meta version {}",
                meta.version
            )));
        }
        let mut effects = BTreeMap::new();
        let mut effect_refs = BTreeMap::new();
        for (hash, kind, obj) in &meta.effects {
            let bytes = store.read_object(obj)?;
            let effect = Effect::decode(&bytes)
                .map_err(|e| Failure::corrupt(format!("stored effect {hash}: {e}")))?;
            if effect.hash() != *hash || effect.kind() != *kind {
                return Err(Failure::corrupt(format!(
                    "stored effect {hash} fails its index binding"
                )));
            }
            // Stored effects were verified at ingest; re-verify on open so a
            // corrupted-but-decodable object cannot slip standing forward.
            if !effect.verify(&meta.genesis.space_id) {
                return Err(Failure::corrupt(format!(
                    "stored effect {hash} fails signature verification"
                )));
            }
            effects.insert(hash.clone(), effect);
            effect_refs.insert(hash.clone(), (*kind, *obj));
        }
        let mut sealed = BTreeMap::new();
        for (key, obj) in &meta.sealed {
            let bytes = store.read_object(obj)?;
            let rec = SealedKeyRecord::decode(&bytes)
                .map_err(|e| Failure::corrupt(format!("sealed key: {e}")))?;
            if rec.epoch != key.0 || rec.device != key.1 {
                return Err(Failure::corrupt("sealed key fails its index binding"));
            }
            sealed.insert(key.clone(), (rec.sealed, *obj));
        }
        let mut receipts = BTreeMap::new();
        let mut receipt_refs = BTreeMap::new();
        for (digest, obj) in &meta.receipts {
            let bytes = store.read_object(obj)?;
            let receipt = BatchReceipt::decode(&bytes)?;
            if receipt.batch_digest != *digest {
                return Err(Failure::corrupt("receipt fails its index binding"));
            }
            receipts.insert(*digest, receipt);
            receipt_refs.insert(*digest, *obj);
        }
        let mut checkpoint_refs = BTreeMap::new();
        for (digest, obj) in &meta.checkpoints {
            // Verify readability + semantics version now; decode lazily later.
            let bytes = store.read_object(obj)?;
            // A checkpoint is a cache of the signed effects' deterministic
            // replay — it introduces no fact the effects do not already carry.
            // So an unreadable one is a cache miss, not a corrupt ledger, and
            // it is dropped from the index exactly like a stale-semantics one.
            //
            // Failing the open here instead would make any layout change to
            // `ReplayCheckpoint` — or to anything it holds, such as `AclState`
            // or `PolicyPass` — unopenable for every store already carrying a
            // checkpoint, with every effect intact and the state fully
            // rebuildable. The semantics version cannot rescue that case: the
            // whole struct is decoded before `semantics` is ever compared, so a
            // layout change that lands without a version bump is undecodable
            // rather than merely stale.
            match postcard::from_bytes::<CheckpointObject>(&bytes) {
                Ok(cp) if cp.semantics == semantics => {
                    checkpoint_refs.insert(*digest, *obj);
                }
                // A stale-semantics checkpoint is dropped from the index: state
                // is rebuilt from the signed effects on demand.
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!(
                        diagnostic = %e,
                        "Discarding an undecodable authority checkpoint; \
                         rebuilding it from the signed effects"
                    );
                }
            }
        }
        let mut ceremony: Vec<(u64, String, SignedSpaceEvent)> = Vec::new();
        let mut ceremony_refs = BTreeMap::new();
        for (seq, hash, obj) in &meta.ceremony {
            let bytes = store.read_object(obj)?;
            let material = CeremonyMaterial::decode(&bytes)
                .map_err(|e| Failure::corrupt(format!("stored ceremony record: {e}")))?;
            if material.hash() != *hash {
                return Err(Failure::corrupt(format!(
                    "stored ceremony record {hash} fails its index binding"
                )));
            }
            if !material.verify(&meta.genesis.space_id) {
                return Err(Failure::corrupt(format!(
                    "stored ceremony record {hash} fails ceremony-domain verification"
                )));
            }
            ceremony.push((*seq, hash.clone(), material.node));
            ceremony_refs.insert(hash.clone(), (*seq, *obj));
        }
        ceremony.sort_by_key(|(seq, _, _)| *seq);
        if ceremony
            .iter()
            .any(|(seq, _, _)| *seq >= meta.ceremony_next_seq)
        {
            return Err(Failure::corrupt("ceremony log sequence exceeds its cursor"));
        }
        let mut ceremony_audits = Vec::new();
        for (commitment, obj) in &meta.ceremony_audits {
            let bytes = store.read_object(obj)?;
            let audit = CeremonyAuditRecord::decode(&bytes)?;
            if audit.commitment != *commitment
                || CeremonyAuditRecord::build(&meta.genesis.space_id, audit.dropped.clone())
                    .commitment
                    != *commitment
            {
                return Err(Failure::corrupt(
                    "ceremony audit record fails its commitment binding",
                ));
            }
            ceremony_audits.push((*commitment, *obj));
        }
        let frontier = meta.frontier.clone();
        let genesis = meta.genesis.clone();
        let mut ledger = Self {
            store,
            genesis,
            effects,
            effect_refs,
            sealed,
            checkpoint_refs,
            checkpoint_cache: BTreeMap::new(),
            receipts,
            receipt_refs,
            frontier,
            ceremony,
            ceremony_refs,
            ceremony_next_seq: meta.ceremony_next_seq,
            ceremony_audits,
            semantics,
        };
        // The current frontier must be materializable (rebuilds if stale).
        ledger.checkpoint_for(&ledger.frontier.clone())?;
        Ok(ledger)
    }

    /// Test seam: attach a fault injector to the underlying journal.
    #[cfg(any(test, feature = "fault-injection"))]
    pub fn with_fault_injector(mut self, injector: Box<dyn Fn(&str) -> bool + Send>) -> Self {
        self.store.set_fault_injector(injector);
        self
    }

    /// Test seam for the detached Journal maintenance boundary. Production
    /// open deliberately performs no payload-directory sweep; corruption
    /// tests invoke the same collector explicitly before claiming that every
    /// remaining object is reachable authoritative state.
    #[cfg(test)]
    pub(crate) fn collect_unreachable_for_test(&self) -> Result<(), Failure> {
        self.store.collect_unreachable().map_err(Failure::from)
    }

    /// The Space this ledger serves.
    pub fn space(&self) -> &SpaceId {
        &self.genesis.space_id
    }

    /// The Space genesis.
    pub fn genesis(&self) -> &Genesis {
        &self.genesis
    }

    /// The current frontier's canonical bytes.
    pub fn frontier(&self) -> Vec<u8> {
        self.frontier.clone()
    }

    /// The journal's committed sequence — instrumentation for the
    /// zero-writes-on-read gates.
    pub fn journal_sequence(&self) -> u64 {
        self.store.manifest().map(|m| m.sequence).unwrap_or(0)
    }

    /// Every held effect's canonical bytes (the full-set export seam).
    pub fn export_effects(&self) -> Vec<Vec<u8>> {
        self.effects.values().map(|e| e.encode()).collect()
    }

    /// Every held sealed-key record's canonical bytes.
    pub fn export_sealed(&self) -> Vec<Vec<u8>> {
        self.sealed
            .iter()
            .map(|((epoch, device), (sealed, _))| {
                SealedKeyRecord {
                    epoch: *epoch,
                    device: device.clone(),
                    sealed: sealed.clone(),
                }
                .encode()
            })
            .collect()
    }

    /// The sealed envelope for `(epoch, device)`, if held.
    pub fn sealed_for(&self, epoch: &[u8; 16], device: &DeviceId) -> Option<Vec<u8>> {
        self.sealed
            .get(&(*epoch, device.clone()))
            .map(|(bytes, _)| bytes.clone())
    }

    /// Devices holding a sealed envelope for `epoch`.
    pub fn sealed_devices(&self, epoch: &[u8; 16]) -> Vec<DeviceId> {
        self.sealed
            .keys()
            .filter(|(e, _)| e == epoch)
            .map(|(_, d)| d.clone())
            .collect()
    }

    /// All held ACL ops (audit surface).
    pub fn acl_ops(&self) -> Vec<SignedOp> {
        self.effects
            .values()
            .filter_map(|e| match e {
                Effect::Acl(op) => Some(op.clone()),
                _ => None,
            })
            .collect()
    }

    /// All held actor events.
    pub fn actor_events(&self) -> Vec<SignedEvent> {
        self.effects
            .values()
            .filter_map(|e| match e {
                Effect::Actor(ev) => Some(ev.clone()),
                _ => None,
            })
            .collect()
    }

    /// All held **terminal** Space-authority events (kind 2) — the input to
    /// `space::replay`. Ceremony transcript traffic is NOT here; see
    /// [`Authority::ceremony_nodes`].
    pub fn space_authority_events(&self) -> Vec<SignedSpaceEvent> {
        self.effects
            .values()
            .filter_map(|e| match e {
                Effect::SpaceAuthority(ev) => Some(ev.clone()),
                _ => None,
            })
            .collect()
    }

    // ---- the ceremony-material class: its own log, cursor and retention ----

    /// The verified ceremony-board nodes, in append order — the bounded
    /// projection input for `dkg::parse_board`.
    pub fn ceremony_nodes(&self) -> Vec<SignedSpaceEvent> {
        self.ceremony.iter().map(|(_, _, n)| n.clone()).collect()
    }

    /// The ceremony log's bounded synchronization cursor: the next sequence
    /// number this ledger will assign. Monotone across appends, restarts and
    /// compaction (compaction never renumbers).
    pub fn ceremony_cursor(&self) -> u64 {
        self.ceremony_next_seq
    }

    /// The held ceremony records with sequence >= `cursor`, as
    /// `(sequence, canonical record bytes)` — the incremental-sync seam. A
    /// consumer resumes from its durable cursor instead of rescanning history.
    pub fn ceremony_after(&self, cursor: u64) -> Vec<(u64, Vec<u8>)> {
        self.ceremony
            .iter()
            .filter(|(seq, _, _)| *seq >= cursor)
            .map(|(seq, _, n)| (*seq, CeremonyMaterial::new(n.clone()).encode()))
            .collect()
    }

    /// Every currently retained ceremony record's canonical bytes (the Contact
    /// export seam). Post-compaction, terminal transcript traffic is absent.
    pub fn export_ceremony(&self) -> Vec<Vec<u8>> {
        self.ceremony
            .iter()
            .map(|(_, _, n)| CeremonyMaterial::new(n.clone()).encode())
            .collect()
    }

    /// The durable ceremony compaction audit commitments, oldest first.
    pub fn ceremony_audit_commitments(&self) -> Vec<[u8; 32]> {
        self.ceremony_audits.iter().map(|(c, _)| *c).collect()
    }

    /// Durably, atomically append one ceremony-material batch: canonical
    /// [`CeremonyMaterial`] records, **validated completely in memory first**
    /// under the ceremony signing domain — one undecodable, misbound, or
    /// cross-domain record refuses the whole batch with the durable ledger
    /// unchanged. Idempotent by node hash: an already-held record is skipped,
    /// and a batch with nothing new writes nothing. The ordinary authority
    /// frontier, checkpoints and receipts are untouched — ceremony material
    /// never enters them. Returns the resulting cursor.
    pub fn commit_ceremony_batch(&mut self, records: &[Vec<u8>]) -> Result<u64, Failure> {
        // 1. Validate the complete batch in memory.
        let mut fresh: Vec<(String, SignedSpaceEvent, Vec<u8>)> = Vec::new();
        for record in records {
            let material = CeremonyMaterial::decode(record)?;
            if !material.verify(&self.genesis.space_id) {
                return Err(Failure::invalid_record(format!(
                    "ceremony record {} fails ceremony-domain verification for this Space",
                    material.hash()
                )));
            }
            let hash = material.hash();
            if self.ceremony_refs.contains_key(&hash) {
                continue; // already held: idempotent
            }
            if fresh.iter().any(|(h, _, _)| h == &hash) {
                continue;
            }
            fresh.push((hash, material.node, record.clone()));
        }
        if fresh.is_empty() {
            return Ok(self.ceremony_next_seq);
        }

        // 2. Stage: assign monotone sequences and object refs.
        let prior_next = self.ceremony_next_seq;
        let mut new_objects: Vec<Vec<u8>> = Vec::new();
        for (hash, node, bytes) in &fresh {
            let obj = Object {
                hash: journal::object_content_hash(bytes),
                len: bytes.len() as u64,
            };
            let seq = self.ceremony_next_seq;
            self.ceremony_next_seq += 1;
            self.ceremony.push((seq, hash.clone(), node.clone()));
            self.ceremony_refs.insert(hash.clone(), (seq, obj));
            new_objects.push(bytes.clone());
        }
        let (mut keep, meta) = self.assemble_meta();
        let new_hashes: BTreeSet<[u8; 32]> = new_objects
            .iter()
            .map(|b| journal::object_content_hash(b))
            .collect();
        keep.retain(|r| !new_hashes.contains(&r.hash));
        let mut seen: BTreeSet<[u8; 32]> = BTreeSet::new();
        new_objects.retain(|b| seen.insert(journal::object_content_hash(b)));

        // 3. One journal commit; unwind the staged in-memory state on failure.
        if let Err(e) = self.store.commit_required_set(&new_objects, &keep, meta) {
            for (hash, _, _) in &fresh {
                self.ceremony_refs.remove(hash);
            }
            self.ceremony.retain(|(seq, _, _)| *seq < prior_next);
            self.ceremony_next_seq = prior_next;
            return Err(e.into());
        }
        Ok(self.ceremony_next_seq)
    }

    /// Compact terminal ceremony transcript traffic: durably drop the named
    /// packet hashes, recording a [`CeremonyAuditRecord`] whose commitment
    /// covers exactly the dropped set — in the SAME journal commit, so the
    /// audit commitment is durable before (never after) the material is gone.
    /// Every named hash must be held; the cursor is never renumbered. Which
    /// packets are *safe* to drop (terminal, not active, not required for
    /// validation or custody evidence) is the caller's policy — see
    /// `ceremony::terminal_compactable`.
    pub fn compact_ceremony(&mut self, drop_hashes: &[String]) -> Result<[u8; 32], Failure> {
        for h in drop_hashes {
            if !self.ceremony_refs.contains_key(h) {
                return Err(Failure::invalid_record(format!(
                    "compaction names an unheld ceremony record {h}"
                )));
            }
        }
        if drop_hashes.is_empty() {
            return Err(Failure::invalid_record("compaction with an empty drop set"));
        }
        let audit = CeremonyAuditRecord::build(&self.genesis.space_id, drop_hashes.to_vec());
        let audit_bytes = audit.encode();
        let audit_obj = Object {
            hash: journal::object_content_hash(&audit_bytes),
            len: audit_bytes.len() as u64,
        };

        // Stage: remove the dropped records, add the audit.
        let dropped: BTreeSet<&String> = drop_hashes.iter().collect();
        let prior_ceremony = self.ceremony.clone();
        let prior_refs = self.ceremony_refs.clone();
        self.ceremony.retain(|(_, h, _)| !dropped.contains(h));
        for h in &dropped {
            self.ceremony_refs.remove(*h);
        }
        self.ceremony_audits.push((audit.commitment, audit_obj));
        let (mut keep, meta) = self.assemble_meta();
        keep.retain(|r| r.hash != audit_obj.hash);

        if let Err(e) = self.store.commit_required_set(&[audit_bytes], &keep, meta) {
            self.ceremony = prior_ceremony;
            self.ceremony_refs = prior_refs;
            self.ceremony_audits.pop();
            return Err(e.into());
        }
        Ok(audit.commitment)
    }

    /// The heads of one actor's event log — the `actor_asof` frontier an
    /// authored op embeds and the parents for the actor's next event.
    pub fn actor_heads(&self, actor: &ActorId) -> Vec<String> {
        let mine: Vec<&SignedEvent> = self
            .effects
            .values()
            .filter_map(|e| match e {
                Effect::Actor(ev) => Some(ev),
                _ => None,
            })
            .filter(|ev| {
                if ev.hash() == actor.incept_hash() {
                    return true;
                }
                postcard::from_bytes::<actor::ActorOp>(&ev.op)
                    .ok()
                    .and_then(|op| op.actor().cloned())
                    .is_some_and(|a| &a == actor)
            })
            .collect();
        let mut is_parent = HashSet::new();
        for e in &mine {
            for p in &e.parents {
                is_parent.insert(p.clone());
            }
        }
        let mut heads: Vec<String> = mine
            .iter()
            .map(|e| e.hash())
            .filter(|h| !is_parent.contains(h))
            .collect();
        heads.sort();
        heads
    }

    /// The ACL DAG heads (the parents a newly authored op names).
    pub fn acl_heads(&self) -> Vec<String> {
        self.plane_heads(1)
    }

    fn plane_heads(&self, kind: u8) -> Vec<String> {
        let mut hashes: BTreeSet<String> = BTreeSet::new();
        let mut referenced: HashSet<String> = HashSet::new();
        for (h, e) in &self.effects {
            if e.kind() != kind {
                continue;
            }
            hashes.insert(h.clone());
            for p in e.parents() {
                referenced.insert(p.clone());
            }
        }
        hashes
            .into_iter()
            .filter(|h| !referenced.contains(h))
            .collect()
    }

    fn current_frontier_body(&self) -> FrontierBody {
        FrontierBody {
            version: 1,
            acl_heads: self.plane_heads(1),
            actor_heads: self.plane_heads(0),
            space_authority_heads: self.plane_heads(2),
        }
    }

    /// The current materialized ACL state (at the current frontier).
    pub fn acl_state(&mut self) -> Result<AclState, Failure> {
        let frontier = self.frontier.clone();
        Ok(self.checkpoint_for(&frontier)?.replay.state)
    }

    /// The current actor plane (over all held actor events).
    pub fn actor_plane(&self) -> Directory {
        actor::replay(&self.genesis.space_id, &self.actor_events())
    }

    /// Resolve the authority state **at a referenced historical frontier**.
    /// The frontier must be canonical and every named head locally held;
    /// missing heads are [`Failure::MissingHistory`] (retryable), never a
    /// fallback to current state.
    pub fn state_at(&mut self, frontier_bytes: &[u8]) -> Result<StateView, Failure> {
        let cp = self.checkpoint_for(frontier_bytes)?;
        let plane_events: Vec<SignedEvent> = cp
            .actor_events
            .iter()
            .filter_map(|h| match self.effects.get(h) {
                Some(Effect::Actor(ev)) => Some(ev.clone()),
                _ => None,
            })
            .collect();
        Ok(StateView {
            acl: cp.replay.state,
            plane: actor::replay(&self.genesis.space_id, &plane_events),
            frontier: frontier_bytes.to_vec(),
        })
    }

    /// Whether `signer` had authoring standing at the referenced frontier —
    /// the historical-authorization seam Replica consults. Errors (malformed
    /// frontier, missing history) are `false` at this boolean seam; callers
    /// needing the distinction use [`Authority::state_at`].
    pub fn signer_authorized_at(&mut self, signer: &[u8; 32], frontier_bytes: &[u8]) -> bool {
        self.state_at(frontier_bytes)
            .map(|view| view.signer_can_write(signer))
            .unwrap_or(false)
    }

    /// The active World implementation id at a referenced frontier.
    pub fn active_implementation_at(
        &mut self,
        frontier_bytes: &[u8],
        world: &str,
    ) -> Result<Option<[u8; 32]>, Failure> {
        let cp = self.checkpoint_for(frontier_bytes)?;
        Ok(cp.replay.state.active_implementation(world))
    }

    /// The canonical commitment of the materialized checkpoint at a frontier —
    /// deterministic across every node holding the same effect closure.
    pub fn checkpoint_commitment_at(&mut self, frontier_bytes: &[u8]) -> Result<[u8; 32], Failure> {
        let cp = self.checkpoint_for(frontier_bytes)?;
        Ok(checkpoint_commitment(&cp))
    }

    /// Derive the deterministic [`AuthorizationReceipt`] for a demand at a
    /// pinned frontier, or a typed denial. This is the ONLY constructor of
    /// World-authorization evidence: evaluation runs against the materialized
    /// checkpoint (journaled first if this frontier was not yet
    /// materialized), the canonical witness is selected per the frozen rules,
    /// and every companion coordinate is bound in.
    #[allow(clippy::too_many_arguments)]
    pub fn authorize(
        &mut self,
        request: &AuthorizationRequest<'_>,
    ) -> Result<crate::demand::AuthorizationReceipt, Refusal> {
        let demand = crate::demand::AuthorizationDemand::decode_canonical(request.demand)
            .map_err(Refusal::Demand)?;
        let cp = self
            .checkpoint_for(request.authority_frontier)
            .map_err(Refusal::Ledger)?;
        // Resolve the device to its actor AT the pinned frontier.
        let view = self
            .state_at(request.authority_frontier)
            .map_err(Refusal::Ledger)?;
        let device = DeviceId::from_key_bytes(&request.device);
        let actor = view
            .plane
            .actor_of_device(&device)
            .cloned()
            .ok_or(Refusal::Denied(DenialReason::DeviceUnbound))?;
        if actor.as_str() != request.actor {
            return Err(Refusal::Denied(DenialReason::ActorMismatch));
        }
        // The implementation id must be active at the pinned frontier.
        match cp.replay.state.active_implementation(request.world) {
            Some(active) if active == request.implementation_id => {}
            _ => return Err(Refusal::ImplementationNotActive),
        }
        let witness = cp
            .replay
            .state
            .evaluate_demand(&actor, &demand)
            .ok_or(Refusal::Denied(DenialReason::DemandUnsatisfied))?;
        Ok(crate::demand::AuthorizationReceipt {
            space: self.genesis.space_id.as_str().to_string(),
            world: request.world.to_string(),
            actor: actor.as_str().to_string(),
            device: request.device,
            authority_frontier: request.authority_frontier.to_vec(),
            authority_checkpoint_commitment: checkpoint_commitment(&cp),
            policy_evidence_digest: crate::demand::policy_evidence_digest(&witness),
            parent_manifest_root: request.parent_manifest_root,
            implementation_id: request.implementation_id,
            intent_digest: request.intent_digest,
            demand_digest: demand.digest().map_err(Refusal::Demand)?,
            effect_operations_digest: request.effect_operations_digest,
            body_transaction_core_digest: request.body_transaction_core_digest,
            decision: 1,
        })
    }

    /// Verify a remote transaction's authorization receipt against historical
    /// Mechanics state — **no World callback runs**. Recomputes the actor
    /// resolution, checkpoint commitment, implementation activation, demand
    /// evaluation, and witness digest at the receipt's referenced frontier,
    /// and requires every binding to the supplied companion coordinates.
    pub fn verify_receipt(
        &mut self,
        receipt: &crate::demand::AuthorizationReceipt,
        expectations: &ReceiptExpectations<'_>,
    ) -> Result<(), Invalid> {
        if receipt.space != self.genesis.space_id.as_str() {
            return Err(Invalid::Binding(ReceiptField::Space));
        }
        if receipt.decision != 1 {
            return Err(Invalid::Binding(ReceiptField::Decision));
        }
        if receipt.device != *expectations.device {
            return Err(Invalid::Binding(ReceiptField::Device));
        }
        if receipt.authority_frontier != expectations.authority_frontier {
            return Err(Invalid::Binding(ReceiptField::AuthorityFrontier));
        }
        if receipt.parent_manifest_root != *expectations.parent_manifest_root {
            return Err(Invalid::Binding(ReceiptField::ParentManifest));
        }
        if receipt.intent_digest != *expectations.intent_digest {
            return Err(Invalid::Binding(ReceiptField::Intent));
        }
        if receipt.effect_operations_digest != *expectations.effect_operations_digest {
            return Err(Invalid::Binding(ReceiptField::Operations));
        }
        if receipt.body_transaction_core_digest != *expectations.body_transaction_core_digest {
            return Err(Invalid::Binding(ReceiptField::Core));
        }
        let demand = crate::demand::AuthorizationDemand::decode_canonical(expectations.demand)
            .map_err(|_| Invalid::Binding(ReceiptField::Demand))?;
        if receipt.demand_digest
            != demand
                .digest()
                .map_err(|_| Invalid::Binding(ReceiptField::Demand))?
        {
            return Err(Invalid::Binding(ReceiptField::DemandDigest));
        }
        let cp = self
            .checkpoint_for(&receipt.authority_frontier)
            .map_err(Invalid::Ledger)?;
        if checkpoint_commitment(&cp) != receipt.authority_checkpoint_commitment {
            return Err(Invalid::Binding(ReceiptField::Checkpoint));
        }
        match cp.replay.state.active_implementation(&receipt.world) {
            Some(active) if active == receipt.implementation_id => {}
            _ => return Err(Invalid::Binding(ReceiptField::Implementation)),
        }
        let view = self
            .state_at(&receipt.authority_frontier)
            .map_err(Invalid::Ledger)?;
        let device = DeviceId::from_key_bytes(&receipt.device);
        let actor = view
            .plane
            .actor_of_device(&device)
            .cloned()
            .ok_or(Invalid::Unsatisfied)?;
        if actor.as_str() != receipt.actor {
            return Err(Invalid::Binding(ReceiptField::Actor));
        }
        let witness = cp
            .replay
            .state
            .evaluate_demand(&actor, &demand)
            .ok_or(Invalid::Unsatisfied)?;
        if crate::demand::policy_evidence_digest(&witness) != receipt.policy_evidence_digest {
            return Err(Invalid::Binding(ReceiptField::Evidence));
        }
        Ok(())
    }

    /// The closure of a frontier: every effect hash reachable from its heads,
    /// per plane. Missing heads or parents are [`Failure::MissingHistory`].
    fn closure_of(&self, body: &FrontierBody) -> Result<BTreeSet<String>, Failure> {
        let mut out: BTreeSet<String> = BTreeSet::new();
        let mut stack: Vec<&String> = Vec::new();
        for (heads, kind) in [
            (&body.acl_heads, 1u8),
            (&body.actor_heads, 0u8),
            (&body.space_authority_heads, 2u8),
        ] {
            for h in heads {
                match self.effects.get(h) {
                    Some(e) if e.kind() == kind => stack.push(h),
                    Some(_) => {
                        return Err(Failure::malformed_frontier(format!(
                            "head {h} names an effect on another plane"
                        )))
                    }
                    None => {
                        return Err(Failure::missing_history(format!(
                            "frontier head {h} is not held"
                        )))
                    }
                }
            }
        }
        while let Some(h) = stack.pop() {
            if !out.insert(h.clone()) {
                continue;
            }
            let effect = &self.effects[h];
            for p in effect.parents() {
                match self.effects.get(p) {
                    Some(_) => stack.push(p),
                    None => {
                        return Err(Failure::missing_history(format!(
                            "effect {h} names an unheld parent {p}"
                        )))
                    }
                }
            }
        }
        Ok(out)
    }

    /// Load or build (and durably journal) the checkpoint for a frontier.
    fn checkpoint_for(&mut self, frontier_bytes: &[u8]) -> Result<CheckpointObject, Failure> {
        let body = FrontierBody::decode(frontier_bytes)?;
        let digest = frontier_digest(&self.genesis.space_id, frontier_bytes);
        if let Some(cp) = self.checkpoint_cache.get(&digest) {
            return Ok(cp.clone());
        }
        if let Some(obj) = self.checkpoint_refs.get(&digest) {
            let bytes = self.store.read_object(obj)?;
            // Same rule as `open`: a checkpoint that will not decode is a cache
            // miss, so fall through and rebuild it from the signed effects
            // rather than refusing to serve a frontier the effects can answer.
            match postcard::from_bytes::<CheckpointObject>(&bytes) {
                Ok(cp) if cp.semantics == self.semantics && cp.frontier == frontier_bytes => {
                    self.cache_checkpoint(digest, cp.clone());
                    return Ok(cp);
                }
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!(
                        diagnostic = %e,
                        "Discarding an undecodable authority checkpoint; \
                         rebuilding it from the signed effects"
                    );
                }
            }
        }
        // Build from the signed effects at the exact closure.
        let cp = self.build_checkpoint(&body, frontier_bytes)?;
        // A newly proven historical frontier is journaled before any receipt
        // is issued on top of it — unless it is the current frontier being
        // rebuilt during open (journaled by the next commit).
        self.persist_checkpoint(&cp)?;
        self.cache_checkpoint(digest, cp.clone());
        Ok(cp)
    }

    fn cache_checkpoint(&mut self, digest: [u8; 32], cp: CheckpointObject) {
        if self.checkpoint_cache.len() >= CHECKPOINT_CACHE_MAX {
            // Bounded: evict the smallest key (deterministic, cheap).
            let evict = self.checkpoint_cache.keys().next().copied();
            if let Some(k) = evict {
                self.checkpoint_cache.remove(&k);
            }
        }
        self.checkpoint_cache.insert(digest, cp);
    }

    /// Replay the closure of `body` into a checkpoint object. Uses the
    /// strict-descendant continuation from the best durable ancestor
    /// checkpoint when its preconditions hold; falls back to complete replay.
    ///
    /// The **effective bootstrap root** seeds the ACL replay: the terminal
    /// Space-authority events in the closure replay to a `RootState`, and a
    /// `Recover` replaces the genesis root exactly as the space plane
    /// specifies. Continuation is only valid while the closure's space-event
    /// set is unchanged (a terminal effect re-seeds the root, so the suffix
    /// rule no longer applies).
    fn build_checkpoint(
        &mut self,
        body: &FrontierBody,
        frontier_bytes: &[u8],
    ) -> Result<CheckpointObject, Failure> {
        let closure = self.closure_of(body)?;
        let acl_ops: Vec<SignedOp> = closure
            .iter()
            .filter_map(|h| match self.effects.get(h) {
                Some(Effect::Acl(op)) => Some(op.clone()),
                _ => None,
            })
            .collect();
        let actor_events: Vec<SignedEvent> = closure
            .iter()
            .filter_map(|h| match self.effects.get(h) {
                Some(Effect::Actor(ev)) => Some(ev.clone()),
                _ => None,
            })
            .collect();
        let actor_hashes: BTreeSet<String> = actor_events.iter().map(|e| e.hash()).collect();
        let space_events: Vec<SignedSpaceEvent> = closure
            .iter()
            .filter_map(|h| match self.effects.get(h) {
                Some(Effect::SpaceAuthority(ev)) => Some(ev.clone()),
                _ => None,
            })
            .collect();
        let space_hashes: BTreeSet<String> = space_events.iter().map(|e| e.hash()).collect();
        let root_state = crate::space::replay(&self.genesis, &self.genesis.space_id, &space_events);
        let effective_genesis = Genesis {
            founding_actors: root_state.root,
            ..self.genesis.clone()
        };

        // Try continuation from the current frontier's cached checkpoint (the
        // common case: a new batch extends the tip).
        let replay = self
            .try_continue(&effective_genesis, &space_hashes, &actor_events, &acl_ops)
            .unwrap_or_else(|| {
                let (cp, _) = acl::replay_checkpointed(&effective_genesis, &actor_events, &acl_ops);
                cp
            });

        Ok(CheckpointObject {
            semantics: self.semantics,
            frontier: frontier_bytes.to_vec(),
            effect_set: closure.into_iter().collect(),
            actor_events: actor_hashes.into_iter().collect(),
            space_events: space_hashes.into_iter().collect(),
            replay,
        })
    }

    /// The strict-descendant continuation attempt, from the current frontier's
    /// in-memory checkpoint. Refused when the space-event set changed: a
    /// terminal Space-authority effect re-seeds the effective root, so the
    /// prior materialization is not a valid replay prefix.
    fn try_continue(
        &self,
        effective_genesis: &Genesis,
        space_hashes: &BTreeSet<String>,
        actor_events: &[SignedEvent],
        acl_ops: &[SignedOp],
    ) -> Option<ReplayCheckpoint> {
        let digest = frontier_digest(&self.genesis.space_id, &self.frontier);
        let prior = self.checkpoint_cache.get(&digest)?;
        let prior_space: BTreeSet<String> = prior.space_events.iter().cloned().collect();
        if prior_space != *space_hashes {
            return None;
        }
        let prior_actor: BTreeSet<String> = prior.actor_events.iter().cloned().collect();
        acl::replay_continue(
            &prior.replay,
            &prior_actor,
            effective_genesis,
            actor_events,
            acl_ops,
        )
        .map(|(cp, _)| cp)
    }

    fn persist_checkpoint(&mut self, cp: &CheckpointObject) -> Result<(), Failure> {
        let digest = frontier_digest(&self.genesis.space_id, &cp.frontier);
        if self.checkpoint_refs.contains_key(&digest) {
            return Ok(());
        }
        let bytes = postcard::to_stdvec(cp).expect("encode checkpoint");
        let obj = Object {
            hash: journal::object_content_hash(&bytes),
            len: bytes.len() as u64,
        };
        self.checkpoint_refs.insert(digest, obj);
        let (mut keep, meta) = self.assemble_meta();
        // The checkpoint object is written by this commit — not carried.
        keep.retain(|r| r.hash != obj.hash);
        match self.store.commit_required_set(&[bytes], &keep, meta) {
            Ok(_) => Ok(()),
            Err(e) => {
                self.checkpoint_refs.remove(&digest);
                Err(e.into())
            }
        }
    }

    /// Replace the indexed checkpoint for `frontier_bytes` with an arbitrary
    /// payload — the test seam standing in for a checkpoint written by a build
    /// whose [`crate::acl::ReplayCheckpoint`] layout differed. The payload is
    /// stored under its true content address, so this models a structurally
    /// intact object this build cannot decode, never a damaged one: damaged
    /// bytes are caught by the journal's content-address check well before any
    /// decode is attempted, and stay an integrity failure.
    #[doc(hidden)]
    pub fn overwrite_checkpoint_payload_for_test(
        &mut self,
        frontier_bytes: &[u8],
        payload: &[u8],
    ) -> Result<(), Failure> {
        let digest = frontier_digest(&self.genesis.space_id, frontier_bytes);
        let obj = Object {
            hash: journal::object_content_hash(payload),
            len: payload.len() as u64,
        };
        self.checkpoint_refs.insert(digest, obj);
        self.checkpoint_cache.remove(&digest);
        let (mut keep, meta) = self.assemble_meta();
        keep.retain(|r| r.hash != obj.hash);
        self.store
            .commit_required_set(&[payload.to_vec()], &keep, meta)?;
        Ok(())
    }

    /// The complete meta index + keep set over everything currently indexed.
    fn assemble_meta(&self) -> (Vec<Object>, Vec<u8>) {
        let mut keep: Vec<Object> = Vec::new();
        let mut effects = Vec::new();
        for (h, (kind, obj)) in &self.effect_refs {
            effects.push((h.clone(), *kind, *obj));
            keep.push(*obj);
        }
        let mut sealed = Vec::new();
        for (key, (_, obj)) in &self.sealed {
            sealed.push((key.clone(), *obj));
            keep.push(*obj);
        }
        let mut checkpoints = Vec::new();
        for (digest, obj) in &self.checkpoint_refs {
            checkpoints.push((*digest, *obj));
            keep.push(*obj);
        }
        let mut receipts = Vec::new();
        for (digest, obj) in &self.receipt_refs {
            receipts.push((*digest, *obj));
            keep.push(*obj);
        }
        let mut ceremony = Vec::new();
        for (seq, hash, _) in &self.ceremony {
            let (_, obj) = self.ceremony_refs[hash];
            ceremony.push((*seq, hash.clone(), obj));
            keep.push(obj);
        }
        let mut ceremony_audits = Vec::new();
        for (commitment, obj) in &self.ceremony_audits {
            ceremony_audits.push((*commitment, *obj));
            keep.push(*obj);
        }
        let meta = LedgerMeta {
            version: 1,
            genesis: self.genesis.clone(),
            effects,
            sealed,
            checkpoints,
            receipts,
            frontier: self.frontier.clone(),
            ceremony,
            ceremony_next_seq: self.ceremony_next_seq,
            ceremony_audits,
        };
        keep.sort_by_key(|r| r.hash);
        keep.dedup_by_key(|r| r.hash);
        // `keep` may name objects being written in this same commit; the
        // journal validates keeps against *existing* objects, so the caller
        // must subtract new objects. assemble_meta callers handle that.
        (
            keep,
            postcard::to_stdvec(&meta).expect("encode ledger meta"),
        )
    }

    /// Durably, atomically commit one authority batch: canonical effect
    /// records plus sealed-key records, **validated completely in memory
    /// first** — one undecodable, misbound, or signature-invalid record
    /// refuses the whole batch with the durable ledger unchanged; no prefix
    /// survives an invalid later record. An exact replay of an
    /// already-committed batch returns the original receipt without a new
    /// journal write.
    pub fn commit_batch(
        &mut self,
        effect_records: &[Vec<u8>],
        sealed_records: &[Vec<u8>],
    ) -> Result<BatchReceipt, Failure> {
        // Exact-replay idempotency (effects + sealed both bind the digest).
        let mut all_records: Vec<Vec<u8>> = Vec::new();
        all_records.extend(effect_records.iter().cloned());
        all_records.extend(sealed_records.iter().cloned());
        let digest = batch_digest(&all_records);
        if let Some(receipt) = self.receipts.get(&digest) {
            return Ok(receipt.clone());
        }

        // 1. Validate the complete batch in memory.
        let mut new_effects: Vec<(String, Effect, Vec<u8>)> = Vec::new();
        for record in effect_records {
            let effect = Effect::decode(record)?;
            if !effect.verify(&self.genesis.space_id) {
                return Err(Failure::invalid_record(format!(
                    "effect {} fails signature verification for this Space",
                    effect.hash()
                )));
            }
            let hash = effect.hash();
            if self.effects.contains_key(&hash) {
                continue; // already held: idempotent
            }
            if new_effects.iter().any(|(h, _, _)| h == &hash) {
                continue;
            }
            new_effects.push((hash, effect, record.clone()));
        }
        let mut new_sealed: Vec<StagedSealed> = Vec::new();
        for record in sealed_records {
            let rec = SealedKeyRecord::decode(record)?;
            let key = (rec.epoch, rec.device.clone());
            if self.sealed.contains_key(&key) {
                continue; // first-write-wins: an existing envelope stands
            }
            if new_sealed.iter().any(|(k, _, _)| k == &key) {
                continue;
            }
            new_sealed.push((key, rec, record.clone()));
        }

        // 2. Compute the union replay + resulting frontier in memory.
        let prior_frontier = self.frontier.clone();
        for (hash, effect, _) in &new_effects {
            self.effects.insert(hash.clone(), effect.clone());
        }
        let body = self.current_frontier_body();
        let frontier_bytes = body.encode();
        let build = self.build_checkpoint(&body, &frontier_bytes);
        let checkpoint = match build {
            Ok(cp) => cp,
            Err(e) => {
                for (hash, _, _) in &new_effects {
                    self.effects.remove(hash);
                }
                return Err(e);
            }
        };

        // 3. Assemble the one journal commit: effects, sealed keys, the
        //    checkpoint, the receipt, and the meta index.
        let receipt = BatchReceipt {
            space: self.genesis.space_id.clone(),
            prior_frontier,
            resulting_frontier: frontier_bytes.clone(),
            batch_digest: digest,
        };
        let cp_digest = frontier_digest(&self.genesis.space_id, &frontier_bytes);
        let cp_bytes = postcard::to_stdvec(&checkpoint).expect("encode checkpoint");
        let receipt_bytes = receipt.encode();

        let mut new_objects: Vec<Vec<u8>> = Vec::new();
        let mut staged_effect_refs: Vec<(String, u8, Object)> = Vec::new();
        for (hash, effect, bytes) in &new_effects {
            let obj = Object {
                hash: journal::object_content_hash(bytes),
                len: bytes.len() as u64,
            };
            staged_effect_refs.push((hash.clone(), effect.kind(), obj));
            new_objects.push(bytes.clone());
        }
        let mut staged_sealed_refs: Vec<StagedSealedRef> = Vec::new();
        for (key, rec, bytes) in &new_sealed {
            let obj = Object {
                hash: journal::object_content_hash(bytes),
                len: bytes.len() as u64,
            };
            staged_sealed_refs.push((key.clone(), rec.sealed.clone(), obj));
            new_objects.push(bytes.clone());
        }
        let cp_obj = Object {
            hash: journal::object_content_hash(&cp_bytes),
            len: cp_bytes.len() as u64,
        };
        new_objects.push(cp_bytes);
        let receipt_obj = Object {
            hash: journal::object_content_hash(&receipt_bytes),
            len: receipt_bytes.len() as u64,
        };
        new_objects.push(receipt_bytes);

        // Stage the index updates, then build meta over the staged state.
        for (hash, kind, obj) in &staged_effect_refs {
            self.effect_refs.insert(hash.clone(), (*kind, *obj));
        }
        for (key, sealed, obj) in &staged_sealed_refs {
            self.sealed.insert(key.clone(), (sealed.clone(), *obj));
        }
        self.checkpoint_refs.insert(cp_digest, cp_obj);
        self.receipt_refs.insert(digest, receipt_obj);
        self.frontier = frontier_bytes.clone();
        let (mut keep, meta) = self.assemble_meta();
        // New objects are written by this commit — not carried.
        let new_hashes: BTreeSet<[u8; 32]> = new_objects
            .iter()
            .map(|b| journal::object_content_hash(b))
            .collect();
        keep.retain(|r| !new_hashes.contains(&r.hash));

        // Dedup new objects by content (a re-sent byte-identical record).
        let mut seen: BTreeSet<[u8; 32]> = BTreeSet::new();
        new_objects.retain(|b| seen.insert(journal::object_content_hash(b)));

        match self.store.commit_required_set(&new_objects, &keep, meta) {
            Ok(_) => {}
            Err(e) => {
                // Unwind the staged in-memory state: the durable ledger is
                // unchanged, so memory must match it.
                for (hash, _, _) in &new_effects {
                    self.effects.remove(hash);
                    self.effect_refs.remove(hash);
                }
                for (key, _, _) in &staged_sealed_refs {
                    self.sealed.remove(key);
                }
                self.checkpoint_refs.remove(&cp_digest);
                self.receipt_refs.remove(&digest);
                self.frontier = receipt.prior_frontier.clone();
                return Err(e.into());
            }
        }
        self.cache_checkpoint(cp_digest, checkpoint);
        self.receipts.insert(digest, receipt.clone());
        Ok(receipt)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acl::{AclAction, AclOp, Standing};
    use crate::ids::SystemUlidSource;

    fn seed(n: u8) -> [u8; 32] {
        [n; 32]
    }

    /// The checkpoint commitment is a function of the signed closure alone.
    ///
    /// This is the property whose absence broke every pre-existing receipt in
    /// the wild: v1 hashed the replayed state, whose postcard bytes move with
    /// any struct-shape change in any build, so a receipt could only ever be
    /// verified by the build generation that minted it. Two checkpoints that
    /// agree on the closure and disagree arbitrarily on the interpretation
    /// must commit identically — build drift in the replay must be invisible
    /// here, because history has to outlive builds.
    #[test]
    fn the_commitment_ignores_the_replayed_interpretation() {
        let fx = fx();
        let (replay, _) = crate::acl::replay_checkpointed(&fx.genesis, &[fx.founder_incept], &[]);
        let closure = CheckpointObject {
            semantics: LEDGER_SEMANTICS_VERSION,
            frontier: vec![7u8; 32],
            effect_set: vec!["aa".into(), "bb".into()],
            actor_events: vec!["aa".into()],
            space_events: vec![],
            replay: replay.clone(),
        };
        let mut reinterpreted = CheckpointObject {
            semantics: closure.semantics,
            frontier: closure.frontier.clone(),
            effect_set: closure.effect_set.clone(),
            actor_events: closure.actor_events.clone(),
            space_events: closure.space_events.clone(),
            replay,
        };
        reinterpreted.replay.heads = vec!["something entirely different".into()];
        reinterpreted.replay.verdicts = vec![("x".into(), true)];
        assert_eq!(
            checkpoint_commitment(&closure),
            checkpoint_commitment(&reinterpreted),
            "a replay difference reached the commitment — receipts are once again              verifiable only by the build generation that minted them"
        );
        // …and the closure itself still binds.
        let mut other_closure = closure;
        other_closure.effect_set.push("cc".into());
        assert_ne!(
            checkpoint_commitment(&other_closure),
            checkpoint_commitment(&reinterpreted),
            "the commitment must still distinguish different closures"
        );
    }

    struct Fx {
        genesis: Genesis,
        founder_seed: [u8; 32],
        founder_actor: ActorId,
        founder_incept: SignedEvent,
    }

    fn fx() -> Fx {
        let space = SpaceId::mint(&SystemUlidSource);
        let (incept, actor_id) = actor::incept_single(&seed(1), &space, [1; 16], [71; 16], None);
        Fx {
            genesis: Genesis {
                space_id: space,
                founding_actors: vec![actor_id.clone()],
                salt: [0u8; 16],
                recovery_root: [0u8; 32],
            },
            founder_seed: seed(1),
            founder_actor: actor_id,
            founder_incept: incept,
        }
    }

    fn incept_other(fx: &Fx, n: u8) -> (SignedEvent, ActorId) {
        actor::incept_single(&seed(n), &fx.genesis.space_id, [n; 16], [n + 70; 16], None)
    }

    fn signed_add(
        fx: &Fx,
        parents: Vec<String>,
        actor_asof: Vec<String>,
        target: &ActorId,
        grants: Vec<Standing>,
    ) -> SignedOp {
        acl::sign_op(
            &fx.founder_seed,
            &AclOp {
                action: AclAction::AddMember {
                    actor: target.clone(),
                    grants,
                },
                by: fx.founder_actor.clone(),
                actor_asof,
                nonce: None,
            },
            parents,
            &fx.genesis.space_id,
        )
    }

    #[test]
    fn create_open_roundtrip_and_empty_frontier() {
        let dir = tempdir();
        let fx = fx();
        let ledger = Authority::create(&dir, fx.genesis.clone()).unwrap();
        let frontier = ledger.frontier();
        drop(ledger);
        let ledger = Authority::open(&dir).unwrap();
        assert_eq!(ledger.frontier(), frontier);
        assert_eq!(ledger.space(), &fx.genesis.space_id);
        cleanup(&dir);
    }

    #[test]
    fn batch_is_atomic_no_prefix_survives_an_invalid_record() {
        let dir = tempdir();
        let fx = fx();
        let mut ledger = Authority::create(&dir, fx.genesis.clone()).unwrap();
        let good = Effect::Actor(fx.founder_incept.clone()).encode();
        let bad = vec![0xFF, 0xEE, 0xDD];
        let before = ledger.frontier();
        let before_seq = ledger.journal_sequence();
        let err = ledger.commit_batch(&[good, bad], &[]).unwrap_err();
        assert!(matches!(err, Failure::InvalidRecord));
        assert_eq!(ledger.frontier(), before, "no partial adoption");
        assert_eq!(ledger.journal_sequence(), before_seq, "no journal write");
        // Restart: still unchanged (the *durable* store was untouched).
        drop(ledger);
        let ledger = Authority::open(&dir).unwrap();
        assert_eq!(ledger.frontier(), before);
        assert!(ledger.actor_events().is_empty());
        cleanup(&dir);
    }

    #[test]
    fn exact_replay_returns_the_original_receipt() {
        let dir = tempdir();
        let fx = fx();
        let mut ledger = Authority::create(&dir, fx.genesis.clone()).unwrap();
        let batch = vec![Effect::Actor(fx.founder_incept.clone()).encode()];
        let first = ledger.commit_batch(&batch, &[]).unwrap();
        let seq = ledger.journal_sequence();
        let replay = ledger.commit_batch(&batch, &[]).unwrap();
        assert_eq!(first, replay);
        assert_eq!(ledger.journal_sequence(), seq, "replay writes nothing");
        cleanup(&dir);
    }

    #[test]
    fn historical_standing_survives_current_removal() {
        let dir = tempdir();
        let fx = fx();
        let mut ledger = Authority::create(&dir, fx.genesis.clone()).unwrap();
        let (incept2, actor2) = incept_other(&fx, 2);
        // Batch 1: founder inception + member 2 inception + AddMember(write).
        ledger
            .commit_batch(
                &[
                    Effect::Actor(fx.founder_incept.clone()).encode(),
                    Effect::Actor(incept2.clone()).encode(),
                ],
                &[],
            )
            .unwrap();
        let add = signed_add(
            &fx,
            ledger.acl_heads(),
            ledger.actor_heads(&fx.founder_actor),
            &actor2,
            vec![Standing::Write],
        );
        ledger
            .commit_batch(&[Effect::Acl(add).encode()], &[])
            .unwrap();
        let member_frontier = ledger.frontier();
        let member_key = crate::crypto::device_from_seed(&seed(2))
            .key_bytes()
            .unwrap();
        assert!(ledger.signer_authorized_at(&member_key, &member_frontier));

        // Remove member 2.
        let remove = acl::sign_op(
            &fx.founder_seed,
            &AclOp {
                action: AclAction::RemoveMember {
                    actor: actor2.clone(),
                },
                by: fx.founder_actor.clone(),
                actor_asof: ledger.actor_heads(&fx.founder_actor),
                nonce: None,
            },
            ledger.acl_heads(),
            &fx.genesis.space_id,
        );
        ledger
            .commit_batch(&[Effect::Acl(remove).encode()], &[])
            .unwrap();
        let removed_frontier = ledger.frontier();

        // Removed **currently**, still authorized **at the old frontier**.
        assert!(
            ledger.signer_authorized_at(&member_key, &member_frontier),
            "historical authorization is at the referenced frontier"
        );
        assert!(
            !ledger.signer_authorized_at(&member_key, &removed_frontier),
            "current frontier reflects the removal"
        );
        cleanup(&dir);
    }

    #[test]
    fn unauthorized_at_referenced_frontier_despite_current_standing() {
        let dir = tempdir();
        let fx = fx();
        let mut ledger = Authority::create(&dir, fx.genesis.clone()).unwrap();
        let (incept2, actor2) = incept_other(&fx, 2);
        ledger
            .commit_batch(
                &[
                    Effect::Actor(fx.founder_incept.clone()).encode(),
                    Effect::Actor(incept2.clone()).encode(),
                ],
                &[],
            )
            .unwrap();
        let before_frontier = ledger.frontier();
        let add = signed_add(
            &fx,
            ledger.acl_heads(),
            ledger.actor_heads(&fx.founder_actor),
            &actor2,
            vec![Standing::Write],
        );
        ledger
            .commit_batch(&[Effect::Acl(add).encode()], &[])
            .unwrap();
        let member_key = crate::crypto::device_from_seed(&seed(2))
            .key_bytes()
            .unwrap();
        assert!(ledger.signer_authorized_at(&member_key, &ledger.frontier().clone()));
        assert!(
            !ledger.signer_authorized_at(&member_key, &before_frontier),
            "authorized now but NOT at the referenced earlier frontier"
        );
        cleanup(&dir);
    }

    #[test]
    fn unknown_frontier_is_missing_history_not_a_pass() {
        let dir = tempdir();
        let fx = fx();
        let mut ledger = Authority::create(&dir, fx.genesis.clone()).unwrap();
        let fake = FrontierBody {
            version: 1,
            acl_heads: vec!["ab".repeat(32)],
            actor_heads: vec![],
            space_authority_heads: vec![],
        }
        .encode();
        match ledger.state_at(&fake) {
            Err(Failure::MissingHistory) => {}
            other => panic!("expected MissingHistory, got {other:?}"),
        }
        let founder_key = crate::crypto::device_from_seed(&fx.founder_seed)
            .key_bytes()
            .unwrap();
        assert!(!ledger.signer_authorized_at(&founder_key, &fake));
        cleanup(&dir);
    }

    #[test]
    fn malformed_frontiers_reject() {
        let dir = tempdir();
        let fx = fx();
        let mut ledger = Authority::create(&dir, fx.genesis).unwrap();
        for bytes in [
            vec![],
            vec![0xFF; 4],
            FrontierBody {
                version: 2,
                acl_heads: vec![],
                actor_heads: vec![],
                space_authority_heads: vec![],
            }
            .encode(),
        ] {
            match ledger.state_at(&bytes) {
                Err(Failure::MalformedFrontier) => {}
                other => panic!("expected MalformedFrontier, got {other:?}"),
            }
        }
        // Unsorted heads reject.
        let mut unsorted = FrontierBody {
            version: 1,
            acl_heads: vec!["bb".repeat(32), "aa".repeat(32)],
            actor_heads: vec![],
            space_authority_heads: vec![],
        };
        let bytes = postcard::to_stdvec(&unsorted).unwrap();
        match ledger.state_at(&bytes) {
            Err(Failure::MalformedFrontier) => {}
            other => panic!("expected MalformedFrontier for unsorted, got {other:?}"),
        }
        unsorted.acl_heads.clear();
        cleanup(&dir);
    }

    #[test]
    fn continuation_equals_complete_replay() {
        let dir = tempdir();
        let fx = fx();
        let mut ledger = Authority::create(&dir, fx.genesis.clone()).unwrap();
        let (incept2, actor2) = incept_other(&fx, 2);
        let (incept3, actor3) = incept_other(&fx, 3);
        ledger
            .commit_batch(
                &[
                    Effect::Actor(fx.founder_incept.clone()).encode(),
                    Effect::Actor(incept2).encode(),
                    Effect::Actor(incept3).encode(),
                ],
                &[],
            )
            .unwrap();
        // A chain of pure-ACL batches: these take the strict-descendant path.
        for target in [&actor2, &actor3] {
            let add = signed_add(
                &fx,
                ledger.acl_heads(),
                ledger.actor_heads(&fx.founder_actor),
                target,
                vec![Standing::Write],
            );
            ledger
                .commit_batch(&[Effect::Acl(add).encode()], &[])
                .unwrap();
        }
        // Differential: the ledger's materialized state equals the complete
        // acl::replay over the same effect sets.
        let expected = acl::replay(&fx.genesis, &ledger.actor_events(), &ledger.acl_ops());
        assert_eq!(ledger.acl_state().unwrap(), expected);
        assert!(expected.can_write(&actor2));
        assert!(expected.can_write(&actor3));
        cleanup(&dir);
    }

    #[test]
    fn reopen_after_crash_between_batches_shows_complete_state() {
        let dir = tempdir();
        let fx = fx();
        let mut ledger = Authority::create(&dir, fx.genesis.clone()).unwrap();
        ledger
            .commit_batch(&[Effect::Actor(fx.founder_incept.clone()).encode()], &[])
            .unwrap();
        let frontier = ledger.frontier();
        drop(ledger); // "crash": nothing in flight
        let mut ledger = Authority::open(&dir).unwrap();
        assert_eq!(ledger.frontier(), frontier);
        assert_eq!(ledger.actor_events().len(), 1);
        // Historical evaluation still works after reopen.
        let founder_key = crate::crypto::device_from_seed(&fx.founder_seed)
            .key_bytes()
            .unwrap();
        assert!(ledger.signer_authorized_at(&founder_key, &frontier));
        cleanup(&dir);
    }

    fn tempdir() -> PathBuf {
        let mut p = std::env::temp_dir();
        let mut raw = [0u8; 8];
        getrandom::fill(&mut raw).unwrap();
        p.push(format!("lait-ledger-test-{}", hex::encode(raw)));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn cleanup(p: &std::path::Path) {
        let _ = std::fs::remove_dir_all(p);
    }

    mod hex {
        pub fn encode(bytes: [u8; 8]) -> String {
            bytes.iter().map(|b| format!("{b:02x}")).collect()
        }
    }
}
