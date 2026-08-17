//! The World implementation contract.
//!
//! A World is an independently supplied semantic implementation under Space
//! authority. It defines Body schemas, decodes Intents/Queries, authorizes
//! within supplied Space standing, stages LAIT-owned Body operations, and
//! returns Effects/Projections. It **cannot** redefine membership, custody, key
//! legitimacy, storage, Contact, or Convergence, and receives no CRDT handle, raw
//! keys/ciphertext, files, network handles, or mutable Replica.
//!
//! World callbacks are trusted, cooperative, in-process Rust code — not a
//! sandbox. The API supplies no clock, RNG, environment, thread, file, or
//! network handle; implementations promise deterministic synchronous bounded CPU
//! work. Runtime contains an unwind-safe panic as `WorldPanicked`
//! without ending the Station.

pub mod call;

use mechanics::{
    ids::{ActorId, DeviceId},
    station::Key,
};
use replica::body::Op;
use replica::body::Schema;
use replica::body::{BodyKey, SchemaId, WorldId};
use replica::frontier::AuthorityFrontier;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

pub use crate::action::{IdempotencyKey, RequestId, SignedWorldAction, WorldActionHeader};
pub use crate::implementation::Implementation;
pub use crate::registry::{Builder, Catalog, Declaration, Refusal};
pub use crate::session::{
    AffectedWorldPublication, CommittedEffect, Conflict, Failure, Interruption, Observation,
    ObservationCursor, ObservationStream, WorldGeneration, WorldSnapshotId,
    DEFAULT_OBSERVATION_CAPACITY, MAX_OBSERVATION_CAPACITY,
};

/// A World-owned semantic rejection. These values are deterministic decisions
/// about a well-bounded request or the World's declared contract; Runtime
/// persistence, shutdown, and callback containment failures do not belong here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rejection {
    InvalidRequest,
    UnsupportedSchema,
    UnsupportedSchemaVersion,
    /// The principal was refused, and the cause says *why* — because each
    /// cause names a different remedy, and a collapsed "denied" phrased every
    /// one of them as "you lack write standing" (including read refusals and
    /// members whose grant simply had not synced to their node yet).
    Denied(DeniedCause),
    /// No World implementation is active at the pinned frontier, so no receipt
    /// can be minted for anyone — the space's problem, not the caller's
    /// standing. Its own variant because it was being reported as `Denied`,
    /// and the surface above then told an admin with full write grants that
    /// they "lack write standing" — a message that sends them to fix the wrong
    /// thing. The remedy is `world_upgrade`, and only a message that names the
    /// cause can name the remedy.
    NoActiveImplementation,
    /// Authority selected an exact implementation for which this Station has
    /// no matching executable package. Runtime must never invoke other code
    /// under that implementation's receipts.
    ImplementationUnavailable,
    Conflict,
    LimitExceeded,
    StateCorrupt,
    ContractViolation,
}

/// Why a [`Rejection::Denied`] was denied. The distinctions matter because the
/// remedies differ: syncing, asking an admin for a grant, retrying, and
/// widening a scoped grant are four different actions, and only a cause that
/// survives to the rendering surface can name the right one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeniedCause {
    /// This device does not resolve to a member at the evaluated view —
    /// admission not yet converged to this node, membership revoked, or a
    /// grant that has not arrived here yet. The remedy starts with sync, not
    /// with asking for a (possibly already-given) grant.
    NotAMember,
    /// The signed action's principal is not the docked identity (an identity
    /// or sponsorship change raced between signing and committing). Retry.
    PrincipalMismatch,
    /// The actor is a member, but no capability grant satisfies this change's
    /// demand at the pinned frontier — view-only standing, an ungranted
    /// sponsored agent, or a scoped grant that does not cover this resource.
    DemandUnsatisfied,
    /// A READ demand was refused — never a write-standing problem, and it must
    /// not be phrased as one.
    ReadRefused,
}

impl std::fmt::Display for Rejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for Rejection {}

/// The facts Runtime derives for a docked principal. A World cannot assert or
/// replace them; authorization and commit compare-and-swap the same
/// `authority_frontier`. Constructed only inside Runtime
/// ([`Station::dock`](crate::lifecycle::Station::dock) resolves them through the
/// mechanics [`AuthorityView`]) — callers hand in a [`LocalIdentity`]
/// (proof-of-possession of a device seed), never facts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrincipalFacts {
    pub actor: ActorId,
    pub device: DeviceId,
    pub station: Key,
    /// The Space this principal is docked in — with the WorldId, the input to
    /// deterministic per-Space identities (for example, a product index BodyId).
    pub space: mechanics::ids::SpaceId,
    pub authority_frontier: AuthorityFrontier,
}

/// What the mechanics authority plane resolves for a local device: who it
/// speaks for and the authority frontier that membership was replayed at.
/// Fine-grained authorization is never carried here — every mutation is judged
/// by the capability demand evaluated at the pinned frontier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrincipalResolution {
    pub actor: ActorId,
    pub authority_frontier: AuthorityFrontier,
}

/// The mechanics-owned view of Space authority that Runtime consults to derive
/// [`PrincipalFacts`] — at dock **and again at every submit** (per-request
/// authorization, and the commit-side authority-frontier compare-and-swap).
/// Supplied by the deployment composition root (which owns the replayed signed
/// history); Sessions and Worlds can neither replace nor bypass it.
///
/// **Atomicity contract.** Runtime performs authorization, the frontier
/// compare-and-swap, and the durable commit inside one Station-writer critical
/// section. Authority mutations that themselves serialize through the same
/// Station writer (as orbital authority mutations do — membership changes are
/// Replica commits) therefore cannot interleave between the comparison and the
/// commit. An implementation whose state mutates *outside* that writer must
/// provide linearizable reads and accept that its mutations are ordered
/// against commits by the frontier CAS: a commit never proceeds against a
/// frontier the view no longer reports.
pub trait AuthorityView: Send + Sync {
    /// Resolve a local device's principal, or `None` when the device has no
    /// standing in the Space.
    fn resolve(&self, device: &DeviceId) -> Option<PrincipalResolution>;

    /// Resolve a *remote* Station's principal for a delivery-plane admission.
    ///
    /// Defaults to [`Self::resolve`] over the same key, because a Station is a
    /// device and membership is membership. It is a separate method so an
    /// implementation cannot acquire a local-only assumption underneath the
    /// peer path — `resolve` is called about our own devices constantly, and
    /// the day one of those callers wants a local shortcut, the peer path must
    /// not silently inherit it.
    fn admit_peer(&self, station: &mechanics::station::Key) -> Option<PrincipalResolution> {
        self.resolve(&station.as_device())
    }

    /// Resolve Contact standing. Implementations may recognize a narrower
    /// bootstrap standing (for example, possession of an unredeemed approach
    /// coordinate) before ordinary membership exists.
    fn admit_contact_peer(&self, station: &mechanics::station::Key) -> Option<PrincipalResolution> {
        self.admit_peer(station)
    }

    /// The active World implementation id at `authority_frontier`. The default
    /// treats every implementation as active (fixtures without a policy
    /// history); the orbital composition overrides it with the ledger's
    /// activation state, refusing an unapproved id.
    ///
    /// `Ok(None)` means *no activation exists* at that frontier; `Err` means
    /// the ledger **could not answer** (missing history, durable failure) —
    /// two different situations with two different remedies, and rendering the
    /// second as the first once sent an admin to run `world_upgrade` against a
    /// ledger that was simply failing to read.
    fn active_implementation(
        &self,
        _world: &WorldId,
        _authority_frontier: &AuthorityFrontier,
    ) -> Result<Option<[u8; 32]>, String> {
        Ok(Some([0u8; 32]))
    }

    /// Produce canonical [`mechanics::authorization::AuthorizationReceipt`] bytes for
    /// a mutation whose transaction core hashes to `core_digest`, binding every
    /// companion coordinate, or a typed denial. No World callback runs. The
    /// default builds a structurally-valid receipt without a real policy
    /// evaluation (fixtures); the orbital composition overrides it to evaluate
    /// the demand at the pinned frontier against signed history.
    #[allow(clippy::too_many_arguments)]
    fn authorize_mutation(
        &self,
        space: &mechanics::ids::SpaceId,
        world: &WorldId,
        actor: &ActorId,
        device: &DeviceId,
        authority_frontier: &AuthorityFrontier,
        parent_manifest_root: [u8; 32],
        implementation_id: [u8; 32],
        intent_digest: [u8; 32],
        demand: &[u8],
        operations_digest: [u8; 32],
        core_digest: [u8; 32],
    ) -> Result<Vec<u8>, mechanics::authorization::Refusal> {
        let parsed = mechanics::authorization::AuthorizationDemand::decode_canonical(demand)
            .map_err(mechanics::authorization::Refusal::Demand)?;
        let receipt = mechanics::authorization::AuthorizationReceipt {
            space: space.as_str().to_string(),
            world: world.as_str().to_string(),
            actor: actor.as_str().to_string(),
            device: device
                .key_bytes()
                .ok_or(mechanics::authorization::Refusal::Denied(
                    mechanics::authorization::DenialReason::Internal(
                        "device key bytes unavailable",
                    ),
                ))?,
            authority_frontier: authority_frontier.as_bytes().to_vec(),
            authority_checkpoint_commitment: [0u8; 32],
            policy_evidence_digest: mechanics::authorization::policy_evidence_digest(&[]),
            parent_manifest_root,
            implementation_id,
            intent_digest,
            demand_digest: parsed
                .digest()
                .map_err(mechanics::authorization::Refusal::Demand)?,
            effect_operations_digest: operations_digest,
            body_transaction_core_digest: core_digest,
            decision: 1,
        };
        Ok(receipt.encode())
    }

    /// Whether `actor` satisfies a read `demand` at `authority_frontier`. The
    /// default permits every read (fixtures); the orbital composition
    /// evaluates the demand against signed history.
    ///
    /// `Err` means the demand **could not be evaluated** (a malformed demand
    /// or a ledger that cannot materialize the frontier) — a local-state
    /// problem that is not a denial and must not be rendered as one.
    fn evaluate_read(
        &self,
        _actor: &ActorId,
        _authority_frontier: &AuthorityFrontier,
        _demand: &[u8],
    ) -> Result<bool, String> {
        Ok(true)
    }
}

/// An authenticated local caller: proof-of-possession of a device seed. Minted
/// only by [`Runtime::identity_from_seed`](crate::lifecycle::Runtime::identity_from_seed),
/// which derives the device key from the seed — a caller cannot assert an
/// arbitrary device id, let alone standing. The identity owns the device
/// signing capability opaquely; it never exposes the seed bytes (no accessor,
/// no serialization, and `Debug` prints only the derived device).
#[derive(Clone)]
pub struct LocalIdentity {
    device: DeviceId,
    seed: [u8; 32],
}

impl std::fmt::Debug for LocalIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalIdentity")
            .field("device", &self.device)
            .finish_non_exhaustive()
    }
}

impl LocalIdentity {
    pub(crate) fn from_seed(seed: &[u8; 32]) -> Self {
        Self {
            device: mechanics::actor::device_from_seed(seed),
            seed: *seed,
        }
    }

    /// The device this identity proved possession of.
    pub fn device(&self) -> &DeviceId {
        &self.device
    }

    /// Construct and sign the canonical [`SignedWorldAction`]
    /// (`crate::action`) for an intent against a docked Session: the header is
    /// built from the Session's Space/World and **fresh mechanics facts**
    /// resolved for this device (the caller cannot assert them), the payload is
    /// hash-bound, and the whole envelope is signed by this device.
    ///
    /// [`Session::submit`](crate::session::Session::submit) verifies and
    /// durably commits the action under the persistent-idempotency scope
    /// `(Space, World, Device, RequestId)`.
    pub fn sign_action(
        &self,
        session: &crate::session::Session,
        request: crate::action::RequestId,
        intent: Intent,
    ) -> Result<crate::action::SignedWorldAction, crate::world::Rejection> {
        // `resolve_for_signing` also returns `None` for a device that is not
        // the docked principal — folded into `NotAMember` because from the
        // caller's seat both read as "this identity has no standing here".
        let resolution = session
            .resolve_for_signing(&self.device)
            .ok_or(crate::world::Rejection::Denied(DeniedCause::NotAMember))?;
        let header = crate::action::WorldActionHeader {
            request,
            space: session.space_id().clone(),
            world: session.world_id().clone(),
            actor: resolution.actor,
            device: self.device.clone(),
            authority_frontier: resolution.authority_frontier,
            intent_schema: intent.schema,
            intent_version: intent.schema_version,
            payload_hash: crate::action::payload_hash(&intent.payload),
        };
        Ok(crate::action::SignedWorldAction::sign(
            header,
            intent.payload,
            &self.seed,
        ))
    }
}

/// The docked identity signs the durable Body transactions its Session
/// commits; the seed never leaves this type.
impl replica::transaction::Signer for LocalIdentity {
    fn signer_key(&self) -> [u8; 32] {
        #[allow(
            clippy::expect_used,
            reason = "LocalIdentity is constructed from an Ed25519 seed and therefore always has key bytes"
        )]
        self.device
            .key_bytes()
            .expect("seed-derived device key is well-formed")
    }
    fn sign_preimage(&self, preimage: &[u8]) -> [u8; 64] {
        mechanics::actor::sign_detached(&self.seed, preimage)
    }
}

/// A World's declared implementation version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Version(pub u32);

/// Bounded resource requirements a World declares. Concrete bounds are frozen in
/// S1; S0 reserves the shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Limits {
    /// Maximum decoded Intent/Query payload size in bytes (`0` = Runtime default).
    pub max_payload_bytes: u32,
}

/// A transient scope a World declares under
/// [`Target::World`](crate::transient::Target).
///
/// A key ceiling and nothing else. A payload ceiling was considered and
/// rejected: `World` admits only `TransientKind::Presence`, which carries
/// no bytes, so the number would bound nothing. A per-scope authorization
/// demand is absent because nothing has decided what one would mean for a
/// scope; carrying the field before its semantics exist is the wrong order.
///
/// The ceiling below is declared, reviewed, committed to the implementation id,
/// and enforced when a World transient scope is admitted. The substrate ceiling
/// remains the outer allocation bound; this declaration may only tighten it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopeSchema {
    pub name: SchemaId,
    /// The World's declared ceiling on a scope field. Registration refuses one
    /// that does not tighten the substrate's
    /// [`MAX_SCOPE_FIELD_BYTES`](crate::transient::MAX_SCOPE_FIELD_BYTES).
    pub max_key_bytes: u32,
}

/// A World signal schema: what it is called, how large it may be, and what
/// authority sending it demands.
///
/// The authority is canonical `mechanics::authorization::AuthorizationDemand` bytes
/// rather than a capability name, because that is the form
/// [`SignalDemand::World`](crate::signal::SignalDemand) carries and policy
/// evaluates; a name would need a translation to demand bytes that nobody has
/// written.
///
/// The ceiling and demand are enforced after the World signal body identifies
/// its registered schema. The generic `selector::WORLD` declaration supplies
/// only the substrate bound; the schema supplies the tighter product bound and
/// the authority demand evaluated at the pinned frontier.
///
/// There is deliberately no answer policy, and that omission is a different
/// case from the two above. `selector::WORLD` declares
/// `ResponsePolicy::Forbidden`, so a per-schema `Acknowledge` would be a
/// declaration the substrate *contradicts* rather than one it has not started
/// applying. If one ever becomes enforceable it is a new descriptor section
/// tag, not an edit to this one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignalSchema {
    pub name: SchemaId,
    /// The World's declared ceiling. Registration refuses one that does not
    /// tighten the plane's
    /// [`MAX_SIGNAL_BYTES`](crate::plane::bounds::MAX_SIGNAL_BYTES).
    pub max_payload_bytes: u32,
    /// Canonical [`mechanics::authorization::AuthorizationDemand`] bytes. Parsed at
    /// registration and evaluated before delivery.
    pub demand: Vec<u8>,
}

/// What a World supplies at registration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Descriptor {
    pub id: WorldId,
    pub implementation_version: Version,
    pub schemas: Vec<Schema>,
    pub limits: Limits,
    /// Declared transient scopes. Empty is the ordinary case and costs nothing:
    /// the implementation descriptor omits an empty section entirely.
    pub scope_schemas: Vec<ScopeSchema>,
    /// Declared World signals, under the same rule.
    pub signal_schemas: Vec<SignalSchema>,
    /// World-owned Find vocabularies. Empty preserves the implementation id
    /// that shipped before Find existed.
    pub find_schemas: Vec<crate::find::Schema>,
    /// Exact package bindings for the declared Body sources. These coordinates
    /// are checked one-to-one at composition and invoked only while building
    /// an immutable publication corpus.
    pub find_extractors: Vec<crate::find::Extractor>,
    /// Callable Exec contracts. Empty omits the Exec descriptor section and
    /// preserves the implementation identity of Worlds that do not adopt it.
    #[serde(default)]
    pub exec_specs: Vec<crate::exec::Spec>,
}

/// A decoded, authorized-by-Runtime application intent handed to a World. The
/// payload is the World's own bytes; Runtime does not interpret it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Intent {
    pub schema: replica::body::SchemaId,
    pub schema_version: u32,
    pub payload: Vec<u8>,
}

/// A decoded application query handed to a World.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Query {
    pub schema: replica::body::SchemaId,
    pub schema_version: u32,
    pub payload: Vec<u8>,
    /// Exact semantic read publication. `None` selects the authority-active
    /// current publication; `Some` resolves the installed implementation and
    /// extractor contract named here and never reinterprets the root with the
    /// ambient package.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publication: Option<crate::publication::PublicationId>,
}

/// A runtime-owned create declaration: the immutable schema binding for a Body
/// this transaction creates. An operation on a new Body with no declaration
/// defaults to the intent's schema; an operation on an existing Body uses its
/// recorded binding — a later write can never change a Body's schema
/// implicitly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BodyDeclaration {
    pub key: BodyKey,
    pub schema: replica::body::SchemaId,
    pub schema_version: u32,
}

mod serde_exec_commands {
    use super::*;

    pub fn serialize<S>(commands: &[crate::exec::Cmd], serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let canonical = commands
            .iter()
            .map(crate::exec::Cmd::encode)
            .collect::<Result<Vec<_>, _>>()
            .map_err(serde::ser::Error::custom)?;
        canonical.serialize(serializer)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Vec<crate::exec::Cmd>, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        Vec::<Vec<u8>>::deserialize(deserializer)?
            .into_iter()
            .map(|bytes| {
                crate::exec::Cmd::decode_canonical(&bytes).map_err(serde::de::Error::custom)
            })
            .collect()
    }
}

/// The result a World returns from `submit`: the staged Body operations, the
/// Observation Bodies they touch, an opaque application effect payload, and the
/// **canonical non-empty authorization demand** the mutation requires. There
/// is no implicit `Write` fallback — Runtime evaluates this exact demand at the
/// pinned authority frontier and commits nothing if it is unsatisfied.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Effect {
    /// The content each touched Body references, declared by the World.
    ///
    /// A `ContentRef` committed inside a Body is product-encoded, and the
    /// substrate may not decode product bytes to find it. Without a declaration
    /// the content catalog could only grow: tombstone every Body that
    /// referenced an upload and its descriptor stays signed state on every peer
    /// forever. So the World says which content its Bodies name, Replica
    /// validates the claim against committed descriptors, and reachability
    /// becomes computable without the boundary moving.
    ///
    /// Absent means unchanged. An empty vector means "this Body references
    /// nothing", which is how content is released.
    pub content_refs: Vec<(BodyKey, Vec<replica::content::ContentRef>)>,
    /// Durable work commands staged by the World for Runtime to contain and
    /// lower beside the Body operations in this same transaction.
    ///
    /// The World declares work here; it never executes it. Runtime owns every
    /// command's authorization, canonical event lowering, and dispatch.
    #[serde(with = "serde_exec_commands")]
    pub exec: Vec<crate::exec::Cmd>,
    /// Body operations staged this transaction, each keyed to the Body it
    /// mutates.
    pub operations: Vec<(BodyKey, Op)>,
    /// The Observation Bodies affected, so Runtime can publish invalidations.
    pub bodies: Vec<BodyKey>,
    /// An opaque application-defined effect payload returned to the caller.
    pub effect: Vec<u8>,
    /// Schema declarations for Bodies this transaction creates (multi-schema
    /// transactions declare each non-intent-schema Body explicitly).
    pub declarations: Vec<BodyDeclaration>,
    /// The canonical [`mechanics::authorization::AuthorizationDemand`] bytes this
    /// mutation requires (mandatory, non-empty).
    pub demand: Vec<u8>,
}

/// A canonical, versioned Projection a World returns from `query`, plus the
/// committed frontier it was derived at, and the read demand it required. Even
/// publicly visible product data uses an explicit read capability granted by
/// policy — there is no implicit-read fallback.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Projection {
    pub schema: replica::body::SchemaId,
    pub schema_version: u32,
    pub bytes: Vec<u8>,
    pub frontier: replica::frontier::ReplicaFrontier,
    /// Runtime stamps the exact immutable read image after the World callback.
    /// Worlds return `None`; a Projection returned by Session always carries
    /// `Some`, including implementation, extractor, and local materialization.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publication: Option<crate::publication::WorldPublicationId>,
    /// The canonical read demand this query required (mandatory, non-empty).
    /// Runtime evaluates it at the pinned frontier and returns no projection
    /// on denial.
    pub demand: Vec<u8>,
}

/// A container a World groups its items by, named twice on purpose.
///
/// `id` is the stable identity a dependent matches on; `label` is a *mutable*
/// display alias that a rename changes underneath you. Matching on the label is
/// a latent bug waiting for the first rename, and dropping it would force every
/// consumer to resolve one before it could route or render — so both travel
/// together, with the roles stated.
///
/// `kind` is the World's own word for what the container is. Runtime carries it
/// and never interprets it; a World with no containers never builds one.
#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScopeRef {
    /// World-declared container vocabulary. Opaque to Runtime.
    pub kind: String,
    /// The container's stable identity.
    pub id: String,
    /// A mutable human-facing alias, when the World has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// Which items moved inside one container.
///
/// Item-level, not container-level: a consumer re-reads exactly the ids named
/// here rather than the whole container. The container is spelled inline rather
/// than nested as a [`ScopeRef`] because `serde(flatten)` and
/// `deny_unknown_fields` do not compose, and this type keeps the strictness.
#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DirtyScope {
    /// World-declared container vocabulary. Opaque to Runtime.
    pub kind: String,
    /// The container's stable identity.
    pub id: String,
    /// A mutable human-facing alias, when the World has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// The item ids whose rows must be re-read.
    pub docs: Vec<String>,
}

/// A structural plane of a World that moved, optionally narrowed to one
/// container.
///
/// Distinct from [`DirtyScope`] because there are two ideas here and collapsing
/// them loses one: a plane names a *structure* to re-read whole and has no item
/// ids to hand out, while a dirty scope names *which items* inside a container
/// moved. A World that partitions a plane per container fills `scope`; a
/// Space-wide plane leaves it `None`.
#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DirtyPlane {
    /// World-declared plane vocabulary. Opaque to Runtime.
    pub plane: String,
    /// The container this plane instance belongs to, if the World partitions it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<ScopeRef>,
}

/// What one World says an [`Observation`] invalidated, in that World's own
/// vocabulary.
///
/// Both halves are World-opaque: Runtime and the host carry `kind`/`plane`
/// strings and never route on them. A World with no containers returns only
/// `planes`; a World with no structural planes returns only `dirty`. Neither
/// half is a substitute for the other.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Invalidation {
    /// Which items moved, grouped by the container the World groups them by.
    pub dirty: Vec<DirtyScope>,
    /// Which structural planes moved.
    pub planes: Vec<DirtyPlane>,
}

/// One World's invalidation payload, tagged for routing by product clients.
///
/// `kind` and `plane` are deliberately only unique inside a World. Grouping
/// under the stable [`WorldId`] prevents two installed Worlds that both use a
/// word such as `document` or `catalog` from invalidating each other's views.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoutedInvalidation {
    pub world: WorldId,
    pub dirty: Vec<DirtyScope>,
    pub planes: Vec<DirtyPlane>,
}

/// What a World may know about one content: enough to render it, and nothing
/// that would let it reach the bytes without asking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContentStatus {
    pub plaintext_len: u64,
    pub chunk_count: u32,
    /// How many chunks are here now. A fact about this moment on this machine,
    /// and never replicated.
    pub resident_chunks: u32,
}

/// Runtime-decoded facts for one exactly-once returned Exec Outcome.
///
/// A World receives coordinates and immutable content identities, never the
/// output bytes or a handle to Runtime-owned Bodies. `Context::outcome` returns
/// this only when the named Run and Attempt exist in the callback's pinned
/// snapshot and the Attempt has exactly one valid `Returned` fact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutcomeFacts {
    pub run: crate::exec::RunId,
    pub attempt: crate::exec::AttemptId,
    pub spec: crate::exec::SchemaRef,
    pub build: crate::exec::BuildId,
    pub station: mechanics::station::Key,
    pub terminal: crate::exec::TerminalClass,
    pub output: crate::exec::SchemaRef,
    pub output_digest: [u8; 32],
    pub output_inline_bytes: u32,
    pub output_content: Vec<replica::content::ContentRef>,
    pub output_content_bytes: u64,
    pub returned_exactly_once: bool,
}

impl ContentStatus {
    pub fn is_complete(&self) -> bool {
        self.resident_chunks == self.chunk_count
    }
}

/// A read view of the committed Body snapshot, handed to a World during a query.
/// It exposes only authorized canonical reads — no CRDT internals, no mutation, no keys.
/// Runtime backs it with the Station's Replica.
pub trait BodyReader {
    /// The committed canonical bytes of an atomic Body, if present.
    fn read_body(&self, key: &BodyKey) -> Option<Vec<u8>>;
    /// The committed collaborative view of a Body. List elements carry the
    /// stable ids `ListRemove`/`ListMove` take. A Body binding a collaborative
    /// type this build does not implement is `SchemaAhead`, never a view with
    /// the unreadable part quietly missing.
    fn read_collaborative_body(
        &self,
        key: &BodyKey,
    ) -> Result<fabric::CollaborativeView, fabric::projection::Failure>;
    /// Every interpreted Body of `world` bound to `schema` — the
    /// singleton-integrity seam (a World validating that exactly its one
    /// deterministic instance of a schema exists).
    fn bodies_with_schema(&self, world: &WorldId, schema: &SchemaId) -> Vec<BodyKey>;

    /// One canonical page of readable Body keys at a World/schema coordinate.
    ///
    /// Production snapshot readers seek the persistent schema directory, so
    /// work is proportional to schema versions plus returned keys rather than
    /// all Bodies. The default preserves detached test readers without making
    /// a performance promise; hosted [`Context`] values are backed by the
    /// indexed implementation.
    fn body_keys_page_with_schema(
        &self,
        world: &WorldId,
        schema: &SchemaId,
        after: Option<&BodyKey>,
        limit: usize,
    ) -> Vec<BodyKey> {
        let mut keys = self.bodies_with_schema(world, schema);
        keys.sort();
        let start = after.map_or(0, |after| keys.partition_point(|key| key <= after));
        keys.into_iter().skip(start).take(limit).collect()
    }

    /// A Body's position in its collaborative history.
    ///
    /// Opaque and orderable, and never the convergence engine's own type. A
    /// World uses it to compare
    /// positions and to stamp anchors; it cannot use it to reach the engine.
    fn body_version(&self, key: &BodyKey) -> Option<fabric::Version>;

    /// Take an anchor at a position inside a collaborative value.
    ///
    /// This is the seam plan 14's carets and range-attached comments consume,
    /// and it is exposed here rather than there because only the algebra that
    /// moves a position can mint one that survives being moved.
    fn anchor_in_body(&self, key: &BodyKey, path: &str, position: u64) -> Option<fabric::Anchor>;

    /// Resolve an anchor against a Body's current state.
    ///
    /// Total and read-only: a position whose material was deleted, or whose
    /// anchor predates what this replica retains, is `Drifted`. Never an error,
    /// never a mutation, and never a silently wrong index.
    fn resolve_anchor(&self, key: &BodyKey, anchor: &fabric::Anchor) -> fabric::AnchorResolution;

    /// What one content is, and how much of it is here.
    ///
    /// A World sees size, geometry, and residency — never a path, never bytes,
    /// never a key. Reading the bytes is a host call with its own demand, and
    /// a `ContentRef` alone authorizes nothing.
    fn content_status(&self, content: &replica::content::ContentRef) -> Option<ContentStatus>;

    /// Runtime-decoded Outcome facts from this exact committed snapshot.
    ///
    /// The default is deliberately unavailable for readers that do not carry
    /// Runtime's protected projection capability.
    fn outcome(
        &self,
        _world: &WorldId,
        _run: crate::exec::RunId,
        _attempt: crate::exec::AttemptId,
    ) -> Option<OutcomeFacts> {
        None
    }

    /// An opaque per-Body VERSION STAMP: two reads returning the same stamp
    /// for a key are guaranteed byte-equivalent Bodies, so a World may reuse
    /// state it derived from the earlier read. `None` (the default) promises
    /// nothing — the World must re-derive. Never a content hash contract;
    /// only equality is meaningful.
    fn body_stamp(&self, _key: &BodyKey) -> Option<Vec<u8>> {
        None
    }
}

/// Runtime-owned, read-only access to the shared Find publication selected for
/// one World callback. Implementations never receive a Corpus or authority
/// object: the capability is already pinned to an immutable publication and
/// its principal gates, and every call re-enters Runtime's bounded evaluator.
pub trait FindReader: Send + Sync {
    fn publication(&self) -> crate::publication::WorldPublicationId;
    fn find(&self, query: crate::find::Query) -> Result<crate::find::Answer, crate::find::Failure>;
}

/// Cloneable, read-only access to one already-authorized immutable Find image.
///
/// Unlike [`Context`], this handle may be moved to a bounded product worker.
/// It owns the exact publication and evaluated gate set, so deferred analytics
/// neither re-enter Session nor consult whatever publication is current later.
/// Every read still crosses the declared Find evaluator and its admission
/// bounds; no Corpus or ungated storage surface is exposed.
#[derive(Clone)]
pub struct FindHandle {
    reader: Arc<dyn FindReader>,
}

impl std::fmt::Debug for FindHandle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("FindHandle")
            .field("publication", &self.publication())
            .finish_non_exhaustive()
    }
}

impl FindHandle {
    pub(crate) fn new(reader: Arc<dyn FindReader>) -> Self {
        Self { reader }
    }

    pub fn publication(&self) -> crate::publication::WorldPublicationId {
        self.reader.publication()
    }

    pub fn find(
        &self,
        query: crate::find::Query,
    ) -> Result<crate::find::Answer, crate::find::Failure> {
        self.reader.find(query)
    }
}

/// Principal-neutral read capability supplied to a declared Find extractor.
///
/// Extraction may derive shared corpus facts from one immutable publication;
/// it cannot see an actor, device, grant, clock, mutable Replica, or network.
/// Disclosure is represented by Gate references on the emitted rows and is
/// applied later for each requesting principal.
pub struct ExtractionContext<'a> {
    reads: &'a dyn BodyReader,
    world: &'a WorldId,
    publication: crate::publication::WorldPublicationId,
}

impl<'a> ExtractionContext<'a> {
    pub(crate) fn new(
        reads: &'a dyn BodyReader,
        world: &'a WorldId,
        publication: crate::publication::WorldPublicationId,
    ) -> Self {
        Self {
            reads,
            world,
            publication,
        }
    }

    pub fn manifest_root(&self) -> [u8; 32] {
        self.publication.publication.manifest_root
    }

    /// The complete immutable read coordinate this extraction will publish
    /// into. Manifest identity alone is insufficient because implementation,
    /// extractor declaration, and Station-local readability may move
    /// independently.
    pub fn world_publication_id(&self) -> crate::publication::WorldPublicationId {
        self.publication
    }

    pub fn read_body(&self, key: &BodyKey) -> Option<Vec<u8>> {
        (key.world == *self.world)
            .then(|| self.reads.read_body(key))
            .flatten()
    }

    pub fn read_collaborative(
        &self,
        key: &BodyKey,
    ) -> Result<fabric::CollaborativeView, fabric::projection::Failure> {
        if key.world != *self.world {
            return Err(fabric::projection::Failure::NotCollaborative);
        }
        self.reads.read_collaborative_body(key)
    }

    pub fn body_stamp(&self, key: &BodyKey) -> Option<Vec<u8>> {
        (key.world == *self.world)
            .then(|| self.reads.body_stamp(key))
            .flatten()
    }
}

/// The bounded capability handed to World callbacks. It exposes the principal
/// facts, authorized reads of the stable committed snapshot (during a query),
/// and **nothing** below the boundary: no CRDT internals, no mutable storage, no keys, no
/// network. A World stages Body operations by *returning* them in a
/// [`Effect`]; Runtime — not the World — performs the durable commit.
pub struct Context<'a> {
    principal: &'a PrincipalFacts,
    reads: Option<&'a dyn BodyReader>,
    outcome_world: Option<&'a WorldId>,
    request: Option<crate::action::RequestId>,
    /// The committed Manifest root this callback is pinned to (the parent of a
    /// submitted transaction; the snapshot root of a query).
    manifest_root: [u8; 32],
    world_publication: Option<crate::publication::WorldPublicationId>,
    find: Option<FindHandle>,
}

impl<'a> Context<'a> {
    /// Largest durable migration/admin page accepted by one callback.
    pub const MAX_BODY_KEY_PAGE: usize = 4_096;
    /// Construct a context over a principal's facts with no read access (submit
    /// authorizes and stages; it does not read the snapshot).
    pub fn new(principal: &'a PrincipalFacts) -> Self {
        Self {
            principal,
            reads: None,
            outcome_world: None,
            request: None,
            manifest_root: [0u8; 32],
            world_publication: None,
            find: None,
        }
    }

    /// Construct a context with committed-snapshot read access, pinned to the
    /// snapshot's Manifest root.
    pub fn with_reads(
        principal: &'a PrincipalFacts,
        reads: &'a dyn BodyReader,
        manifest_root: [u8; 32],
    ) -> Self {
        Self {
            principal,
            reads: Some(reads),
            outcome_world: None,
            request: None,
            manifest_root,
            world_publication: None,
            find: None,
        }
    }

    /// Construct the capability used by a hosted World callback. Outcome
    /// access is bound to that World and cannot name another namespace.
    pub(crate) fn with_world_reads(
        principal: &'a PrincipalFacts,
        reads: &'a dyn BodyReader,
        publication: crate::publication::WorldPublicationId,
        world: &'a WorldId,
        find: FindHandle,
    ) -> Self {
        Self {
            principal,
            reads: Some(reads),
            outcome_world: Some(world),
            request: None,
            manifest_root: publication.publication.manifest_root,
            world_publication: Some(publication),
            find: Some(find),
        }
    }

    #[cfg(test)]
    pub(crate) fn with_world_reads_for_test(
        principal: &'a PrincipalFacts,
        reads: &'a dyn BodyReader,
        manifest_root: [u8; 32],
        world: &'a WorldId,
    ) -> Self {
        Self {
            principal,
            reads: Some(reads),
            outcome_world: Some(world),
            request: None,
            manifest_root,
            world_publication: None,
            find: None,
        }
    }

    /// Construct the capability used by one hosted World submission.
    ///
    /// In addition to pinned reads and Outcome facts, a submission may derive
    /// the stable Run id Runtime will assign to a staged command. This lets the
    /// World bind its own target to that id in the same Effect without learning
    /// or duplicating Runtime's derivation algorithm.
    pub(crate) fn with_world_submission(
        principal: &'a PrincipalFacts,
        reads: &'a dyn BodyReader,
        publication: crate::publication::WorldPublicationId,
        world: &'a WorldId,
        request: crate::action::RequestId,
        find: FindHandle,
    ) -> Self {
        Self {
            principal,
            reads: Some(reads),
            outcome_world: Some(world),
            request: Some(request),
            manifest_root: publication.publication.manifest_root,
            world_publication: Some(publication),
            find: Some(find),
        }
    }

    /// The committed Manifest root this callback is pinned to.
    pub fn manifest_root(&self) -> [u8; 32] {
        self.manifest_root
    }

    /// The complete immutable World read coordinate selected for this hosted
    /// callback. Detached fixture contexts do not claim one.
    pub fn world_publication_id(&self) -> Option<crate::publication::WorldPublicationId> {
        self.world_publication
    }

    /// Run one declared, bounded query against this callback's exact shared
    /// publication and already-evaluated principal gates. No mutable or
    /// ungated Corpus access crosses the World boundary.
    pub fn find(
        &self,
        query: crate::find::Query,
    ) -> Result<crate::find::Answer, crate::find::Failure> {
        let find = self
            .find
            .as_ref()
            .ok_or(crate::find::Failure::Unavailable)?;
        if self.world_publication != Some(find.publication()) {
            return Err(crate::find::Failure::Unavailable);
        }
        find.find(query)
    }

    /// Detach this callback's exact, gated Find capability for bounded
    /// background projection work. Creating the handle is O(1); extraction or
    /// traversal begins only when the worker calls [`FindHandle::find`].
    pub fn deferred_find(&self) -> Option<FindHandle> {
        self.find.clone()
    }

    /// The authenticated persistent action coordinate for this submission.
    /// Query and detached contexts return `None`. A World may use these bytes
    /// only as deterministic input to its own domain-separated identity
    /// derivation; Runtime still owns mutation admission and Body containment.
    pub fn request_id(&self) -> Option<crate::action::RequestId> {
        self.request
    }

    /// The stable Run id Runtime will assign to `command` in this submission.
    /// Queries and detached fixture contexts return `None` because they carry
    /// no persistent request coordinate.
    pub fn run_id(&self, command: u32) -> Option<crate::exec::RunId> {
        let world = self.outcome_world?;
        let request = self.request?;
        Some(crate::exec::derive_run_id(
            &self.principal.space,
            world,
            &self.principal.device,
            request.as_bytes(),
            command,
        ))
    }

    /// Every interpreted Body of `world` bound to `schema` in the committed
    /// snapshot (empty without read access).
    /// The reader's per-Body version stamp (see [`BodyReader::body_stamp`]).
    pub fn body_stamp(&self, key: &BodyKey) -> Option<Vec<u8>> {
        self.reads.and_then(|r| r.body_stamp(key))
    }

    pub fn bodies_with_schema(&self, world: &WorldId, schema: &SchemaId) -> Vec<BodyKey> {
        self.reads
            .map(|r| r.bodies_with_schema(world, schema))
            .unwrap_or_default()
    }

    /// Seek one bounded, publication-pinned page of Body keys for a schema.
    /// `after` is an exclusive durable BodyKey cursor, not a process-local
    /// iterator or Find cursor; callers persist `(phase, after)` and resubmit
    /// the next batch explicitly.
    pub fn body_keys_page_with_schema(
        &self,
        world: &WorldId,
        schema: &SchemaId,
        after: Option<&BodyKey>,
        limit: usize,
    ) -> Result<Vec<BodyKey>, Rejection> {
        if limit == 0 || limit > Self::MAX_BODY_KEY_PAGE {
            return Err(Rejection::LimitExceeded);
        }
        Ok(self
            .reads
            .map(|reader| reader.body_keys_page_with_schema(world, schema, after, limit))
            .unwrap_or_default())
    }

    /// The derived facts for the docked principal. A World authorizes against
    /// these; it cannot replace them.
    pub fn principal(&self) -> &PrincipalFacts {
        self.principal
    }

    /// Read a World-owned atomic Body from the stable committed snapshot.
    /// Returns `None` if the Body is absent, this context has no read access,
    /// or its schema is Runtime-reserved. Runtime-owned Exec truth is exposed
    /// only through typed, independently authorized facades such as the later
    /// `Context::outcome`, never through raw Body decoding.
    pub fn read_body(&self, key: &BodyKey) -> Option<Vec<u8>> {
        self.reads.and_then(|r| r.read_body(key))
    }

    /// A Body's causal position, for comparison and for stamping anchors.
    pub fn body_version(&self, key: &BodyKey) -> Option<fabric::Version> {
        self.reads.and_then(|r| r.body_version(key))
    }

    /// Take an anchor at a position inside a collaborative value.
    pub fn anchor(&self, key: &BodyKey, path: &str, position: u64) -> Option<fabric::Anchor> {
        self.reads
            .and_then(|r| r.anchor_in_body(key, path, position))
    }

    /// Resolve an anchor. Total: `Drifted` rather than an error or a guess.
    pub fn resolve_anchor(
        &self,
        key: &BodyKey,
        anchor: &fabric::Anchor,
    ) -> fabric::AnchorResolution {
        match self.reads {
            Some(reads) => reads.resolve_anchor(key, anchor),
            None => fabric::AnchorResolution::Drifted,
        }
    }

    /// What one content is, and how much of it is here.
    pub fn content_status(&self, content: &replica::content::ContentRef) -> Option<ContentStatus> {
        self.reads.and_then(|r| r.content_status(content))
    }

    /// Read Runtime-decoded facts for one exactly-once returned Outcome from
    /// the same committed snapshot as every other Context read.
    pub fn outcome(
        &self,
        run: crate::exec::RunId,
        attempt: crate::exec::AttemptId,
    ) -> Option<OutcomeFacts> {
        self.reads.and_then(|reads| {
            self.outcome_world
                .and_then(|world| reads.outcome(world, run, attempt))
        })
    }

    /// Read a World-owned collaborative Body's view from the stable committed
    /// snapshot. A Runtime-reserved Body is reported as unavailable here; its
    /// typed facade is the only sanctioned read boundary.
    pub fn read_collaborative(
        &self,
        key: &BodyKey,
    ) -> Result<fabric::CollaborativeView, fabric::projection::Failure> {
        match self.reads {
            Some(reads) => reads.read_collaborative_body(key),
            None => Err(fabric::projection::Failure::NotCollaborative),
        }
    }
}

/// An independently supplied World implementation.
///
/// Implementations promise deterministic synchronous bounded CPU work: identical
/// snapshot, facts, and request must produce identical staged operations and
/// Projection bytes. They must not persist, publish Observations, access
/// network/custody/configuration, decide Space legitimacy, or retain the
/// context.
pub trait World: Send + Sync + 'static {
    /// The reviewed declaration for this implementation.
    ///
    /// Runtime obtains the descriptor from the implementation itself so a
    /// composition root cannot pair running code with a different declaration.
    fn descriptor(&self) -> Descriptor {
        Descriptor {
            id: self.id(),
            implementation_version: Version(1),
            schemas: self.schemas().to_vec(),
            limits: Limits::default(),
            scope_schemas: self.scope_schemas().to_vec(),
            signal_schemas: self.signal_schemas().to_vec(),
            find_schemas: self.find_schemas().to_vec(),
            find_extractors: self.find_extractors().to_vec(),
            exec_specs: self.exec_specs().to_vec(),
        }
    }

    /// This World's stable namespaced identity.
    fn id(&self) -> WorldId;

    /// The Body schemas this World supports.
    fn schemas(&self) -> &[Schema];

    /// The transient scopes this World declares.
    ///
    /// Defaulted to none rather than made a required method: a World with
    /// nothing to declare needs no edit, and because the descriptor omits an
    /// empty section it also keeps the implementation id it already has.
    /// The default is a declaration and not a hole: [`World::descriptor`]
    /// includes this exact answer in the reviewed descriptor.
    fn scope_schemas(&self) -> &[ScopeSchema] {
        &[]
    }

    /// The World signals this World declares, under the same rule.
    fn signal_schemas(&self) -> &[SignalSchema] {
        &[]
    }

    /// The World-owned Find vocabularies this package declares.
    fn find_schemas(&self) -> &[crate::find::Schema] {
        &[]
    }

    /// Exact extractor coordinates implemented by this package.
    ///
    /// Runtime composition requires one binding for every declared source and
    /// refuses missing, extra, duplicated, or cross-wired coordinates. The
    /// ABI and semantic digest are part of the extractor contract; changing
    /// executable meaning changes the publication identity.
    fn find_extractors(&self) -> &[crate::find::Extractor] {
        &[]
    }

    /// Derive this package's principal-neutral Find rows for one exact Body
    /// source binding. Runtime invokes it only for coordinates declared by
    /// [`Self::find_extractors`] and validates the returned body/schema/gates
    /// before a publication becomes ready.
    fn extract(
        &self,
        _ctx: &ExtractionContext<'_>,
        _extractor: &crate::find::Extractor,
        _body: &BodyKey,
    ) -> Result<crate::find::BodyExtraction, Rejection> {
        Err(Rejection::ContractViolation)
    }

    /// Callable Exec Specs implemented by this World package.
    fn exec_specs(&self) -> &[crate::exec::Spec] {
        &[]
    }

    /// Decode, authorize, and stage Body operations for an application intent.
    fn submit(
        &self,
        ctx: &mut Context<'_>,
        intent: Intent,
    ) -> Result<Effect, crate::world::Rejection>;

    /// Decode a query and derive a Projection from the stable snapshot.
    fn query(&self, ctx: &Context<'_>, query: Query)
        -> Result<Projection, crate::world::Rejection>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effect_serde_round_trips_exec_through_the_canonical_command_codec() {
        let effect = Effect {
            content_refs: Vec::new(),
            exec: vec![crate::exec::Cmd::Cancel {
                run: crate::exec::RunId::from_bytes([0x42; 16]),
            }],
            operations: Vec::new(),
            bodies: Vec::new(),
            effect: Vec::new(),
            declarations: Vec::new(),
            demand: vec![1],
        };

        let bytes = postcard::to_stdvec(&effect).unwrap();
        let decoded: Effect = postcard::from_bytes(&bytes).unwrap();

        assert_eq!(decoded, effect);
    }
}
