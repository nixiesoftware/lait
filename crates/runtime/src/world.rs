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

use mechanics::ids::{ActorId, DeviceId, StationId};
use replica::body::BodyOp;
use replica::frontier::AuthorityFrontier;
use replica::ids::{BodyKey, SchemaId, WorldId};
use replica::BodySchema;
use serde::{Deserialize, Serialize};

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
    pub station: StationId,
    /// The Space this principal is docked in — with the WorldId, the input to
    /// deterministic per-Space identities (e.g. the Issues Catalog BodyId).
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
    fn admit_peer(&self, station: &mechanics::ids::StationId) -> Option<PrincipalResolution> {
        self.resolve(&station.as_device())
    }

    /// The active World implementation id at `authority_frontier`. The default
    /// treats every implementation as active (fixtures without a policy
    /// history); the orbital composition overrides it with the ledger's
    /// activation state, refusing an unapproved id.
    fn active_implementation(
        &self,
        _world: &WorldId,
        _authority_frontier: &AuthorityFrontier,
    ) -> Option<[u8; 32]> {
        Some([0u8; 32])
    }

    /// Produce canonical [`mechanics::demand::AuthorizationReceipt`] bytes for
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
    ) -> Result<Vec<u8>, String> {
        let parsed = mechanics::demand::AuthorizationDemand::decode_canonical(demand)
            .map_err(|e| format!("demand: {e}"))?;
        let receipt = mechanics::demand::AuthorizationReceipt {
            space: space.as_str().to_string(),
            world: world.as_str().to_string(),
            actor: actor.as_str().to_string(),
            device: device.key_bytes().ok_or("device key")?,
            authority_frontier: authority_frontier.as_bytes().to_vec(),
            authority_checkpoint_commitment: [0u8; 32],
            policy_evidence_digest: mechanics::demand::policy_evidence_digest(&[]),
            parent_manifest_root,
            implementation_id,
            intent_digest,
            demand_digest: parsed.digest().map_err(|e| format!("demand digest: {e}"))?,
            effect_operations_digest: operations_digest,
            body_transaction_core_digest: core_digest,
            decision: 1,
        };
        Ok(receipt.encode())
    }

    /// Whether `actor` satisfies a read `demand` at `authority_frontier`. The
    /// default permits every read (fixtures); the orbital composition
    /// evaluates the demand against signed history.
    fn evaluate_read(
        &self,
        _actor: &ActorId,
        _authority_frontier: &AuthorityFrontier,
        _demand: &[u8],
    ) -> bool {
        true
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
            device: mechanics::crypto::device_from_seed(seed),
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
        intent: WorldIntent,
    ) -> Result<crate::action::SignedWorldAction, crate::error::WorldError> {
        let resolution = session
            .resolve_for_signing(&self.device)
            .ok_or(crate::error::WorldError::Denied)?;
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
impl replica::transaction::TransactionSigner for LocalIdentity {
    fn signer_key(&self) -> [u8; 32] {
        self.device
            .key_bytes()
            .expect("seed-derived device key is well-formed")
    }
    fn sign_preimage(&self, preimage: &[u8]) -> [u8; 64] {
        mechanics::crypto::sign_detached(&self.seed, preimage)
    }
}

/// A World's declared implementation version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct WorldVersion(pub u32);

/// Bounded resource requirements a World declares. Concrete bounds are frozen in
/// S1; S0 reserves the shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct WorldLimits {
    /// Maximum decoded Intent/Query payload size in bytes (`0` = Runtime default).
    pub max_payload_bytes: u32,
}

/// A transient scope a World declares under
/// [`TransientScope::CustomWorld`](crate::transient::TransientScope).
///
/// A key ceiling and nothing else. A payload ceiling was considered and
/// rejected: `CustomWorld` admits only `TransientKind::Presence`, which carries
/// no bytes, so the number would bound nothing. A per-scope authorization
/// demand is absent because nothing has decided what one would mean for a
/// scope; carrying the field before its semantics exist is the wrong order.
///
/// The ceiling below is declared and reviewed — it is in the implementation id
/// — and it is **not yet applied at delivery**. `transient.rs` bounds every
/// scope field at
/// [`MAX_SCOPE_FIELD_BYTES`](crate::transient::MAX_SCOPE_FIELD_BYTES) and
/// consults no registered `ScopeSchema`, so a World declaring 8 still sees 128.
/// Committing the number to the reviewed identity ahead of the check is
/// deliberate: wiring it changes no descriptor bytes and therefore moves no id
/// an authority has already activated.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopeSchema {
    pub name: SchemaId,
    /// The World's declared ceiling on a scope field. Registration refuses one
    /// that does not tighten the substrate's
    /// [`MAX_SCOPE_FIELD_BYTES`](crate::transient::MAX_SCOPE_FIELD_BYTES),
    /// which is the only place it is read today.
    pub max_key_bytes: u32,
}

/// A World signal schema: what it is called, how large it may be, and what
/// authority sending it demands.
///
/// The authority is canonical `mechanics::demand::AuthorizationDemand` bytes
/// rather than a capability name, because that is the form
/// [`SignalDemand::World`](crate::signal::SignalDemand) carries and policy
/// evaluates; a name would need a translation to demand bytes that nobody has
/// written.
///
/// Neither the ceiling nor the demand is applied at delivery yet. Every World
/// signal rides `selector::WORLD`, whose core declaration carries
/// `SignalDemand::Session` and
/// [`MAX_SIGNAL_BYTES`](crate::planes::bounds::MAX_SIGNAL_BYTES), and the read
/// path resolves its bound and its authority from that declaration without
/// consulting the registered schema. Both numbers are declared, reviewed, and
/// committed to the implementation id; what is missing is the resolution step
/// that turns a `selector::WORLD` frame into its schema's declaration. Wiring
/// it changes no descriptor bytes, so it moves no already-activated id.
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
    /// [`MAX_SIGNAL_BYTES`](crate::planes::bounds::MAX_SIGNAL_BYTES), which is
    /// the only place it is read today.
    pub max_payload_bytes: u32,
    /// Canonical [`mechanics::demand::AuthorizationDemand`] bytes. Parsed at
    /// registration; not evaluated on any delivery path yet.
    pub demand: Vec<u8>,
}

/// What a World supplies at registration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorldRegistration {
    pub id: WorldId,
    pub implementation_version: WorldVersion,
    pub schemas: Vec<BodySchema>,
    pub limits: WorldLimits,
    /// Declared transient scopes. Empty is the ordinary case and costs nothing:
    /// the implementation descriptor omits an empty section entirely.
    pub scope_schemas: Vec<ScopeSchema>,
    /// Declared World signals, under the same rule.
    pub signal_schemas: Vec<SignalSchema>,
}

/// A decoded, authorized-by-Runtime application intent handed to a World. The
/// payload is the World's own bytes; Runtime does not interpret it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorldIntent {
    pub schema: replica::ids::SchemaId,
    pub schema_version: u32,
    pub payload: Vec<u8>,
}

/// A decoded application query handed to a World.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorldQuery {
    pub schema: replica::ids::SchemaId,
    pub schema_version: u32,
    pub payload: Vec<u8>,
}

/// A runtime-owned create declaration: the immutable schema binding for a Body
/// this transaction creates. An operation on a new Body with no declaration
/// defaults to the intent's schema; an operation on an existing Body uses its
/// recorded binding — a later write can never change a Body's schema
/// implicitly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BodyDeclaration {
    pub key: BodyKey,
    pub schema: replica::ids::SchemaId,
    pub schema_version: u32,
}

/// The result a World returns from `submit`: the staged Body operations, the
/// Observation scopes they touch, an opaque application effect payload, and the
/// **canonical non-empty authorization demand** the mutation requires. There
/// is no implicit `Write` fallback — Runtime evaluates this exact demand at the
/// pinned authority frontier and commits nothing if it is unsatisfied.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorldEffect {
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
    pub content_refs: Vec<(BodyKey, Vec<replica::ContentRef>)>,
    /// Body operations staged this transaction, each keyed to the Body it
    /// mutates.
    pub operations: Vec<(BodyKey, BodyOp)>,
    /// The Observation scopes affected, so Runtime can publish invalidations.
    pub scopes: Vec<BodyKey>,
    /// An opaque application-defined effect payload returned to the caller.
    pub effect: Vec<u8>,
    /// Schema declarations for Bodies this transaction creates (multi-schema
    /// transactions declare each non-intent-schema Body explicitly).
    pub declarations: Vec<BodyDeclaration>,
    /// The canonical [`mechanics::demand::AuthorizationDemand`] bytes this
    /// mutation requires (mandatory, non-empty).
    pub demand: Vec<u8>,
}

/// A canonical, versioned Projection a World returns from `query`, plus the
/// committed frontier it was derived at, and the read demand it required. Even
/// publicly visible product data uses an explicit read capability granted by
/// policy — there is no implicit-read fallback.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorldProjection {
    pub schema: replica::ids::SchemaId,
    pub schema_version: u32,
    pub bytes: Vec<u8>,
    pub frontier: replica::frontier::ReplicaFrontier,
    /// The canonical read demand this query required (mandatory, non-empty).
    /// Runtime evaluates it at the pinned frontier and returns no projection
    /// on denial.
    pub demand: Vec<u8>,
}

/// What a World may know about one content: enough to render it, and nothing
/// that would let it reach the bytes without asking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorldContentStatus {
    pub plaintext_len: u64,
    pub chunk_count: u32,
    /// How many chunks are here now. A fact about this moment on this machine,
    /// and never replicated.
    pub resident_chunks: u32,
}

impl WorldContentStatus {
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
    ) -> Result<replica::CollaborativeView, replica::ProjectionError>;
    /// Every interpreted Body of `world` bound to `schema` — the
    /// singleton-integrity seam (a World validating that exactly its one
    /// deterministic instance of a schema exists).
    fn bodies_with_schema(&self, world: &WorldId, schema: &SchemaId) -> Vec<BodyKey>;

    /// A Body's position in its collaborative history.
    ///
    /// Opaque and orderable, and never the convergence engine's own type. A
    /// World uses it to compare
    /// positions and to stamp anchors; it cannot use it to reach the engine.
    fn body_version(&self, key: &BodyKey) -> Option<replica::FabricVersion>;

    /// Take an anchor at a position inside a collaborative value.
    ///
    /// This is the seam plan 14's carets and range-attached comments consume,
    /// and it is exposed here rather than there because only the algebra that
    /// moves a position can mint one that survives being moved.
    fn anchor_in_body(
        &self,
        key: &BodyKey,
        path: &str,
        position: u64,
    ) -> Option<replica::FabricAnchor>;

    /// Resolve an anchor against a Body's current state.
    ///
    /// Total and read-only: a position whose material was deleted, or whose
    /// anchor predates what this replica retains, is `Drifted`. Never an error,
    /// never a mutation, and never a silently wrong index.
    fn resolve_anchor(
        &self,
        key: &BodyKey,
        anchor: &replica::FabricAnchor,
    ) -> replica::AnchorResolution;

    /// What one content is, and how much of it is here.
    ///
    /// A World sees size, geometry, and residency — never a path, never bytes,
    /// never a key. Reading the bytes is a host call with its own demand, and
    /// a `ContentRef` alone authorizes nothing.
    fn content_status(&self, content: &replica::ContentRef) -> Option<WorldContentStatus>;

    /// An opaque per-Body VERSION STAMP: two reads returning the same stamp
    /// for a key are guaranteed byte-equivalent Bodies, so a World may reuse
    /// state it derived from the earlier read. `None` (the default) promises
    /// nothing — the World must re-derive. Never a content hash contract;
    /// only equality is meaningful.
    fn body_stamp(&self, _key: &BodyKey) -> Option<Vec<u8>> {
        None
    }
}

/// The bounded capability handed to World callbacks. It exposes the principal
/// facts, authorized reads of the stable committed snapshot (during a query),
/// and **nothing** below the boundary: no CRDT internals, no mutable storage, no keys, no
/// network. A World stages Body operations by *returning* them in a
/// [`WorldEffect`]; Runtime — not the World — performs the durable commit.
pub struct WorldContext<'a> {
    principal: &'a PrincipalFacts,
    reads: Option<&'a dyn BodyReader>,
    /// The committed Manifest root this callback is pinned to (the parent of a
    /// submitted transaction; the snapshot root of a query).
    manifest_root: [u8; 32],
}

impl<'a> WorldContext<'a> {
    /// Construct a context over a principal's facts with no read access (submit
    /// authorizes and stages; it does not read the snapshot).
    pub fn new(principal: &'a PrincipalFacts) -> Self {
        Self {
            principal,
            reads: None,
            manifest_root: [0u8; 32],
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
            manifest_root,
        }
    }

    /// The committed Manifest root this callback is pinned to.
    pub fn manifest_root(&self) -> [u8; 32] {
        self.manifest_root
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

    /// The derived facts for the docked principal. A World authorizes against
    /// these; it cannot replace them.
    pub fn principal(&self) -> &PrincipalFacts {
        self.principal
    }

    /// Read an atomic Body from the stable committed snapshot. Returns `None`
    /// if the Body is absent or this context has no read access.
    pub fn read_body(&self, key: &BodyKey) -> Option<Vec<u8>> {
        self.reads.and_then(|r| r.read_body(key))
    }

    /// A Body's causal position, for comparison and for stamping anchors.
    pub fn body_version(&self, key: &BodyKey) -> Option<replica::FabricVersion> {
        self.reads.and_then(|r| r.body_version(key))
    }

    /// Take an anchor at a position inside a collaborative value.
    pub fn anchor(
        &self,
        key: &BodyKey,
        path: &str,
        position: u64,
    ) -> Option<replica::FabricAnchor> {
        self.reads
            .and_then(|r| r.anchor_in_body(key, path, position))
    }

    /// Resolve an anchor. Total: `Drifted` rather than an error or a guess.
    pub fn resolve_anchor(
        &self,
        key: &BodyKey,
        anchor: &replica::FabricAnchor,
    ) -> replica::AnchorResolution {
        match self.reads {
            Some(reads) => reads.resolve_anchor(key, anchor),
            None => replica::AnchorResolution::Drifted,
        }
    }

    /// What one content is, and how much of it is here.
    pub fn content_status(&self, content: &replica::ContentRef) -> Option<WorldContentStatus> {
        self.reads.and_then(|r| r.content_status(content))
    }

    /// Read a collaborative Body's view from the stable committed snapshot.
    pub fn read_collaborative(
        &self,
        key: &BodyKey,
    ) -> Result<replica::CollaborativeView, replica::ProjectionError> {
        match self.reads {
            Some(reads) => reads.read_collaborative_body(key),
            None => Err(replica::ProjectionError::NotCollaborative),
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
    /// This World's stable namespaced identity.
    fn id(&self) -> WorldId;

    /// The Body schemas this World supports.
    fn schemas(&self) -> &[BodySchema];

    /// The transient scopes this World declares.
    ///
    /// Defaulted to none rather than made a required method: a World with
    /// nothing to declare needs no edit, and because the descriptor omits an
    /// empty section it also keeps the implementation id it already has.
    /// `build()` refuses a World whose answer here disagrees with its
    /// registration, so the default is a declaration and not a hole.
    fn scope_schemas(&self) -> &[ScopeSchema] {
        &[]
    }

    /// The World signals this World declares, under the same rule.
    fn signal_schemas(&self) -> &[SignalSchema] {
        &[]
    }

    /// Decode, authorize, and stage Body operations for an application intent.
    fn submit(
        &self,
        ctx: &mut WorldContext<'_>,
        intent: WorldIntent,
    ) -> Result<WorldEffect, crate::error::WorldError>;

    /// Decode a query and derive a Projection from the stable snapshot.
    fn query(
        &self,
        ctx: &WorldContext<'_>,
        query: WorldQuery,
    ) -> Result<WorldProjection, crate::error::WorldError>;
}
